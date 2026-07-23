//! `HirExprKind::MethodCall` -> `TerminatorKind::Call` lowering.
//!
//! A `r.m(args)` method call desugars to a `Call` against the resolved
//! receiver-extended free function with the receiver prepended at
//! argument position 0. Capability-method calls (`allocator.alloc(...)`)
//! are intercepted first and routed to [`super::super::capability_method_call`].

use edda_types::{HirCallArg, HirExpr};

use crate::ids::BlockId;
use crate::operand::Operand;

use super::super::ctx::LoweringContext;
use super::super::expr::lower_expr_to_operand;
use super::{lower_call_to_binding, push_unknown_function};

/// Lower `HirExprKind::MethodCall { receiver, name, args }` by
/// consulting `ctx.method_resolutions[expr.span]` for the receiver-
/// extended free function's `BindingId`, then routing through the same
/// call machinery as a regular `Call`. The receiver becomes the first
/// positional argument; the typechecker's
/// `infer::method::synth_method_call` has already validated arity and
/// types so this layer trusts the resolution.
pub(in crate::lower) fn lower_method_call(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    receiver: &HirExpr,
    name: edda_syntax::ast::Ident,
    args: &[HirCallArg],
) -> Operand {
    lower_method_call_with_on_error_inner(ctx, expr, receiver, name, args, None)
}

/// Method-call lowering with an explicit on-error successor. Used by
/// [`super::super::raise::lower_try`] when the `?` operator wraps a method
/// call: the propagation block (or, when a handler is on the stack,
/// the handler's recovery block) becomes the call's on-error
/// successor.
pub(in crate::lower) fn lower_method_call_with_on_error(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    receiver: &HirExpr,
    name: edda_syntax::ast::Ident,
    args: &[HirCallArg],
    on_error: BlockId,
) -> Operand {
    lower_method_call_with_on_error_inner(ctx, expr, receiver, name, args, Some(on_error))
}

/// Shared body of [`lower_method_call`] and
/// [`lower_method_call_with_on_error`]. Routes the resolved
/// method-call site through [`lower_call_to_binding`] with the
/// receiver prepended at argument position 0.
fn lower_method_call_with_on_error_inner(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    receiver: &HirExpr,
    name: edda_syntax::ast::Ident,
    args: &[HirCallArg],
    on_error: Option<BlockId>,
) -> Operand {
    // Capability-method dispatch comes BEFORE the regular
    // method-resolutions path because the typechecker records the
    // capability-method site in `ctx.capability_method_calls` instead of
    // resolving it to a free function (the catalogue has no Edda-side
    // declaration). MIR synthesises a Call against the variant's
    // `__edda_*` extern name so the existing alloc-family rewrite
    // promotes it to the `_raw`-suffixed runtime ABI.
    if let Some(&method) = ctx.capability_method_calls.get(&expr.span) {
        return super::super::capability_method_call::lower_capability_method_call(
            ctx, expr, receiver, method, args, on_error,
        );
    }
    let Some(binding) = ctx.method_resolutions.get(&expr.span).copied() else {
        // Typechecker should have rejected this site; if it didn't,
        // surface the failure rather than silently producing Unit.
        push_unknown_function(ctx, name.name, expr.span);
        let _ = lower_expr_to_operand(ctx, receiver);
        for arg in args.iter() {
            let _ = lower_expr_to_operand(ctx, &arg.expr);
        }
        return Operand::Unit;
    };
    lower_call_to_binding(ctx, expr, binding, Some(receiver), args, on_error)
}
