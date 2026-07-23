//! B22 err-propagation gate at call sites.
//!
//! `check_err_propagation` verifies that every `err: T` entry in a
//! callee's translated row is propagated via an enclosing `?` or
//! discharged by an enclosing `handle err: T -> ...` body.

use edda_diag::DiagnosticClass;
use edda_intern::Symbol;
use edda_span::Span;

use crate::effect::{EffectEntry, PureEffect};

use super::super::InferCx;

/// Verify that every `err: T` entry in the callee's translated row is
/// either propagated via an enclosing `?` operand or discharged by an
/// enclosing `handle err: T -> recovery { ... }` body.
///
/// Per `effect-tracking.md §3` and the B22 compiler-bug log: declaring
/// `err: T` in the enclosing function's row authorises the *function*
/// to propagate the error, but each call site still needs an explicit
/// propagator (`?`) or a handler. Without this gate the second of two
/// statement-position calls to a fallible function silently discards
/// its effect — the first call's return value is the function result
/// and the second call's side effects never materialise at runtime.
///
/// Emits one `EffectRowMismatch` diagnostic per `err: T` entry the
/// callee declares when neither propagation route applies. Non-err
/// entries (capabilities, panic, yield, divergence) pass through
/// untouched — those are handled by other rules (row-containment at
/// function exit, comptime purity, etc.).
pub(crate) fn check_err_propagation(
    translated_entries: &[EffectEntry],
    callee_name: Symbol,
    span: Span,
    ic: &mut InferCx<'_>,
) {
    if ic.try_position_depth > 0 {
        return;
    }
    let mut emitted = std::collections::BTreeSet::new();
    for entry in translated_entries {
        let EffectEntry::Pure(PureEffect::Err(_)) = entry else {
            continue;
        };
        if ic.discharged_effects.contains(entry) {
            continue;
        }
        if !emitted.insert(*entry) {
            continue;
        }
        let rendered = match entry {
            EffectEntry::Pure(PureEffect::Err(t)) => {
                format!("err: {}", ic.display_ty(*t))
            }
            _ => unreachable!(),
        };
        let callee_display = ic.lower.interner.resolve(callee_name).to_string();
        ic.emit_diagnostic(
            DiagnosticClass::EffectRowMismatch,
            span,
            format!(
                "call to `{callee_display}` propagates `{rendered}` but the result \
                 is not wrapped in `?` and no enclosing `handle err: T -> recovery {{ ... }}` \
                 admits it; declaring the err in the enclosing function's row is not \
                 enough — each fallible call needs explicit propagation",
            ),
        );
    }
}
