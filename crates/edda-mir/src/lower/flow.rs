//! Control-flow lowering helpers for the typed-HIR -> MIR pass.
//!
//! Every variant in this file allocates fresh basic blocks, threads control
//! flow through them, and rejoins the lowering at a fresh post-block (or
//! leaves `current_bb` cleared if the construct diverges). Split out of
//! `expr.rs` to keep both files under the 500-line cap.

use edda_span::Span;
use edda_types::{HirBlock, HirExpr, HirExprKind, HirMatchArm};

use crate::constant::{Const, ConstValue};
use crate::error::{LoweringError, MirError};
use crate::ids::LocalId;
use crate::operand::Operand;
use crate::place::Place;
use crate::rvalue::{Rvalue, RvalueKind};
use crate::terminator::{Terminator, TerminatorKind};
use crate::ty::{MirPrim, MirType, MirTypeKind};

use super::ctx::{LoopFrame, LoweringContext};
use super::expr::{lower_block_expr, lower_expr_to_operand};
use super::cfg::{alloc_block, alloc_three_blocks, assign_into, goto, push_assign, seal_switch_bool, seal_terminator};
use super::ty::lower_ty;

/// Lower `if cond { then } [else { else }]` to a result local + three blocks
/// (then / else / join) and a `SwitchBool` terminator.
///
/// When both branches diverge (every path leaves via `return` / `raise` /
/// `panic` / `break` / `continue`), the join block is unreachable. In that
/// case we skip the result-local allocation entirely and seal the join
/// `Unreachable`. Allocating a result local typed `expr.ty` (which is
/// `never` when both branches diverge) would trip the LLVM
/// emitter's `non-lowerable-local` guard because the join `move _result`
/// path has no alloca to load from. The previous shape produced exactly
/// that error for `if c { return X } else { return Y }`.
pub(super) fn lower_if(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    cond: &HirExpr,
    then_block: &HirBlock,
    else_branch: Option<&HirExpr>,
) -> Operand {
    let cond_op = lower_expr_to_operand(ctx, cond);
    let result_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, expr.ty);

    // Decide up-front whether we even need a result local: a result of
    // `MirTypeKind::Never` cannot be loaded by the emitter (Never locals
    // get no alloca), so we defer allocation until after we know whether
    // any branch falls through. The local is materialised lazily on the
    // first non-diverging arm and `Some(local)` is read from the join.
    let needs_value = !matches!(result_ty.kind, MirTypeKind::Never);

    let (then_bb, else_bb, join_bb) = match alloc_three_blocks(ctx) {
        Some(triple) => triple,
        None => return Operand::Unit,
    };
    seal_switch_bool(ctx, expr.span, cond_op, then_bb, else_bb);

    // Track whether either branch reaches the join. If neither does, we
    // never allocate the result local and seal `join_bb` as `Unreachable`.
    let mut result_local: Option<LocalId> = None;
    let mut any_branch_joins = false;

    ctx.current_bb = Some(then_bb);
    let then_op = lower_block_expr(ctx, then_block);
    if ctx.current_bb.is_some() {
        any_branch_joins = true;
        if needs_value {
            let local = match result_local {
                Some(l) => l,
                None => {
                    let body = ctx.body.as_mut().expect("body present in lower_if");
                    let l = body.temp(result_ty.clone(), expr.span);
                    result_local = Some(l);
                    l
                }
            };
            assign_into(ctx, then_block.span, local, then_op, result_ty.clone());
        }
        goto(ctx, then_block.span, join_bb);
    }

    ctx.current_bb = Some(else_bb);
    let else_op = match else_branch {
        Some(branch) => lower_expr_to_operand(ctx, branch),
        None => Operand::Unit,
    };
    let else_span = else_branch.map(|e| e.span).unwrap_or(expr.span);
    if ctx.current_bb.is_some() {
        any_branch_joins = true;
        if needs_value {
            let local = match result_local {
                Some(l) => l,
                None => {
                    let body = ctx.body.as_mut().expect("body present in lower_if");
                    let l = body.temp(result_ty.clone(), expr.span);
                    result_local = Some(l);
                    l
                }
            };
            assign_into(ctx, else_span, local, else_op, result_ty.clone());
        }
        goto(ctx, else_span, join_bb);
    }

    if !any_branch_joins {
        // Join is unreachable: seal it Unreachable and clear current_bb
        // so callers know the if-expression diverges. The returned
        // operand is irrelevant because no consumer will read it.
        ctx.current_bb = Some(join_bb);
        seal_terminator(
            ctx,
            Terminator {
                span: expr.span,
                kind: TerminatorKind::Unreachable,
            },
        );
        return Operand::Unit;
    }

    ctx.current_bb = Some(join_bb);
    match result_local {
        Some(local) => Operand::Move(Place::local(local)),
        None => Operand::Unit,
    }
}

