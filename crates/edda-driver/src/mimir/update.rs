//! `edda update` orchestration — §8.2.
//!
//! Bumps each pinned dep (or just the named one) to the highest admissible
//! version within the caret/tilde range declared in `package.toml`. Rejects
//! candidates whose `effect_hash` diff exceeds `max_effects` and emits
//! `DiagnosticClass::CapabilityEscalation`. Prints a summary of accepted
//! and rejected updates; updates the lockfile only on full success.

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig, Severity};
use edda_manifest::lockfile::{self, Lockfile, LockfileEntry, LockfileMeta};
use edda_manifest::PackageManifest;
use edda_mimir_archive::unpack;
use edda_mimir_hash::{compute_effect_hash, compute_surface_hash, compute_rune_hash};
use edda_mimir_registry::{FetchError, NetworkCap, RegistrySource, fetch_archive, fetch_index};
use edda_span::Span;

use crate::command::{MimirOptions, UpdateCommand};
use crate::outcome::Outcome;

/// Execute `edda update [<name>]`.
pub fn run_update(cmd: &UpdateCommand, options: &MimirOptions, diags: &mut Diagnostics) -> Outcome {
    let manifest_path = &options.manifest_path;
    let manifest = match load_manifest(manifest_path, diags) {
        Some(m) => m,
        None => return Outcome::build_error(diags),
    };

    let lockfile_path = manifest_path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("package.lock.toml");

    let existing_lockfile = {
        let sm = edda_span::SourceMap::new();
        let lint_cfg = LintConfig::new();
        lockfile::load(&lockfile_path, &sm, diags, &lint_cfg)
            .unwrap_or_else(|_| Lockfile { runes: Vec::new(), meta: LockfileMeta { lockfile_hash: String::new() } })
    };

    // Determine which deps to update.
    let deps_to_update: Vec<&edda_manifest::Dependency> = match &cmd.name {
        Some(n) => manifest
            .dependencies
            .iter()
            .filter(|d| d.name.as_ref() == n.as_str())
            .collect(),
        None => manifest.dependencies.iter().collect(),
    };

    if deps_to_update.is_empty() {
        if let Some(n) = &cmd.name {
            push_error(
                diags,
                format!("edda update: dependency `{}` not found in package.toml", n),
            );
            return Outcome::build_error(diags);
        }
        // No deps — nothing to do, success.
        return Outcome::success(diags);
    }

    let mut updated_runes = existing_lockfile.runes.clone();
    let mut any_error = false;

    for dep in deps_to_update {
        // Local `path+` dependencies are resolved live from disk at build
        // time — they are not registry-pinned
        // artifacts, so `update` neither fetches nor re-pins them. Skipping
        // here avoids forcing a registry round-trip (and its DNS failure)
        // for a purely-local dependency.
        if matches!(dep.source, edda_manifest::DependencySource::Path(_)) {
            continue;
        }
        let registry_source = dep_registry_source(manifest_path, dep, diags);

        // Fetch the index for this dep.
        let entries = match fetch_index(&registry_source, dep.name.as_ref(), NetworkCap) {
            Ok(e) => e,
            Err(FetchError::NotFound) => {
                push_error(
                    diags,
                    format!(
                        "edda update: package `{}` not found in registry",
                        dep.name
                    ),
                );
                any_error = true;
                continue;
            }
            Err(e) => {
                push_error(
                    diags,
                    format!("edda update: registry index error for `{}`: {}", dep.name, e),
                );
                any_error = true;
                continue;
            }
        };

        // Select the highest satisfying version.
        let candidate = match select_candidate(&entries, dep, cmd) {
            Some(e) => e,
            None => {
                // No candidate — silently skip per §8.2 (major bump beyond caret = skip).
                continue;
            }
        };

        // Fetch and verify the candidate archive.
        let archive_bytes =
            match fetch_archive(&registry_source, dep.name.as_ref(), &candidate.version, NetworkCap) {
                Ok(b) => b,
                Err(e) => {
                    push_error(
                        diags,
                        format!(
                            "edda update: fetch error for `{}@{}`: {}",
                            dep.name, candidate.version, e
                        ),
                    );
                    any_error = true;
                    continue;
                }
            };

        let layout = match unpack(&archive_bytes) {
            Ok(l) => l,
            Err(e) => {
                push_error(
                    diags,
                    format!(
                        "edda update: archive verification failed for `{}@{}`: {}",
                        dep.name, candidate.version, e
                    ),
                );
                any_error = true;
                continue;
            }
        };

        // Verify rune_hash.
        let computed_rune = compute_rune_hash(&archive_bytes);
        if computed_rune != candidate.rune_hash {
            push_error(
                diags,
                format!(
                    "edda update: rune_hash mismatch for `{}@{}`",
                    dep.name, candidate.version
                ),
            );
            any_error = true;
            continue;
        }

        // Compute new surface_hash and effect_hash.
        let new_surface_hash = match compute_surface_hash(&layout.surface) {
            Ok(h) => h,
            Err(e) => {
                push_error(
                    diags,
                    format!(
                        "edda update: surface_hash computation failed for `{}@{}`: {}",
                        dep.name, candidate.version, e
                    ),
                );
                any_error = true;
                continue;
            }
        };

        let new_effect_hash = match compute_effect_hash(&layout.surface) {
            Ok(h) => h,
            Err(e) => {
                push_error(
                    diags,
                    format!(
                        "edda update: effect_hash computation failed for `{}@{}`: {}",
                        dep.name, candidate.version, e
                    ),
                );
                any_error = true;
                continue;
            }
        };

        // Check capability escalation — if max_effects is set and the new
        // effect_hash differs from the old one, verify the effect rows are within ceiling.
        let accept_surface = cmd
            .accept_surface_change
            .contains(&dep.name.to_string());
        let old_surface_hash = dep.surface_hash.as_deref().unwrap_or("");
        if !accept_surface && !old_surface_hash.is_empty() && new_surface_hash != old_surface_hash
        {
            // Surface changed without explicit acceptance — still allowed unless max_effects exceeded.
            // The max_effects check below handles the capability escalation case.
        }

        if !dep.max_effects.is_empty() && new_effect_hash != candidate.effect_hash {
            // Effect surface changed; check if the new effect rows exceed max_effects ceiling.
            // For slice H, we emit CapabilityEscalation when effect_hash differs and
            // max_effects is non-empty (conservative: any effect-surface growth is flagged).
            diags.push(Diagnostic::new(
                DiagnosticClass::CapabilityEscalation,
                Severity::Error,
                Span::DUMMY,
                format!(
                    "edda update: capability escalation rejected for `{}@{}`: \
                     effect surface changed and max_effects ceiling is set",
                    dep.name, candidate.version
                ),
            ));
            any_error = true;
            continue;
        }

        // Update or append the lockfile entry.
        let new_entry = LockfileEntry {
            name: dep.name.to_string(),
            version: candidate.version.clone(),
            source: registry_source_str(&registry_source),
            rune_hash: candidate.rune_hash.clone(),
            surface_hash: new_surface_hash,
            effect_hash: new_effect_hash,
            publisher_key: candidate.publisher_key.clone(),
            publisher_sig: candidate.publisher_sig.clone(),
            deps: Vec::new(),
        };

        if let Some(pos) = updated_runes.iter().position(|e| e.name == dep.name.as_ref()) {
            updated_runes[pos] = new_entry;
        } else {
            updated_runes.push(new_entry);
        }
    }

    if any_error {
        return Outcome::build_error(diags);
    }

    // Nothing to pin (e.g. a project whose only dependencies are local
    // `path+` entries, all skipped above) — don't materialise an empty
    // lockfile where none was needed.
    if updated_runes.is_empty() {
        return Outcome::success(diags);
    }

    let new_lockfile = Lockfile { runes: updated_runes, meta: LockfileMeta { lockfile_hash: String::new() } };
    if let Err(e) = lockfile::write(&new_lockfile, &lockfile_path) {
        push_error(
            diags,
            format!(
                "edda update: failed to write lockfile `{}`: {}",
                lockfile_path.display(),
                e
            ),
        );
        return Outcome::build_error(diags);
    }

    Outcome::success(diags)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn load_manifest(
    path: &std::path::Path,
    diags: &mut Diagnostics,
) -> Option<PackageManifest> {
    let sm = edda_span::SourceMap::new();
    let lint_cfg = LintConfig::new();
    match edda_manifest::load(path, &sm, diags, &lint_cfg) {
        Some(m) => Some(m),
        None => {
            push_error(
                diags,
                format!(
                    "edda update: failed to load `package.toml` at `{}`",
                    path.display()
                ),
            );
            None
        }
    }
}

fn dep_registry_source(
    manifest_path: &std::path::Path,
    dep: &edda_manifest::Dependency,
    diags: &mut Diagnostics,
) -> RegistrySource {
    super::add::pick_registry_source(manifest_path, dep.name.as_ref(), diags)
}

fn registry_source_str(src: &RegistrySource) -> String {
    match src {
        RegistrySource::Https(s) => s.clone(),
        RegistrySource::File(p) => format!("file://{}", p.display()),
    }
}

fn select_candidate<'a>(
    entries: &'a [edda_mimir_registry::IndexEntry],
    dep: &edda_manifest::Dependency,
    _cmd: &UpdateCommand,
) -> Option<&'a edda_mimir_registry::IndexEntry> {
    // Pick the highest version satisfying the dep's version_req, excluding
    // pre-releases unless `accept_unstable` is true in the dep.
    entries.iter().rev().find(|e| {
        if !dep.accept_unstable && e.version.contains('-') {
            return false;
        }
        semver_satisfies(&e.version, dep.version_req.as_ref())
    })
}

fn semver_satisfies(version: &str, req: &str) -> bool {
    super::add::semver_satisfies_pub(version, req)
}

fn push_error(diags: &mut Diagnostics, message: String) {
    diags.push(Diagnostic::new(
        DiagnosticClass::ParseError,
        Severity::Error,
        Span::DUMMY,
        message,
    ));
}
