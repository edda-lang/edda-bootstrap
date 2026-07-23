//! Function-body type-checking entry point — enforces the §4 exit
//! rule and the effect-row containment check from
//! `effect-tracking.md §1` / `inference-rules.md §1a.4`.

use edda_diag::DiagnosticClass;
use edda_span::Span;

use super::{InferCx, TyEnv, synth_block};
use crate::effect::{EffectEntry, EffectRow, PureEffect};
use crate::hir::HirBlock;
use crate::ty::TyId;

/// Type-check a function body and enforce the §4 function-exit rule
/// plus the row-containment check.
///
/// `body` is the function's [`HirBlock`]; `declared` is the row
/// declared by the function's `with { ... }` clause (or
/// [`EffectRow::empty`] when absent); `exit_span` is the source range
/// to attribute exit diagnostics to (typically the closing brace of
/// the body or the trailing `return` site). Returns the synthesised
/// body type for caller convenience.
///
/// # Row check
///
/// On entry, the function's [`InferCx::declared_row`] and
/// [`InferCx::row`] accumulator are swapped to `declared` and a fresh
/// accumulator. On exit, every entry accumulated during the body walk
/// is compared against `declared`: each entry not in `declared`
/// produces one diagnostic attributed to `exit_span`. The previous
/// declared row and accumulator are then restored — this makes the
/// call re-entrant for nested function bodies (closures land in a
/// later wave; the save/restore is in place so the existing surface
/// is forward-compatible).
///
/// # Stream-D diagnostic projection
///
/// Missing entries are projected to one of three classes per the
/// Phase C rebucket of the original generic `effect_row_mismatch`:
///
/// - `Pure(Divergence)` with `inject_divergence == true`
///   → `DiagnosticClass::TerminationUnproven` — the function is a
///   recursive-SCC member without a `decreases` measure, so its
///   *signature* needs the fix (supply `decreases` or admit
///   `divergence` in the row).
/// - `Pure(Divergence)` with `inject_divergence == false`
///   → `DiagnosticClass::DivergenceNotAdmitted` — divergence was
///   pushed from inside the body (loop without `decreases`, explicit
///   `divergence` expression) but the declared row doesn't admit it.
/// - any other missing entry
///   → `DiagnosticClass::EffectRowMismatch` (unchanged).
///
/// # §4 exit rule (re-statement)
///
/// The caller is responsible for pushing the function's parameters
/// into `env` before invoking this function — each non-`init`
/// parameter starts in [`super::BindingState::Valid`] per the
/// M-FunCall row from §4. An `init`-mode parameter is the symmetric
/// case: it starts `Uninit`, and the same exit check below requires
/// it to reach `Valid` by the time the body returns — via a plain
/// assignment or an `init`-arg delegation to another init-taking
/// callee.
pub(crate) fn check_fn_body(
    env: &mut TyEnv,
    body: &mut HirBlock,
    ic: &mut InferCx<'_>,
    declared: &EffectRow,
    return_ty: TyId,
    exit_span: Span,
    inject_divergence: bool,
    take_param_syms: &[edda_intern::Symbol],
) -> TyId {
    let prev_declared = std::mem::replace(&mut ic.declared_row, declared.clone());
    let prev_row = std::mem::take(&mut ic.row);
    let prev_return_ty = std::mem::replace(&mut ic.fn_return_ty, return_ty);

    // A function whose call-graph SCC is recursive without
    // any member supplying a `decreases` measure accumulates a
    // `Pure(Divergence)` entry at function entry. The row-containment
    // check below then surfaces the missing entry — projecting it to
    // `termination_unproven` so the diagnostic names the root cause
    // (no `decreases`) rather than the symptom (extra row entry).
    if inject_divergence {
        ic.push_effect_entry(EffectEntry::Pure(PureEffect::Divergence));
    }

    let ty = synth_block(env, body, ic);
    super::mode::check_fn_body_exit(env, ic, exit_span, take_param_syms);
    check_row_containment(ic, declared, exit_span, inject_divergence);

    ic.declared_row = prev_declared;
    ic.row = prev_row;
    ic.fn_return_ty = prev_return_ty;
    ty
}