/// Lower `match scrutinee { arms... }` as a chain of pattern tests.
pub(super) fn lower_match(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    scrutinee: &HirExpr,
    arms: &[HirMatchArm],
) -> Operand {
    let subject_op = lower_expr_to_operand(ctx, scrutinee);
    let result_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, expr.ty);
    let result_local = match ctx.body.as_mut() {
        Some(body) => body.temp(result_ty.clone(), expr.span),
        None => return Operand::Unit,
    };
    let join_bb = match alloc_block(ctx) {
        Some(bb) => bb,
        None => return Operand::Unit,
    };
    // Pre-initialise the result slot to a defined zero before lowering
    // the arms. The "no arm matched" fall-through reaches the join
    // without writing `result_local`; without this seed that path would
    // leave the slot `undef`, and LLVM's -O2 join-phi exploits the undef
    // to corrupt aggregate results — e.g. a `{ptr,len}` String built in
    // an arm body acquires a garbage length.
    // Value-bearing arms overwrite the seed; the dead/unmatched path
    // keeps the defined zero. Unit / Never results carry no slot to seed.
    if !matches!(result_ty.kind, MirTypeKind::Unit | MirTypeKind::Never) {
        let zero_id = ctx.program.push_const(Const {
            ty: result_ty.clone(),
            value: ConstValue::Zero,
        });
        assign_into(
            ctx,
            expr.span,
            result_local,
            Operand::Const(zero_id),
            result_ty.clone(),
        );
    }
    let mut next_subject = subject_op;
    for arm in arms.iter() {
        let body_bb = match alloc_block(ctx) {
            Some(bb) => bb,
            None => return Operand::Unit,
        };
        let miss_bb = match alloc_block(ctx) {
            Some(bb) => bb,
            None => return Operand::Unit,
        };
        super::pattern::lower_pattern_test(ctx, &arm.pat, next_subject.clone(), body_bb, miss_bb);
        ctx.current_bb = Some(body_bb);
        // Per-arm `where` guard: when present, evaluate the guard inside
        // the pattern-match success block. A truthy guard proceeds to the
        // arm body; a falsy guard rejoins the miss path so the next arm
        // is tried (matching the semantics of an arm whose pattern did
        // not match in the first place).
        if let Some(arm_body_bb) = lower_match_arm_guard(ctx, arm, miss_bb) {
            ctx.current_bb = Some(arm_body_bb);
            let arm_op = lower_expr_to_operand(ctx, &arm.body);
            assign_into(ctx, arm.body.span, result_local, arm_op, result_ty.clone());
            if ctx.current_bb.is_some() {
                goto(ctx, arm.body.span, join_bb);
            }
        }
        ctx.current_bb = Some(miss_bb);
        next_subject = Operand::Copy(scrutinee_place(&next_subject));
    }
    // The "no arm matched" fall-through reaches `join_bb` without
    // assigning `result_local`. Because `result_local` is pre-initialised
    // to a defined zero above, this path leaves a defined value rather
    // than the `undef` that LLVM exploits at -O2 — which was corrupting
    // aggregate results such as a `{ptr,len}` String built in the arm
    // bodies. The `goto join_bb`
    // continuation is preserved: matches the typechecker accepts but
    // that are non-exhaustive at runtime (guard rejection, or a gap in
    // exhaustiveness checking) keep flowing through the join instead of
    // hitting an `Unreachable` (UB) or `Panic` (behavioural regression).
    if ctx.current_bb.is_some() {
        goto(ctx, expr.span, join_bb);
    }
    ctx.current_bb = Some(join_bb);
    Operand::Move(Place::local(result_local))
}

