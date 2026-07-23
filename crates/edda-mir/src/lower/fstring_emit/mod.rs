//! FString lowering — emit the format-call + string-concat sequence.
//!
//! Slots are parsed as full expressions by the surface parser
//! and arrive here as typed
//! [`HirFStringPart`]s. This module converts each part to a `String`
//! operand — a literal text run becomes a `String` const; a slot
//! becomes its value formatted via `__edda_format_<T>` (chosen by the
//! slot's MIR type) — then folds them all with `__edda_string_concat`.
//!
//! # Layout
//!
//! - [`format`] — per-type `format_<T>` dispatch + operand widening,
//!   plus the slot-expression staging in [`format::emit_format_for_slot`].
//! - [`emit`] — low-level const / concat / single-arg call emission.
//! - this file — the [`lower_fstring`] orchestrator.

mod emit;
mod format;

use edda_types::{HirExpr, HirFStringPart};

use crate::operand::Operand;

use super::ctx::LoweringContext;

use emit::{emit_concat_call, emit_str_const};
use format::emit_format_for_slot;

/// Lower `f"...{expr}..."` to a sequence of per-part format calls, then
/// concatenate every part into a single `String` operand.
pub(super) fn lower_fstring(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    parts: &[HirFStringPart],
) -> Operand {
    if parts.is_empty() {
        let empty_sym = ctx.interner.intern("");
        return emit_str_const(ctx, expr, empty_sym);
    }

    let mut operands: Vec<Operand> = Vec::with_capacity(parts.len());
    for part in parts {
        let op = match part {
            HirFStringPart::Text(sym) => emit_str_const(ctx, expr, *sym),
            HirFStringPart::Slot(slot) => match emit_format_for_slot(ctx, slot) {
                Some(op) => op,
                None => return Operand::Unit,
            },
        };
        if matches!(op, Operand::Unit) && ctx.current_bb.is_none() {
            // Diverged inside a helper — bail.
            return Operand::Unit;
        }
        operands.push(op);
    }

    // Left-fold: concat all parts.
    let mut acc = operands.remove(0);
    for part in operands {
        acc = emit_concat_call(ctx, expr.span, acc, part);
    }
    acc
}
