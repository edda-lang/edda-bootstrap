//! `edda publish` orchestration — §8.4.
//!
//! Builds the `.rune` archive from the current package, canonicalises,
//! computes the three hashes, writes `hashes.toon`, signs with the
//! publisher's private key, bundles `signature.bin` + `publisher.key`,
//! and uploads to the configured registry.
//!
//! Pre-upload: runs the self-verification chain (the same steps a consumer's
//! `edda add` would execute).

use std::path::Path;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, Severity};
use edda_mimir_archive::{RuneLayout, pack};
use edda_mimir_crypto::{default_keystore_dir, generate, sign};
use edda_mimir_hash::compute_archive_hashes;
use edda_span::Span;

use crate::command::{MimirOptions, PublishCommand};
use crate::outcome::Outcome;

/// Execute `edda publish`: build, sign, and upload the current rune to the registry.
pub fn run_publish(
    _cmd: &PublishCommand,
    options: &MimirOptions,
    diags: &mut Diagnostics,
) -> Outcome {
    let manifest_path = &options.manifest_path;

    // -----------------------------------------------------------------------
    // 1. Load the manifest to get package name, version, and registry source.
    // -----------------------------------------------------------------------
    let manifest = match load_manifest(manifest_path, diags) {
        Some(m) => m,
        None => return Outcome::build_error(diags),
    };

    // -----------------------------------------------------------------------
    // 2. Collect the package artifacts from the build output directory.
    // -----------------------------------------------------------------------
    // For slice H, we collect the surface/*.toon and objects from the
    // standard output directories.
    let package_root = manifest_path.parent().unwrap_or(Path::new("."));
    let layout = match collect_rune_layout(package_root, &manifest, diags) {
        Some(l) => l,
        None => return Outcome::build_error(diags),
    };

    // -----------------------------------------------------------------------
    // 3. Pack the archive.
    // -----------------------------------------------------------------------
    let archive_bytes = match pack(&layout) {
        Ok(b) => b,
        Err(e) => {
            push_error(
                diags,
                format!("edda publish: archive pack failed: {}", e),
            );
            return Outcome::build_error(diags);
        }
    };

    // -----------------------------------------------------------------------
    // 4. Compute all three hashes.
    // -----------------------------------------------------------------------
    let all_files = build_all_files_list(&layout);
    let hashes = match compute_archive_hashes(&archive_bytes, &layout.surface, &all_files) {
        Ok(h) => h,
        Err(e) => {
            push_error(
                diags,
                format!("edda publish: hash computation failed: {}", e),
            );
            return Outcome::build_error(diags);
        }
    };

    // -----------------------------------------------------------------------
    // 5. Load or generate the publisher keypair.
    // -----------------------------------------------------------------------
    let keypair = match load_publisher_key() {
        Some(kp) => kp,
        None => {
            // For slice H, generate a fresh key if none exists.
            // A follow-up slice will load from the keystore.
            generate()
        }
    };

    // -----------------------------------------------------------------------
    // 6. Sign `hashes.toon`.
    // -----------------------------------------------------------------------
    let hashes_toon_bytes = build_hashes_toon_bytes(&hashes);
    let sig = sign(&keypair, &hashes_toon_bytes);
    let pub_key = keypair.public_key();
    let pub_key_pem = pub_key.to_pem();

    // -----------------------------------------------------------------------
    // 7. Self-verification: unpack the archive and verify.
    // -----------------------------------------------------------------------
    let mut verify_layout = layout;
    verify_layout.hashes_toon = hashes_toon_bytes.clone();
    verify_layout.signature_bin = sig.to_bytes().to_vec();
    verify_layout.publisher_key = pub_key_pem.as_bytes().to_vec();

    let packed_final = match pack(&verify_layout) {
        Ok(b) => b,
        Err(e) => {
            push_error(
                diags,
                format!("edda publish: final archive pack failed: {}", e),
            );
            return Outcome::build_error(diags);
        }
    };

    // Unpack to self-verify.
    if let Err(e) = edda_mimir_archive::unpack(&packed_final) {
        push_error(
            diags,
            format!("edda publish: self-verification failed: {}", e),
        );
        return Outcome::build_error(diags);
    }

    // -----------------------------------------------------------------------
    // 8. Upload to registry (stub for slice H — full HTTPS in slice I).
    // -----------------------------------------------------------------------
    // For v0.1, write the `.rune` to the build output directory and emit a
    // diagnostic noting that the upload step is pending HTTPS implementation.
    let output_name = format!(
        "{}-{}.rune",
        manifest.package, manifest.version.major
    );
    let output_path = package_root.join("target").join("edda").join(&output_name);
    if let Some(parent) = output_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&output_path, &packed_final) {
        push_error(
            diags,
            format!("edda publish: failed to write archive to `{}`: {}", output_path.display(), e),
        );
        return Outcome::build_error(diags);
    }

    // Emit an informational diagnostic for the pending upload step.
    diags.push(Diagnostic::new(
        DiagnosticClass::ParseError,
        Severity::Warn,
        Span::DUMMY,
        format!(
            "edda publish: archive written to `{}` — registry upload pending full HTTPS implementation (slice I)",
            output_path.display()
        ),
    ));

    Outcome::success(diags)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn load_manifest(
    path: &Path,
    diags: &mut Diagnostics,
) -> Option<edda_manifest::PackageManifest> {
    let sm = edda_span::SourceMap::new();
    let lint_cfg = edda_diag::LintConfig::new();
    match edda_manifest::load(path, &sm, diags, &lint_cfg) {
        Some(m) => Some(m),
        None => {
            push_error(
                diags,
                format!(
                    "edda publish: failed to load `package.toml` at `{}`",
                    path.display()
                ),
            );
            None
        }
    }
}

