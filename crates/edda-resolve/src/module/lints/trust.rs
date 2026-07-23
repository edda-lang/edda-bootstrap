//! Trust-hatch density and trust-budget lints (escape-hatch ratchet).

use std::collections::HashMap;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_syntax::ast::ItemKind;

use crate::ResolvedPackage;
use crate::resolve::ResolveCx;

/// Emit `trust_hatch_too_dense` for every source directory whose ratio
/// of `@unverified` + `@trust`-attributed functions to total functions
/// exceeds the configured threshold.
///
/// The default threshold is 10% — a single hatched function in a tiny
/// directory will not trigger; a crypto-heavy directory with many
/// hatches will. The lint is per-directory, mirroring the structmap
/// density lint: the directory's `index.toon` is what audit consumers
/// see, so the lint operates on that grouping.
///
/// Two-threshold (warn vs deny) configuration via
/// `[lints.trust_hatch_density]` in `package.toml` is a follow-up; for
/// now the single threshold yields a warning that the user can
/// escalate to error via `lints.trust_hatch_too_dense = "error"`.
pub fn emit_trust_hatch_too_dense_lints(
    package: &ResolvedPackage,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) {
    let severity = lint_cfg.effective(DiagnosticClass::TrustHatchTooDense);
    const WARN_RATIO: f64 = 0.10;
    // Per-directory aggregate: (total fns, hatched fns, representative span)
    let mut by_dir: HashMap<std::path::PathBuf, (usize, usize, edda_span::Span)> = HashMap::new();
    for module in &package.modules {
        let entry = package.graph.module(module.id);
        let Some(dir) = entry.file_path.parent() else {
            continue;
        };
        let dir = dir.to_path_buf();
        let slot = by_dir.entry(dir).or_insert((0, 0, edda_span::Span::DUMMY));
        for item in &entry.ast.items {
            let ItemKind::Function(fn_decl) = &item.kind else {
                continue;
            };
            slot.0 += 1;
            let is_hatched = item.attributes.iter().any(|a| {
                matches!(cx.interner.resolve(a.name.name), "unverified" | "trust")
            });
            if is_hatched {
                slot.1 += 1;
                if slot.2 == edda_span::Span::DUMMY {
                    slot.2 = item.span;
                }
            }
            let _ = fn_decl; // fn_decl currently unused; reserved for future filtering
        }
    }
    for (dir, (total, hatched, primary_span)) in by_dir {
        if total == 0 {
            continue;
        }
        let ratio = hatched as f64 / total as f64;
        if ratio < WARN_RATIO {
            continue;
        }
        let pct = ratio * 100.0;
        let dir_text = dir.display().to_string();
        let diag = Diagnostic::new(
            DiagnosticClass::TrustHatchTooDense,
            severity,
            primary_span,
            format!(
                "directory `{dir_text}` has high trust-hatch density: {hatched}/{total} functions ({pct:.1}%) carry `@unverified` or `@trust`",
            ),
        )
        .with_note(
            "audit the hatched functions: split them into a focused submodule, or replace `@unverified`/`@trust` with discharged refinements where possible",
        );
        diags.push(diag);
    }
}

/// Emit a hard error when the package's total count of `@unverified` /
/// `@trust` escape hatches exceeds the manifest's `[package].max_trust`
/// ceiling.
///
/// This is the trust budget: trust becomes a visible, capped,
/// deferred obligation. Reuses [`DiagnosticClass::TrustHatchTooDense`] —
/// the locked 40-class set is not extended — but pins `Severity::Error`
/// so the build fails (`Diagnostics::has_errors`) rather than warning,
/// which is the CI ratchet. Only called when the manifest sets
/// `max_trust`; absent, no ceiling applies.
pub fn emit_trust_budget_lints(
    package: &ResolvedPackage,
    cx: &ResolveCx<'_>,
    max_trust: u32,
    diags: &mut Diagnostics,
) {
    let mut count: u32 = 0;
    let mut primary_span = edda_span::Span::DUMMY;
    for module in &package.modules {
        let entry = package.graph.module(module.id);
        for item in &entry.ast.items {
            if !matches!(item.kind, ItemKind::Function(_)) {
                continue;
            }
            let is_hatched = item
                .attributes
                .iter()
                .any(|a| matches!(cx.interner.resolve(a.name.name), "unverified" | "trust"));
            if is_hatched {
                count += 1;
                if primary_span == edda_span::Span::DUMMY {
                    primary_span = item.span;
                }
            }
        }
    }
    if count > max_trust {
        let diag = Diagnostic::new(
            DiagnosticClass::TrustHatchTooDense,
            edda_diag::Severity::Error,
            primary_span,
            format!(
                "package trust budget exceeded: {count} `@unverified` / `@trust` points exceed the `[package].max_trust = {max_trust}` ceiling",
            ),
        )
        .with_note(
            "the trust budget ratchets verification escape hatches: discharge an obligation, or raise `[package].max_trust` deliberately if the increase is intended",
        );
        diags.push(diag);
    }
}
