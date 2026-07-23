//! `edda audit` orchestration — §8.3.
//!
//! Re-verifies every `[[rune]]` entry in `package.lock.toml` against the
//! project cache. No network access — entries missing from cache are reported
//! as "unverified" rather than fetched.

use std::path::Path;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig, Severity};
use edda_manifest::lockfile;
use edda_span::Span;

use crate::command::{AuditCommand, MimirOptions};
use crate::outcome::Outcome;

/// Execute `edda audit`: re-verify all lockfile entries against the cache.
pub fn run_audit(cmd: &AuditCommand, options: &MimirOptions, diags: &mut Diagnostics) -> Outcome {
    let manifest_path = &options.manifest_path;
    let lockfile_path = manifest_path
        .parent()
        .unwrap_or(Path::new("."))
        .join("package.lock.toml");

    if !lockfile_path.exists() {
        if !cmd.quiet {
            push_error(
                diags,
                format!(
                    "edda audit: no lockfile found at `{}` — run `edda add` first",
                    lockfile_path.display()
                ),
            );
        }
        return Outcome::build_error(diags);
    }

    // Load the lockfile — `lockfile::load` already verifies the trailer and
    // emits `LockfileTampered` into `diags` if it mismatches.
    let lockfile = {
        let sm = edda_span::SourceMap::new();
        let lint_cfg = LintConfig::new();
        match lockfile::load(&lockfile_path, &sm, diags, &lint_cfg) {
            Ok(lf) => lf,
            Err(_) => return Outcome::build_error(diags),
        }
    };

    if lockfile.runes.is_empty() {
        // Nothing to audit.
        return Outcome::success(diags);
    }

    // For each entry, report whether it is present in the project cache.
    // The project cache is `.edda/cache/` adjacent to the manifest.
    let cache_root = manifest_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(".edda")
        .join("cache");

    let mut unverified = Vec::new();

    for entry in &lockfile.runes {
        // Derive the expected archive filename: `<name>-<version>.rune`.
        let archive_name = format!("{}-{}.rune", entry.name, entry.version);
        let cache_path = cache_root.join("xlibs").join(&archive_name);

        if cache_path.exists() {
            // Re-hash the cached archive and compare to the locked rune_hash.
            match verify_cached_archive(&cache_path, &entry.rune_hash) {
                Ok(()) => {
                    if !cmd.quiet {
                        // A verified entry — no diagnostic needed.
                    }
                }
                Err(msg) => {
                    push_error(
                        diags,
                        format!(
                            "edda audit: hash mismatch for `{}@{}`: {}",
                            entry.name, entry.version, msg
                        ),
                    );
                }
            }
        } else {
            unverified.push(format!("{}@{}", entry.name, entry.version));
        }
    }

    if !unverified.is_empty() {
        if !cmd.quiet {
            for pkg in &unverified {
                diags.push(Diagnostic::new(
                    DiagnosticClass::ParseError,
                    Severity::Warn,
                    Span::DUMMY,
                    format!("edda audit: `{}` not in cache — unverified", pkg),
                ));
            }
        }
        // Unverified entries are warnings, not errors — audit still succeeds.
    }

    Outcome::success(diags)
}

/// Verify a cached `.rune` archive against its expected `blake3:<hex>` hash.
fn verify_cached_archive(cache_path: &Path, expected_hash: &str) -> Result<(), String> {
    let bytes = std::fs::read(cache_path).map_err(|e| format!("read error: {}", e))?;
    let computed = edda_mimir_hash::compute_rune_hash(&bytes);
    if computed != expected_hash {
        return Err(format!(
            "expected {}, computed {}",
            expected_hash, computed
        ));
    }
    Ok(())
}

fn push_error(diags: &mut Diagnostics, message: String) {
    diags.push(Diagnostic::new(
        DiagnosticClass::ParseError,
        Severity::Error,
        Span::DUMMY,
        message,
    ));
}
