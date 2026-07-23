//! Comptime-purity gate at call sites (P-CompTimePure, `inference-rules.md §1a.6`).
//!
//! `check_comptime_purity` verifies the callee's declared row is a
//! subset of `{panic, yield: T}` when the call appears inside a
//! `comptime` body.

use edda_diag::DiagnosticClass;
use edda_intern::Symbol;
use edda_span::Span;

use crate::effect::{EffectEntry, EffectRow, PureEffect};

use super::super::InferCx;

/// Verify the callee's declared row is permitted inside a comptime
/// context. Per `inference-rules.md §1a.6`, only `panic` and
/// `yield: T` entries are admissible (the prose form: "row(f) ⊆
/// {panic, yield: T}"). Other entries — capability or `err: T` —
/// disqualify the callee.
pub(crate) fn check_comptime_purity(
    effects: &EffectRow,
    callee_name: Symbol,
    span: Span,
    ic: &mut InferCx<'_>,
) {
    let offending: Vec<EffectEntry> = effects
        .entries()
        .iter()
        .copied()
        .filter(|e| {
            !matches!(
                e,
                EffectEntry::Pure(PureEffect::Panic) | EffectEntry::Pure(PureEffect::Yield(_))
            )
        })
        .collect();
    if offending.is_empty() {
        return;
    }
    let callee_display = ic.lower.interner.resolve(callee_name).to_string();
    let rendered: Vec<String> = offending
        .iter()
        .map(|e| render_entry_short(*e, ic))
        .collect();
    ic.emit_diagnostic(
        DiagnosticClass::ComptimePurityLoss,
        span,
        format!(
            "call to `{callee_display}` inside a comptime context is not comptime-pure; \
             the callee's row contains non-pure entries: {}",
            rendered.join(", "),
        ),
    );
}

/// Render a single [`EffectEntry`] as it appears in source — used by
/// the comptime-purity diagnostic. A trimmed copy of the renderer in
/// `fn_body.rs`; lives here so the call-site diagnostic does not
/// reach across module boundaries.
fn render_entry_short(entry: EffectEntry, ic: &InferCx<'_>) -> String {
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
