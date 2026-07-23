//! `HirExprKind::Handle` lowering — `handle err: T -> recovery { body }`.
//!
//! Lowering produces three blocks plus a result local:
//!
//! - **body_bb (current)** — `body`'s lowering. On normal completion
//!   the body's value is assigned into `result_local` and control
//!   flows to `join_bb`. While `body` is being walked a
//!   [`crate::lower::ctx::HandlerFrame`] sits on `ctx.handler_stack`;
//!   any `?` inside body whose first-err-entry resolves to the
//!   matching ADT routes its on-error successor to `recovery_bb`
//!   (see [`super::raise::lower_try`]).
//! - **recovery_bb** — the recovery expression's lowering. Its value
//!   is assigned into `result_local`; control flows to `join_bb`.
//! - **join_bb** — the post-handler block. `lower_handle` returns
//!   `Operand::Move(Place::local(result_local))` with `current_bb`
//!   set to `join_bb`.
//!
//! Only the `err: T` form is currently admitted, per `effects.md`. The
//! typechecker has already verified `body.ty == recovery.ty == expr.ty`
//! and that `effect` is the `err` keyword; this layer trusts the
//! proof.

use edda_span::Span;
use edda_types::{HirBlock, HirExpr, TyKind};

use crate::error::{LoweringError, MirError};
use crate::ids::AdtId;
use crate::operand::Operand;
use crate::place::Place;
use crate::Mutability;

use super::ctx::{HandlerFrame, LoweringContext};
use super::expr::{lower_block_expr, lower_expr_to_operand};
use super::cfg::{alloc_block, assign_into, goto};
use super::ty::lower_ty;

/// Lower a `handle err: T [as <binder>] -> recovery { body }` expression.
///
/// Allocates a result local typed by `expr.ty`, two new blocks
/// (`recovery_bb` and `join_bb`), and (when the source bound the
/// caught payload) a user local for the binder, then runs the body
/// inside a handler-frame scope. On body completion the value is
/// written into the result local; the recovery branch is filled in
/// afterwards and also writes into the result local. The
/// expression's value comes from the result local at `join_bb`.
pub(super) fn lower_handle(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    handled_ty: edda_types::TyId,
    binder: Option<edda_syntax::ast::Ident>,
    recovery: &HirExpr,
    body: &HirBlock,
) -> Operand {
    let Some(handled_adt) = resolve_handled_adt(ctx, handled_ty, expr.span) else {
        // Diagnostic already pushed. Walk body + recovery for their
        // side-effects so embedded diagnostics still surface.
        let _ = lower_block_expr(ctx, body);
        let _ = lower_expr_to_operand(ctx, recovery);
        return Operand::Unit;
    };

    // Allocate the result local + the two extra blocks.
    let result_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, expr.ty);
    let result_local = match ctx.body.as_mut() {
        Some(body_builder) => body_builder.temp(result_ty.clone(), expr.span),
        None => return Operand::Unit,
    };
    // Allocate the binder local. Per-`?` dispatchers and checked-arith
    // err arms populate it via `binder_local` on the HandlerFrame
    // before jumping to `recovery_bb`. It is NOT registered in
    // `ctx.bindings` here: the binder is in scope only inside the
    // recovery expression, and an eager registration would let a
    // nested handler's same-named binder clobber this one before the
    // recovery is lowered.
    let binder_local = if let Some(b) = &binder {
        let binder_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, handled_ty);
        let Some(body_builder) = ctx.body.as_mut() else {
            return Operand::Unit;
        };
        Some(body_builder.user_local(b.name, Mutability::Imm, binder_ty, b.span))
    } else {
        None
    };
    let Some(recovery_bb) = alloc_block(ctx) else {
        return Operand::Unit;
    };
    let Some(join_bb) = alloc_block(ctx) else {
        return Operand::Unit;
    };

    // Push the handler frame and lower the body inside it.
    ctx.handler_stack.push(HandlerFrame {
        handled_adt,
        recovery_bb,
        join_bb,
        result_local,
        binder_local,
    });
    let body_op = lower_block_expr(ctx, body);
    // Pop the frame BEFORE walking the recovery — a `?` inside the
    // recovery expression should see the enclosing handler stack, not
    // this frame.
    let _frame = ctx.handler_stack.pop();
    // If the body's lowering didn't diverge, write its value into the
    // result local and goto the join block. If it did diverge (e.g.
    // ended with a Raise routed elsewhere), there's nothing to seal.
    if ctx.current_bb.is_some() {
        assign_into(ctx, expr.span, result_local, body_op, result_ty.clone());
        goto(ctx, expr.span, join_bb);
    }

    // Fill the recovery block. The binder is in scope only here:
    // register it around the recovery's lowering, saving any shadowed
    // outer binding of the same name and restoring it afterwards so
    // nested same-named handler binders each resolve to their own
    // local.
    ctx.current_bb = Some(recovery_bb);
    let saved_binding = binder
        .as_ref()
        .zip(binder_local)
        .map(|(b, local)| (b.name, ctx.bindings.insert(b.name, local)));
    let recovery_op = lower_expr_to_operand(ctx, recovery);
    if let Some((name, shadowed)) = saved_binding {
        match shadowed {
            Some(outer) => ctx.bindings.insert(name, outer),
            None => ctx.bindings.remove(&name),
        };
    }
    if ctx.current_bb.is_some() {
        assign_into(ctx, expr.span, result_local, recovery_op, result_ty);
        goto(ctx, expr.span, join_bb);
    }

    // Continue lowering at the join block.
    ctx.current_bb = Some(join_bb);
    Operand::Move(Place::local(result_local))
}

/// Resolve `handled_ty` (the handler's declared err payload type) to
/// the registered [`AdtId`] via `TyKind::Nominal(binding) ->
/// ctx.adt_map[binding]`. Mirrors the resolution path used by
/// `lower_effect_row` and `super::raise::resolve_err_adt`; replicated
/// here to keep the diagnostic span at the handler site.
fn resolve_handled_adt(
    ctx: &mut LoweringContext<'_>,
    handled_ty: edda_types::TyId,
    span: Span,
) -> Option<AdtId> {
    match ctx.ty_interner.kind(handled_ty) {
        TyKind::Nominal(binding) => ctx.adt_map.get(binding).copied().or_else(|| {
            ctx.errors.push(MirError::from(
                LoweringError::UnsupportedErrTypeInRow { span },
            ));
            None
        }),
        TyKind::Error => None,
        _ => {
            ctx.errors.push(MirError::from(
                LoweringError::UnsupportedErrTypeInRow { span },
            ));
            None
        }
    }
}