/// Compare the accumulated row (canonicalised) against `declared` and
/// emit one diagnostic per entry that the function body performed but
/// did not declare. The diagnostic class is chosen per the Stream-D
/// projection documented on [`check_fn_body`].
///
/// The check is a *containment* check — extra entries in `declared`
/// are not a diagnostic (functions may declare effects they do not
/// perform; the surface formatter may even normalise toward a richer
/// declaration). Only entries *performed but not declared* are
/// erroneous.
fn check_row_containment(
    ic: &mut InferCx<'_>,
    declared: &EffectRow,
    exit_span: Span,
    inject_divergence: bool,
) {
    let actual = ic.row.to_row();
    if actual.is_empty() {
        return;
    }
    // Snapshot entries first so we don't hold a borrow of `ic` across
    // the diagnostic emission.
    let missing: Vec<EffectEntry> = actual
        .entries()
        .iter()
        .copied()
        .filter(|entry| !declared.contains(entry))
        .collect();
    for entry in missing {
        let declared_rendered = declared
            .display(ic.lower.interner, ic.lower.ty_interner)
            .to_string();
        let (class, message) = project_missing(entry, &declared_rendered, inject_divergence, ic);
        ic.emit_diagnostic(class, exit_span, message);
    }
}

/// Project a missing row entry to its diagnostic class and message.
///
/// `Pure(Divergence)` splits along `inject_divergence`:
/// `true` ⇒ `TerminationUnproven` (no `decreases` on a recursive
/// function-entry injection), `false` ⇒ `DivergenceNotAdmitted`
/// (divergence pushed from inside the body). Every other entry stays
/// on `EffectRowMismatch`.
fn project_missing(
    entry: EffectEntry,
    declared_rendered: &str,
    inject_divergence: bool,
    ic: &InferCx<'_>,
) -> (DiagnosticClass, String) {
    if let EffectEntry::Pure(PureEffect::Divergence) = entry {
        if inject_divergence {
            return (
                DiagnosticClass::TerminationUnproven,
                format!(
                    "function is recursive but supplies no `decreases` \
                     measure; either supply `decreases` on the function (or \
                     a recursive sibling) or admit `divergence` in the \
                     declared row (currently `{declared_rendered}`)",
                ),
            );
        }
        return (
            DiagnosticClass::DivergenceNotAdmitted,
            format!(
                "function performs `divergence` but its declared row does \
                 not admit it; either supply `decreases` for the unbounded \
                 loop or add `divergence` to the row (currently \
                 `{declared_rendered}`)",
            ),
        );
    }
    let rendered = render_entry(entry, ic);
    (
        DiagnosticClass::EffectRowMismatch,
        format!(
            "function performs effect `{rendered}` but does not declare it; \
             declared row is `{declared_rendered}`",
        ),
    )
}

/// Render a single [`EffectEntry`] as it appears in source — used by
/// the `effect_row_mismatch` diagnostic. Mirrors the rendering inside
/// [`EffectRow::display`] for a single entry.
fn render_entry(entry: EffectEntry, ic: &InferCx<'_>) -> String {
    match entry {
        EffectEntry::Capability(sym) => ic.lower.interner.resolve(sym).to_string(),
        EffectEntry::Pure(PureEffect::Panic) => "panic".to_string(),
        EffectEntry::Pure(PureEffect::Err(t)) => {
            format!("err: {}", ic.display_ty(t))
        }
        EffectEntry::Pure(PureEffect::Yield(t)) => {
            format!("yield: {}", ic.display_ty(t))
        }
        EffectEntry::Pure(PureEffect::Divergence) => "divergence".to_string(),
        EffectEntry::Pure(PureEffect::Cancellation) => "cancellation".to_string(),
        EffectEntry::Pure(PureEffect::Nondet) => "nondet".to_string(),
    }
}

#[cfg(test)]
#[path = "fn_body_tests.rs"]
mod tests;