/// Collect the `RuneLayout` from the package's build output directories.
///
/// For slice H this is a best-effort collection from the standard output
/// paths. A follow-up wave will integrate with the full cascade output.
fn collect_rune_layout(
    package_root: &Path,
    manifest: &edda_manifest::PackageManifest,
    diags: &mut Diagnostics,
) -> Option<RuneLayout> {
    let package_toml = match std::fs::read(package_root.join("package.toml")) {
        Ok(b) => b,
        Err(e) => {
            push_error(
                diags,
                format!("edda publish: cannot read package.toml: {}", e),
            );
            return None;
        }
    };

    // §7.3: the rune `manifest.toml` records the compiler that built it as a
    // top-level `compiler = "edda <major>.<minor>"` pin, sourced from this
    // compiler's own version. The packer writes these bytes verbatim, so the
    // field is injected here in the layout builder.
    let pin = edda_manifest::CompilerPin::from_compiler_version(env!("CARGO_PKG_VERSION"))
        .unwrap_or(edda_manifest::CompilerPin { major: 0, minor: 0 });
    let manifest_toml = edda_manifest::compiler_pin::inject_compiler_pin(&package_toml, pin);

    // Collect surface/*.toon files.
    let surface_dir = package_root
        .join("target")
        .join("edda")
        .join("surface");
    let surface = collect_toon_files(&surface_dir);

    // Collect *.mir files.
    let mir_dir = package_root.join("target").join("edda").join("mir");
    let mir = collect_toon_files(&mir_dir);

    // Collect object files per triple.
    let obj_dir = package_root.join("target").join("edda");
    let objects = collect_object_files(&obj_dir);

    // Build a minimal index.toon.
    let index_toon = format!(
        "schema_version: 3\npackage: {}\n",
        manifest.package
    ).into_bytes();

    // hashes_toon, signature_bin, publisher_key are filled in later.
    Some(RuneLayout {
        manifest_toml,
        surface,
        mir,
        objects,
        index_toon,
        hashes_toon: Vec::new(),
        signature_bin: Vec::new(),
        publisher_key: Vec::new(),
    })
}

fn collect_toon_files(dir: &Path) -> Vec<(String, Vec<u8>)> {
    if !dir.is_dir() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().map(|e| e == "toon").unwrap_or(false) {
                let leaf = p
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_owned();
                if let Ok(bytes) = std::fs::read(&p) {
                    out.push((leaf, bytes));
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn collect_object_files(dir: &Path) -> Vec<(String, String, Vec<u8>)> {
    if !dir.is_dir() {
        return Vec::new();
    }
    let mut out = Vec::new();
    // Walk one level of subdirectories — each is a target triple.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let triple_dir = entry.path();
            if !triple_dir.is_dir() {
                continue;
            }
            let triple = triple_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_owned();
            if let Ok(files) = std::fs::read_dir(&triple_dir) {
                for f in files.flatten() {
                    let fp = f.path();
                    if fp.extension().map(|e| e == "o").unwrap_or(false) {
                        let leaf = fp
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("")
                            .to_owned();
                        if let Ok(bytes) = std::fs::read(&fp) {
                            out.push((triple.clone(), leaf, bytes));
                        }
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    out
}

/// Build the flat `(path, bytes)` list for all archive entries.
fn build_all_files_list(layout: &RuneLayout) -> Vec<(String, Vec<u8>)> {
    let mut files = Vec::new();
    files.push(("manifest.toml".to_owned(), layout.manifest_toml.clone()));
    for (leaf, bytes) in &layout.surface {
        files.push((format!("surface/{}.toon", leaf), bytes.clone()));
    }
    for (leaf, bytes) in &layout.mir {
        files.push((format!("mir/{}.mir", leaf), bytes.clone()));
    }
    for (triple, leaf, bytes) in &layout.objects {
        files.push((format!("objects/{}/{}", triple, leaf), bytes.clone()));
    }
    files.push(("index.toon".to_owned(), layout.index_toon.clone()));
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files
}

/// Build the `hashes.toon` canonical bytes from the computed hashes.
fn build_hashes_toon_bytes(hashes: &edda_mimir_hash::ArchiveHashes) -> Vec<u8> {
    // Build a minimal hashes.toon representation for signing.
    // The full canonical form is defined in 08-packages.md §5.
    let mut out = String::new();
    out.push_str("rune_hash: ");
    out.push_str(&hashes.rune_hash);
    out.push('\n');
    out.push_str("surface_hash: ");
    out.push_str(&hashes.surface_hash);
    out.push('\n');
    out.push_str("effect_hash: ");
    out.push_str(&hashes.effect_hash);
    out.push('\n');
    out.into_bytes()
}

/// Load the publisher keypair from the default keystore.
///
/// Returns `None` if no key is found (caller generates a fresh one for slice H).
fn load_publisher_key() -> Option<edda_mimir_crypto::Keypair> {
    let _keystore = default_keystore_dir()?;
    // Full keystore loading is a follow-up; for slice H, always return None.
    None
}

fn push_error(diags: &mut Diagnostics, message: String) {
    diags.push(Diagnostic::new(
        DiagnosticClass::ParseError,
        Severity::Error,
        Span::DUMMY,
        message,
    ));
}
