//! `edda why` orchestration — §8.6.
//!
//! Transitive provenance trace: who imports `<name>` directly, which deps
//! pull it transitively, and what capabilities/effects it brings into the
//! consumer's ceiling.

use std::path::Path;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig, Severity};
use edda_manifest::lockfile;
use edda_span::Span;

use crate::command::{MimirOptions, WhyCommand};
use crate::outcome::Outcome;

/// Execute `edda why <name>`: trace transitive provenance for a dependency.
pub fn run_why(cmd: &WhyCommand, options: &MimirOptions, diags: &mut Diagnostics) -> Outcome {
    let manifest_path = &options.manifest_path;
    let lockfile_path = manifest_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("package.lock.toml");

    // -----------------------------------------------------------------------
    // Load the lockfile.
    // -----------------------------------------------------------------------
    let sm = edda_span::SourceMap::new();
    let lint_cfg = LintConfig::new();
    let lockfile = match lockfile::load(&lockfile_path, &sm, diags, &lint_cfg) {
        Ok(lf) => lf,
        Err(_) => {
            push_error(
                diags,
                format!(
                    "edda why: no lockfile found at `{}` — run `edda add` first",
                    lockfile_path.display()
                ),
            );
            return Outcome::build_error(diags);
        }
    };

    // -----------------------------------------------------------------------
    // Load the manifest to find direct dependencies.
    // -----------------------------------------------------------------------
    let manifest = {
        let sm = edda_span::SourceMap::new();
        let lint_cfg = LintConfig::new();
        edda_manifest::load(manifest_path, &sm, diags, &lint_cfg)
    };

    let direct_deps: std::collections::HashSet<String> = match manifest {
        Some(m) => m
            .dependencies
            .iter()
            .map(|d| d.name.to_string())
            .collect(),
        None => std::collections::HashSet::new(),
    };

    // -----------------------------------------------------------------------
    // Find the target in the lockfile.
    // -----------------------------------------------------------------------
    let target_entry = lockfile.runes.iter().find(|e| e.name == cmd.name);

    match target_entry {
        None => {
            push_error(
                diags,
                format!(
                    "edda why: `{}` is not in the lockfile; it may not be a dependency of this package",
                    cmd.name
                ),
            );
            return Outcome::build_error(diags);
        }
        Some(entry) => {
            let is_direct = direct_deps.contains(&cmd.name);

            // Emit provenance information.
            let relationship = if is_direct { "direct" } else { "transitive" };
            diags.push(Diagnostic::new(
                DiagnosticClass::ParseError,
                Severity::Info,
                Span::DUMMY,
                format!(
                    "edda why: `{}@{}` is a {} dependency",
                    entry.name, entry.version, relationship
                ),
            ));

            // Trace which other deps bring it in transitively.
            let transitive_via: Vec<&str> = lockfile
                .runes
                .iter()
                .filter(|e| e.name != cmd.name && e.deps.iter().any(|d| d == &cmd.name))
                .map(|e| e.name.as_str())
                .collect();

            if !transitive_via.is_empty() {
                diags.push(Diagnostic::new(
                    DiagnosticClass::ParseError,
                    Severity::Info,
                    Span::DUMMY,
                    format!(
                        "  pulled in transitively by: {}",
                        transitive_via.join(", ")
                    ),
                ));
            }

            // Report hashes / capabilities summary.
            diags.push(Diagnostic::new(
                DiagnosticClass::ParseError,
                Severity::Info,
                Span::DUMMY,
                format!(
                    "  rune_hash:    {}",
                    &entry.rune_hash[..32.min(entry.rune_hash.len())]
                ),
            ));
            diags.push(Diagnostic::new(
                DiagnosticClass::ParseError,
                Severity::Info,
                Span::DUMMY,
                format!(
                    "  surface_hash: {}",
                    &entry.surface_hash[..32.min(entry.surface_hash.len())]
                ),
            ));
            diags.push(Diagnostic::new(
                DiagnosticClass::ParseError,
                Severity::Info,
                Span::DUMMY,
                format!(
                    "  effect_hash:  {}",
                    &entry.effect_hash[..32.min(entry.effect_hash.len())]
                ),
            ));
            diags.push(Diagnostic::new(
                DiagnosticClass::ParseError,
                Severity::Info,
                Span::DUMMY,
                format!(
                    "  publisher:    {}",
                    entry.publisher_key
                ),
            ));
        }
    }

    Outcome::success(diags)
}

fn push_error(diags: &mut Diagnostics, message: String) {
    diags.push(Diagnostic::new(
        DiagnosticClass::ParseError,
        Severity::Error,
        Span::DUMMY,
        message,
    ));
}
