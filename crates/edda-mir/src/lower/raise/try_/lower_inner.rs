//! Inner-call lowering for the two `?` paths.
//!
//! `?`'s inner expression is either a `Call` or a `MethodCall`. These
//! two helpers route it through the regular call machinery — one without
//! error propagation (the `?`-without-err diagnostic path) and one with
//! an explicit on-error successor block.

use edda_types::{HirExpr, HirExprKind};

use crate::ids::BlockId;
use crate::operand::Operand;

use crate::lower::call::lower_call;
use crate::lower::ctx::LoweringContext;
use crate::lower::expr::lower_expr_to_operand;

/// Walk an inner expression for side-effects only, without setting up
/// error propagation. Used by the `?`-without-err diagnostic path.
pub(super) fn lower_inner_no_propagation(ctx: &mut LoweringContext<'_>, inner: &HirExpr) -> Operand {
    match &inner.kind {
        HirExprKind::Call { callee, args } => lower_call(ctx, inner, callee, args, None),
        HirExprKind::MethodCall { receiver, name, args } => {
            crate::lower::call::lower_method_call(ctx, inner, receiver, *name, args)
        }
        _ => lower_expr_to_operand(ctx, inner),
    }
}

/// Lower an inner `Call` or `MethodCall` with the given on-error
/// successor block.
pub(super) fn lower_inner_with_on_error(
    ctx: &mut LoweringContext<'_>,
    inner: &HirExpr,
    on_error: BlockId,
) -> Operand {
    match &inner.kind {
        HirExprKind::Call { callee, args } => {
            lower_call(ctx, inner, callee, args, Some(on_error))
        }
        HirExprKind::MethodCall { receiver, name, args } => {
            crate::lower::call::lower_method_call_with_on_error(
                ctx,
                inner,
                receiver,
                *name,
                args,
                on_error,
            )
        }
        _ => lower_expr_to_operand(ctx, inner),
    }
}