/// Lower the `where <expr>` guard of a match arm, if present. The guard
/// is evaluated in the pattern-match success block (so pattern bindings
/// from `case let v ...` are in scope). A falsy guard branches to
/// `on_miss`; a truthy guard branches to a fresh arm-body block whose
/// `BlockId` is returned for the caller to position into.
fn lower_match_arm_guard(
    ctx: &mut LoweringContext<'_>,
    arm: &HirMatchArm,
    on_miss: crate::ids::BlockId,
) -> Option<crate::ids::BlockId> {
    let Some(guard_expr) = arm.guard.as_ref() else {
        return ctx.current_bb;
    };
    let guard_op = lower_expr_to_operand(ctx, guard_expr);
    // Guard lowering may diverge if the guard expression diverges
    // (rare but the HIR admits it). In that case `current_bb` is
    // already cleared and we have nothing to seal.
    ctx.current_bb?;
    let arm_body_bb = alloc_block(ctx)?;
    seal_switch_bool(ctx, guard_expr.span, guard_op, arm_body_bb, on_miss);
    Some(arm_body_bb)
}

/// Strip an [`Operand`] back to a [`Place`] for re-use as the next ladder
/// subject. Constants / units have no place; fall back to the return slot,
/// which carries the right shape for the placeholder operand.
fn scrutinee_place(op: &Operand) -> Place {
    match op {
        Operand::Copy(p) | Operand::Move(p) => p.clone(),
        Operand::Const(_) | Operand::Unit => Place::local(LocalId::RETURN_SLOT),
    }
}

/// Lower `loop { ... }` — the body falls back to the continue block; the
/// loop's value reads from the per-frame `loop_value` temp.
pub(super) fn lower_loop(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    body: &HirBlock,
) -> Operand {
    let result_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, expr.ty);
    let loop_value = match ctx.body.as_mut() {
        Some(b) => b.temp(result_ty.clone(), expr.span),
        None => return Operand::Unit,
    };
    let (continue_bb, break_bb) = match (alloc_block(ctx), alloc_block(ctx)) {
        (Some(c), Some(b)) => (c, b),
        _ => return Operand::Unit,
    };
    goto(ctx, expr.span, continue_bb);
    ctx.loop_stack.push(LoopFrame {
        continue_bb,
        break_bb,
        loop_value: Some(loop_value),
    });
    ctx.current_bb = Some(continue_bb);
    let _ = lower_block_expr(ctx, body);
    if ctx.current_bb.is_some() {
        goto(ctx, body.span, continue_bb);
    }
    ctx.loop_stack.pop();
    ctx.current_bb = Some(break_bb);
    Operand::Move(Place::local(loop_value))
}

/// Lower `return [expr]` — write the value into the body's return slot,
/// then seal with `Return(Move(return_slot))`. For raising functions the
/// success value is first wrapped in the Ok variant of the synthesized
/// `Result<T, E>` sum ADT.
pub(super) fn lower_return(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    value: Option<&HirExpr>,
) {
    let return_slot = match ctx.return_slot {
        Some(local) => local,
        None => {
            ctx.errors.push(MirError::from(LoweringError::InternalError {
                message: "return without a return-slot local".to_string(),
                span,
            }));
            return;
        }
    };
    // Return-position borrow: store the address of the returned place
    // into the HeapPtr return slot instead of copying its value.
    if try_lower_borrow_return(ctx, span, value) {
        seal_terminator(
            ctx,
            Terminator {
                span,
                kind: TerminatorKind::Return(Operand::Move(Place::local(return_slot))),
            },
        );
        return;
    }
    let op = match value {
        Some(expr) => lower_expr_to_operand(ctx, expr),
        None => Operand::Unit,
    };
    let return_ty = match ctx.body.as_ref() {
        Some(body) => body.body_ref().return_ty.clone(),
        None => return,
    };
    let final_op = wrap_ok_if_raising(ctx, span, op);
    assign_into(ctx, span, return_slot, final_op, return_ty);
    seal_terminator(
        ctx,
        Terminator {
            span,
            kind: TerminatorKind::Return(Operand::Move(Place::local(return_slot))),
        },
    );
}

/// Whether a trailing expression is a bare-place tail (`{ o.inner }`)
/// eligible for borrow-return address-of lowering. A `return <place>`
/// tail is a `HirExprKind::Return` (handled by [`lower_return`]); only
/// the keyword-less place forms route through the trailing-expression
/// borrow path.
pub(super) fn is_borrow_place_tail(kind: &HirExprKind) -> bool {
    matches!(
        kind,
        HirExprKind::Path(_)
            | HirExprKind::Field { .. }
            | HirExprKind::TupleIndex { .. }
            | HirExprKind::Index { .. }
            | HirExprKind::Call { .. }
            | HirExprKind::MethodCall { .. }
    )
}

