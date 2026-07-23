//! Explicit `raise <expr>` lowering.
//!
//! The payload's [`edda_types::TyId`] resolves to a
//! [`edda_types::TyKind::Nominal`] whose binding keys
//! [`super::super::ctx::LoweringContext::adt_map`]; the resulting
//! [`crate::AdtId`] is the `err_adt` argument of
//! [`crate::TerminatorKind::Raise`]. The current block is sealed; the
//! diverging operand `Operand::Unit` is returned.

use edda_types::HirExpr;

use crate::operand::Operand;

use super::super::ctx::LoweringContext;
use super::super::expr::lower_expr_to_operand;
use super::{resolve_err_adt, seal_raise};

/// Lower a `raise <inner>` expression to a `TerminatorKind::Raise`.
/// `inner.ty` must resolve to a `TyKind::Nominal(binding)` whose binding
/// is registered in `ctx.adt_map`; that is the err ADT propagated to
/// the caller. The payload itself is lowered to an [`Operand`].
pub(in super::super) fn lower_raise(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    inner: &HirExpr,
) -> Operand {
    let value = lower_expr_to_operand(ctx, inner);
    // The payload may itself have diverged (e.g. `raise return 1`); if so,
    // `current_bb` is already `None` and there is nothing left to seal.
    if ctx.current_bb.is_none() {
        return Operand::Unit;
    }
    let Some(err_adt) = resolve_err_adt(ctx, inner.ty, expr.span) else {
        // Diagnostic already pushed by `resolve_err_adt`; leave `current_bb`
        // alone so the caller's surrounding lowering still has a place to
        // emit into. Diverging emission would mask the original problem.
        return Operand::Unit;
    };
    seal_raise(ctx, expr.span, err_adt, value);
    Operand::Unit
}
