//! Manifest / lockfile / publisher-key I/O helpers for `edda add`.
//!
//! Registry-source resolution, publisher-key parsing, and the
//! string-append `package.toml` editor that pins the resolved
//! dependency.

use std::path::Path;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig, Severity};
use edda_manifest::{Dependency, DependencySource, PackageManifest};
use edda_mimir_crypto::PublicKey;
use edda_mimir_registry::RegistrySource;
use edda_span::Span;

/// Load the manifest at `path`; push a diagnostic and return `None` on failure.
pub(super) fn load_manifest(path: &Path, diags: &mut Diagnostics) -> Option<PackageManifest> {
    let sm = edda_span::SourceMap::new();
    let lint_cfg = LintConfig::new();
    match edda_manifest::load(path, &sm, diags, &lint_cfg) {
        Some(m) => Some(m),
        None => {
            push_error(
                diags,
                format!(
                    "edda add: failed to load `package.toml` at `{}`",
                    path.display()
                ),
            );
            None
        }
    }
}

/// Derive the registry source to fetch `dep_name` from.
///
/// `DependencySource::Registry` is a unit variant — `package.toml`'s
/// `[[dependencies]]` schema has nowhere to store a per-dep registry URL.
/// The lockfile next to `manifest_path` is the only place a resolved
/// dependency's registry source is actually recorded, so reuse the
/// existing entry for `dep_name` if the lockfile has one, otherwise the
/// first entry's source (the project's already-established mirror),
/// otherwise the default registry.
pub(crate) fn pick_registry_source(
    manifest_path: &Path,
    dep_name: &str,
    diags: &mut Diagnostics,
) -> RegistrySource {
    let lockfile_path = manifest_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("package.lock.toml");
    let sm = edda_span::SourceMap::new();
    let lint_cfg = LintConfig::new();
    let source_str = edda_manifest::lockfile::load(&lockfile_path, &sm, diags, &lint_cfg)
        .ok()
        .and_then(|lf| {
            lf.runes
                .iter()
                .find(|e| e.name == dep_name)
                .or_else(|| lf.runes.first())
                .map(|e| e.source.clone())
        });

    source_str
        .and_then(|s| RegistrySource::parse(&s).ok())
        .unwrap_or_else(|| RegistrySource::Https("mimir.edda.dev".to_owned()))
}

/// Convert a `RegistrySource` back to a canonical string for the manifest/lockfile.
pub(super) fn registry_source_to_string(src: &RegistrySource) -> String {
    match src {
        RegistrySource::Https(s) => s.clone(),
        RegistrySource::File(p) => format!("file://{}", p.display()),
    }
}

/// Parse a publisher key from the text form in `publisher.key`.
pub(super) fn parse_publisher_key(text: &str) -> Result<PublicKey, String> {
    // The publisher.key file format: one line `ed25519:<pem>` or bare PEM.
    // The archive's publisher.key uses PEM form per spec §3.2.
    let pem_start = text.find("-----BEGIN");
    let pem_text = pem_start.map(|i| &text[i..]).unwrap_or(text);
    PublicKey::from_pem(pem_text).map_err(|e| e.to_string())
}

/// Write a new `[[dependencies]]` entry into the manifest file.
///
/// Appends a TOML block to the existing file. A follow-up wave will use a
/// proper TOML editor once `toml_edit` is a workspace dep; for slice H,
/// string append is correct because the resolver enforces no-duplicate names.
pub(super) fn write_dep_to_manifest(
    path: &Path,
    manifest: &PackageManifest,
    dep: Dependency,
) -> Result<(), std::io::Error> {
    // Check if this dep already exists; if so update it by removing the old block.
    // For v0.1 simplicity, we append a new `[[dependencies]]` entry. The manifest
    // parser deduplication rules mean a repeated name will be an error on next load,
    // so we first check and warn; a future wave adds in-place editing.
    let existing_names: std::collections::HashSet<&str> =
        manifest.dependencies.iter().map(|d| d.name.as_ref()).collect();
    let already_present = existing_names.contains(dep.name.as_ref());

    let content = std::fs::read_to_string(path)?;
    let mut new_content = content.clone();

    if already_present {
        // Remove the existing block for this dep by finding its start.
        new_content = remove_dep_block(&new_content, dep.name.as_ref());
    }

    // Append the new entry.
    let toml_block = format_dep_toml(&dep);
    if !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content.push_str(&toml_block);
    std::fs::write(path, new_content)
}

/// Remove a `[[dependencies]]` block for `dep_name` from the TOML text.
fn remove_dep_block(content: &str, dep_name: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut skip = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed == "[[dependencies]]" {
            // Look ahead is not available here, so we use a state machine:
            // once we see [[dependencies]] we check the next `name =` line.
            skip = false; // reset; will be re-set if name matches
        }
        if skip {
            // Skip lines inside the block we're removing.
            if trimmed.starts_with("[[") && trimmed != "[[dependencies]]" {
                // Start of a different table — stop skipping.
                skip = false;
                out.push_str(line);
                out.push('\n');
            }
            // Also stop skipping at the next [[dependencies]] block.
            else if trimmed == "[[dependencies]]" {
                skip = false;
                out.push_str(line);
                out.push('\n');
            }
            continue;
        }
        // Detect whether to start skipping.
        if trimmed == "[[dependencies]]" {
            // Peek the next relevant line.
            // We collect remaining content to check the name.
            let rest = content
                .find(line)
                .map(|i| &content[i..])
                .unwrap_or("")
                .lines()
                .skip(1);
            let name_matches = rest
                .take_while(|l| !l.trim().starts_with("[["))
                .any(|l| {
                    let t = l.trim();
                    t.starts_with("name") && t.contains(dep_name)
                });
            if name_matches {
                skip = true;
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Render a `Dependency` as a `[[dependencies]]` TOML block.
fn format_dep_toml(dep: &Dependency) -> String {
    let src = match &dep.source {
        DependencySource::Registry => "registry".to_owned(),
        DependencySource::Path(p) => format!("path:{}", p.as_ref()),
        DependencySource::Git(url) => url.as_ref().to_owned(),
    };
    let mut s = String::new();
    s.push_str("[[dependencies]]\n");
    s.push_str(&format!("name = \"{}\"\n", dep.name));
    s.push_str(&format!("version = \"{}\"\n", dep.version_req));
    s.push_str(&format!("source = \"{}\"\n", src));
    if let Some(sh) = &dep.surface_hash {
        s.push_str(&format!("surface_hash = \"{}\"\n", sh));
    }
    if !dep.max_effects.is_empty() {
        let effects: Vec<String> = dep.max_effects.iter().map(|e| format!("\"{}\"", e)).collect();
        s.push_str(&format!("max_effects = [{}]\n", effects.join(", ")));
    }
    if dep.accept_unstable {
        s.push_str("accept_unstable = true\n");
    }
    if let Some(pin) = &dep.publisher {
        s.push_str(&format!(
            "publisher_key = \"{}\"\n",
            pin.key_fingerprint
        ));
    }
    s
}

/// Push an error diagnostic at `Span::DUMMY`.
pub(super) fn push_error(diags: &mut Diagnostics, message: String) {
    diags.push(Diagnostic::new(
        DiagnosticClass::ParseError,
        Severity::Error,
        Span::DUMMY,
        message,
    ));
}
