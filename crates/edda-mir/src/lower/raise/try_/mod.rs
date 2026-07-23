//! `?`-propagation lowering.
//!
//! Only `<call-expr>?` is currently admitted. The callee's [`edda_types::FnSig`]
//! effect row supplies the err type(s); the call's `on_error` successor
//! branches to a per-variant dispatcher that routes each runtime err
//! variant either to an enclosing handler's `recovery_bb` or to a
//! propagation block that re-raises that specific err.
//!
//! For single-err callees the dispatcher collapses into a single edge —
//! either straight to the matching handler or to a single propagation
//! block. For multi-err callees (callee row has `err: E1, err: E2,
//! ...`) the dispatcher is a `SwitchTag` over the call destination's
//! `__Result` sum, with one arm per err variant; each arm picks the
//! correct handler or propagation block independently. This is the
//! structural fix for §C9: every raise in the callee maps to the right
//! handler/propagation per its dynamic variant, not to whichever err
//! type sorts first in the canonical row.
//!
//! # Layout
//!
//! - [`resolve`] — err-row discovery + admissibility / handler predicates.
//! - [`lower_inner`] — inner `Call` / `MethodCall` lowering (with / without
//!   on-error successor).
//! - [`dispatch`] — dispatcher block + propagation block construction and
//!   the success-path Ok-payload extraction.
//! - this file — the [`lower_try`] orchestrator.

mod dispatch;
mod lower_inner;
mod resolve;

use edda_types::{HirExpr, HirExprKind};

use crate::error::{LoweringError, MirError};
use crate::operand::Operand;

use crate::lower::cfg::alloc_block;
use crate::lower::ctx::LoweringContext;
use crate::lower::expr::lower_expr_to_operand;

use dispatch::extract_ok_payload;
pub(in crate::lower) use dispatch::fill_dispatcher_block;
use lower_inner::{lower_inner_no_propagation, lower_inner_with_on_error};
use resolve::{body_has_error_scope, callee_err_adts_of_inner};

/// Lower an `<inner>?` expression. Only the
/// `<call-expr>?` shape is currently admitted: `inner` must be a `HirExprKind::Call` or
/// `HirExprKind::MethodCall`. The callee's effect row supplies the
/// err type(s); the call's `on_error` successor branches to a fresh
/// dispatcher block that fans out per err variant.
pub(in crate::lower) fn lower_try(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    inner: &HirExpr,
) -> Operand {
    // `?` is admissible when (a) the enclosing function declares at
    // least one `err: E` OR (b) an enclosing
    // `handle err: T -> recovery { body }` is on the handler stack
    // (the handler discharges the err locally, so the
    // function need not redeclare it). Both paths satisfy the
    // `effect-tracking.md §3` formal rule because the err entry has
    // a known sink.
    if !body_has_error_scope(ctx) && ctx.handler_stack.is_empty() {
        ctx.errors.push(MirError::from(LoweringError::TryOutsideErrorScope {
            span: expr.span,
        }));
        // Lower `inner` for its side-effects (diagnostics) but discard the
        // result — the success path has no meaningful continuation here.
        let _ = lower_expr_to_operand(ctx, inner);
        return Operand::Unit;
    }

    // The inner expression is either a regular function `Call` or a
    // `MethodCall`. The two route through different lowering helpers
    // but the propagation seam is identical: pass `Some(dispatcher_bb)`
    // as the call's on-error successor.
    match &inner.kind {
        HirExprKind::Call { .. } | HirExprKind::MethodCall { .. } => {}
        _ => {
            ctx.errors.push(MirError::from(LoweringError::UnsupportedHirVariant {
                variant: "Try (inner is not a Call)",
                span: inner.span,
            }));
            let _ = lower_expr_to_operand(ctx, inner);
            return Operand::Unit;
        }
    }

    let err_adts = match callee_err_adts_of_inner(ctx, inner, expr.span) {
        Some(v) if !v.is_empty() => v,
        _ => {
            // Either the callee declares no err entries, or resolution
            // failed and a diagnostic was already pushed. Lower the call
            // with no error propagation so embedded diagnostics surface.
            return lower_inner_no_propagation(ctx, inner);
        }
    };

    let Some(dispatcher_bb) = alloc_block(ctx) else {
        return Operand::Unit;
    };

    // The call seals the current block, sets `current_bb = target_bb`, and
    // returns `Operand::Move(Place::local(dest_local))` where `dest_local`
    // is sized for the callee's actual return — `__Result<T, E1, E2, ...>`
    // for raising callees per `super::call::callee_actual_ret_ty`.
    let result_op = lower_inner_with_on_error(ctx, inner, dispatcher_bb);

    // Fill the dispatcher: route each err variant to its matching
    // handler (if any frame on the handler stack discharges it) or to
    // a fresh propagation block that raises that exact err type.
    fill_dispatcher_block(ctx, expr.span, dispatcher_bb, &err_adts, &result_op);

    // Success path (current_bb == target_bb): extract the Ok payload
    // from the Result-typed destination into a fresh local typed as
    // `expr.ty` (= T). That T-typed local is what the rest of the
    // expression sees as the Try's value.
    extract_ok_payload(ctx, expr.span, expr.ty, &result_op)
}