//            `Some`) takes the address; by-value returns short-circuit to
//            `false` so the caller keeps its value path
//            parameter — guaranteed by the return-borrow region check in
//            `edda-types`; a non-place form would have been rejected
//            before lowering
/// For a return-position borrow function, write the *address* of the
/// returned place into the (HeapPtr) return slot via `RvalueKind::Ref`
/// and return `true`. Returns `false` for by-value returns so the caller
/// falls back to its value path.
pub(super) fn try_lower_borrow_return(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    value: Option<&HirExpr>,
) -> bool {
    if ctx.return_borrow_pointee.is_none() {
        return false;
    }
    let Some(return_slot) = ctx.return_slot else {
        return false;
    };
    let Some(expr) = value else {
        return false;
    };
    let Some(place) = super::stmt::resolve_place(ctx, expr) else {
        // resolve_place already pushed a diagnostic; treat as handled so
        // the caller does not also emit a value-path store.
        return true;
    };
    push_assign(
        ctx,
        span,
        return_slot,
        Rvalue {
            span,
            kind: RvalueKind::Ref { place },
            ty: MirType::prim(MirPrim::HeapPtr),
        },
    );
    true
}

/// If the current function is raising, wrap `op` in the Ok variant of the
/// synthesized `Result<T, E>` sum ADT. Otherwise return `op` unchanged.
fn wrap_ok_if_raising(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    op: Operand,
) -> Operand {
    let Some((result_adt_id, ok_idx)) = ctx.result_adt else {
        return op;
    };
    let ok_has_payload = ctx
        .program
        .program()
        .adts
        .get(result_adt_id)
        .and_then(|adt| adt.variants.get(ok_idx.as_index()))
        .map(|v| !v.fields.is_empty())
        .unwrap_or(true);
    let fields = if ok_has_payload { vec![op] } else { Vec::new() };
    let ok_ty = MirType::new(MirTypeKind::Adt(result_adt_id));
    let ok_rvalue = Rvalue {
        span,
        kind: RvalueKind::MakeVariant {
            adt: result_adt_id,
            variant: ok_idx,
            fields,
        },
        ty: ok_ty.clone(),
    };
    let tmp = match ctx.body.as_mut() {
        Some(b) => b.temp(ok_ty, span),
        None => return Operand::Unit,
    };
    push_assign(ctx, span, tmp, ok_rvalue);
    Operand::Move(Place::local(tmp))
}

/// Lower `break [expr]` — write the value (if any) into the active loop
/// frame's `loop_value` local, then `Goto(break_bb)`.
pub(super) fn lower_break(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    value: Option<&HirExpr>,
) {
    let Some(frame) = ctx.loop_stack.last() else {
        ctx.errors.push(MirError::from(LoweringError::BreakOutsideLoop { span }));
        return;
    };
    let break_bb = frame.break_bb;
    let loop_value = frame.loop_value;
    if let Some(expr) = value {
        if let Some(slot) = loop_value {
            let op = lower_expr_to_operand(ctx, expr);
            let ty = lower_ty(ctx.ty_interner, &ctx.adt_map, expr.ty);
            assign_into(ctx, span, slot, op, ty);
        } else {
            let _ = lower_expr_to_operand(ctx, expr);
        }
    }
    goto(ctx, span, break_bb);
}

/// Lower `continue` — `Goto(continue_bb)`.
pub(super) fn lower_continue(ctx: &mut LoweringContext<'_>, span: Span) {
    let Some(frame) = ctx.loop_stack.last() else {
        ctx.errors.push(MirError::from(LoweringError::ContinueOutsideLoop { span }));
        return;
    };
    let target = frame.continue_bb;
    goto(ctx, span, target);
}

/// Lower `panic msg` — seal with `Terminator::Panic { msg }`.
pub(super) fn lower_panic(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    msg: &HirExpr,
) {
    let msg_op = lower_expr_to_operand(ctx, msg);
    seal_terminator(
        ctx,
        Terminator {
            span,
            kind: TerminatorKind::Panic { msg: msg_op },
        },
    );
}

/// Recognise diverging expression kinds whose lowering already sealed the
/// current block (so callers know not to expect a value).
#[allow(dead_code)]
pub(super) fn is_diverging(kind: &HirExprKind) -> bool {
    matches!(
        kind,
        HirExprKind::Return(_)
            | HirExprKind::Break { .. }
            | HirExprKind::Continue { .. }
            | HirExprKind::Panic(_)
    )
}
