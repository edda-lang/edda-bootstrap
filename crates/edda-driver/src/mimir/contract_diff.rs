//! `edda contract-diff` orchestration — §8.5.
//!
//! Human-readable surface/effect diff between two rune versions. Each argument
//! is either `<name>@<version>` (look up in registry) or a local `.rune` path.
//!
//! Output grouped by SemVer impact: patch / minor / major.

use std::path::Path;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, Severity};
use edda_mimir_archive::{RuneLayout, unpack};
use edda_mimir_hash::{compute_effect_hash, compute_surface_hash};
use edda_mimir_registry::{NetworkCap, fetch_archive};
use edda_span::Span;

use crate::command::{ContractDiffCommand, MimirOptions};
use crate::outcome::Outcome;

/// Execute `edda contract-diff <a> <b>`.
pub fn run_contract_diff(
    cmd: &ContractDiffCommand,
    options: &MimirOptions,
    diags: &mut Diagnostics,
) -> Outcome {
    // -----------------------------------------------------------------------
    // Load both rune layouts.
    // -----------------------------------------------------------------------
    let layout_a = match load_rune_arg(&cmd.a, &options.manifest_path, diags) {
        Some(l) => l,
        None => return Outcome::build_error(diags),
    };
    let layout_b = match load_rune_arg(&cmd.b, &options.manifest_path, diags) {
        Some(l) => l,
        None => return Outcome::build_error(diags),
    };

    // -----------------------------------------------------------------------
    // Compute hashes for both.
    // -----------------------------------------------------------------------
    let surface_a = match compute_surface_hash(&layout_a.surface) {
        Ok(h) => h,
        Err(e) => {
            push_error(diags, format!("edda contract-diff: surface hash error for `{}`: {}", cmd.a, e));
            return Outcome::build_error(diags);
        }
    };
    let surface_b = match compute_surface_hash(&layout_b.surface) {
        Ok(h) => h,
        Err(e) => {
            push_error(diags, format!("edda contract-diff: surface hash error for `{}`: {}", cmd.b, e));
            return Outcome::build_error(diags);
        }
    };
    let effect_a = match compute_effect_hash(&layout_a.surface) {
        Ok(h) => h,
        Err(e) => {
            push_error(diags, format!("edda contract-diff: effect hash error for `{}`: {}", cmd.a, e));
            return Outcome::build_error(diags);
        }
    };
    let effect_b = match compute_effect_hash(&layout_b.surface) {
        Ok(h) => h,
        Err(e) => {
            push_error(diags, format!("edda contract-diff: effect hash error for `{}`: {}", cmd.b, e));
            return Outcome::build_error(diags);
        }
    };

    // -----------------------------------------------------------------------
    // Classify the impact.
    // -----------------------------------------------------------------------
    let surface_changed = surface_a != surface_b;
    let effect_changed = effect_a != effect_b;

    let impact = classify_impact(surface_changed, effect_changed);

    // -----------------------------------------------------------------------
    // Emit the diff summary as diagnostics.
    // -----------------------------------------------------------------------
    let severity = if surface_changed || effect_changed {
        Severity::Warn
    } else {
        Severity::Info
    };

    diags.push(Diagnostic::new(
        DiagnosticClass::ParseError,
        severity,
        Span::DUMMY,
        format!(
            "edda contract-diff: {} vs {} — impact: {}",
            cmd.a, cmd.b, impact
        ),
    ));

    if surface_changed {
        diags.push(Diagnostic::new(
            DiagnosticClass::ParseError,
            Severity::Warn,
            Span::DUMMY,
            format!(
                "  surface_hash changed: {} → {}",
                &surface_a[..16.min(surface_a.len())],
                &surface_b[..16.min(surface_b.len())]
            ),
        ));
    }
    if effect_changed {
        diags.push(Diagnostic::new(
            DiagnosticClass::ParseError,
            Severity::Warn,
            Span::DUMMY,
            format!(
                "  effect_hash changed: {} → {}",
                &effect_a[..16.min(effect_a.len())],
                &effect_b[..16.min(effect_b.len())]
            ),
        ));
    }
    if !surface_changed && !effect_changed {
        diags.push(Diagnostic::new(
            DiagnosticClass::ParseError,
            Severity::Info,
            Span::DUMMY,
            "  no surface or effect changes detected (patch-level change only)".to_owned(),
        ));
    }

    Outcome::success(diags)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Classify the SemVer impact of the observed changes.
fn classify_impact(surface_changed: bool, effect_changed: bool) -> &'static str {
    if surface_changed {
        "major (stable surface changed)"
    } else if effect_changed {
        "minor (effect surface changed)"
    } else {
        "patch"
    }
}

/// Load a `RuneLayout` from either a `<name>@<version>` registry spec or a local `.rune` path.
fn load_rune_arg(arg: &str, manifest_path: &Path, diags: &mut Diagnostics) -> Option<RuneLayout> {
    if arg.ends_with(".rune") && Path::new(arg).exists() {
        // Local file path.
        let bytes = match std::fs::read(arg) {
            Ok(b) => b,
            Err(e) => {
                push_error(
                    diags,
                    format!("edda contract-diff: cannot read `{}`: {}", arg, e),
                );
                return None;
            }
        };
        match unpack(&bytes) {
            Ok(l) => Some(l),
            Err(e) => {
                push_error(
                    diags,
                    format!("edda contract-diff: unpack error for `{}`: {}", arg, e),
                );
                None
            }
        }
    } else {
        // Registry spec: `<name>@<version>`.
        let (name, version) = match arg.split_once('@') {
            Some((n, v)) => (n, v),
            None => {
                push_error(
                    diags,
                    format!(
                        "edda contract-diff: `{}` is not a local `.rune` file nor a `<name>@<version>` spec",
                        arg
                    ),
                );
                return None;
            }
        };
        let source = super::add::pick_registry_source(manifest_path, name, diags);
        match fetch_archive(&source, name, version, NetworkCap) {
            Ok(bytes) => match unpack(&bytes) {
                Ok(l) => Some(l),
                Err(e) => {
                    push_error(
                        diags,
                        format!(
                            "edda contract-diff: unpack error for `{}@{}`: {}",
                            name, version, e
                        ),
                    );
                    None
                }
            },
            Err(e) => {
                push_error(
                    diags,
                    format!(
                        "edda contract-diff: fetch error for `{}@{}`: {}",
                        name, version, e
                    ),
                );
                None
            }
        }
    }
}

fn push_error(diags: &mut Diagnostics, message: String) {
    diags.push(Diagnostic::new(
        DiagnosticClass::ParseError,
        Severity::Error,
        Span::DUMMY,
        message,
    ));
}
