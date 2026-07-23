//! Arithmetic and short-circuit expression lowering.
//!
//! Binary / unary arithmetic, `&&` / `||` short-circuit, and the wrapping +
//! checked overflow CFG synthesis for `+?` / `-?` / `*?` per
//! `spec-sweep-locks.md` S1.

use edda_syntax::ast::{BinOp as AstBinOp, UnOp as AstUnOp};
use edda_types::{HirExpr, TyId, TyKind};

use crate::constant::{Const, ConstValue};
use crate::ids::BodyId;
use crate::operand::Operand;
use crate::place::Place;
use crate::rvalue::{BinOp, Rvalue, RvalueKind, UnOp};
use crate::terminator::{CallArg, CallMode, FuncRef, Terminator, TerminatorKind};
use crate::ty::{MirPrim, MirType};

use super::cfg::{alloc_block, assign_into, goto, push_assign, seal_switch_bool, seal_terminator};
use super::ctx::LoweringContext;
use super::expr::{lower_expr_to_operand, unsupported_and_unit};
use super::overflow;
use super::ty::{lower_ty, ty_to_prim};

/// Lower a binary expression to a fresh temp populated with the operation's
/// result.
pub(super) fn lower_binary(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    op: AstBinOp,
    lhs: &HirExpr,
    rhs: &HirExpr,
) -> Operand {
    if matches!(op, AstBinOp::And | AstBinOp::Or) {
        return lower_short_circuit(ctx, expr, op, lhs, rhs);
    }
    if matches!(
        op,
        AstBinOp::CheckAdd | AstBinOp::CheckSub | AstBinOp::CheckMul
    ) {
        return overflow::lower_checked_arith(ctx, expr, op, lhs, rhs);
    }
    if matches!(op, AstBinOp::CheckMod) {
        return overflow::lower_checked_mod(ctx, expr, lhs, rhs);
    }
    if matches!(op, AstBinOp::Add | AstBinOp::Sub | AstBinOp::Mul) {
        match ty_to_prim(ctx.ty_interner, lhs.ty) {
            Some(
                prim @ (MirPrim::I8
                | MirPrim::I16
                | MirPrim::I32
                | MirPrim::I64
                | MirPrim::I128
                | MirPrim::Isize
                | MirPrim::U8
                | MirPrim::U16
                | MirPrim::U32
                | MirPrim::U64
                | MirPrim::U128
                | MirPrim::Usize),
            ) => return overflow::lower_trapping_arith(ctx, expr, op, lhs, rhs, prim),
            _ => {}
        }
    }
    let lhs_op = lower_expr_to_operand(ctx, lhs);
    let rhs_op = lower_expr_to_operand(ctx, rhs);
    let result_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, expr.ty);
    // BinOp lowering is currently primitive-only: the backend's
    // `lower_binop` keys on a `MirPrim`. Non-primitive operand types
    // (sums / products / slices) have no scalar lowering yet —
    // structural equality on sums per `refinements.md` (*Equality is
    // structural*) and pointer-equality on slices are spec-admitted
    // but not yet implemented. Surface a controlled lowering error
    // rather than fall through to an ill-typed `BinOp` that would
    // panic the LLVM emitter. The diagnostic tag distinguishes the
    // shape of the operand so users get a pointer to the right
    // workaround (e.g. write a per-T equality helper instead of
    // relying on generic `==`).
    let Some(prim) = ty_to_prim(ctx.ty_interner, lhs.ty) else {
        // `==` / `!=` on a `derive eq` nominal operand lowers to a `Call`
        // into the synthesised structural comparator.
        // `lower_nominal_eq` returns `None`
        // for non-`Eq`/`Ne` ops, non-nominal operands, and nominals whose
        // type is absent from the comparator map — those fall through to
        // the controlled lowering error below.
        if let Some(result) = lower_nominal_eq(ctx, expr.span, op, lhs.ty, lhs_op.clone(), rhs_op.clone()) {
            return result;
        }
        // `==` / `!=` on a slice lowers to an inline length-check +
        // element-compare loop: a
        // primitive element compares inline, a `derive eq` nominal
        // element (record / sum) delegates to its comparator. `lower_slice_eq`
        // returns `None` for non-`Eq`/`Ne` ops, non-slice operands, and
        // slices whose element type has neither lowering (e.g. `[[T]]`,
        // tuple elements) — those fall through to the controlled lowering
        // error below.
        if let Some(result) = lower_slice_eq(ctx, expr.span, op, lhs.ty, lhs_op.clone(), rhs_op.clone()) {
            return result;
        }
        let tag = match ctx.ty_interner.kind(lhs.ty) {
            TyKind::Nominal(_) => "BinOp on sum/record operand (structural equality not yet lowered)",
            TyKind::Slice(_) => "BinOp on slice operand (pointer/structural equality not yet lowered)",
            TyKind::Tuple(_) => "BinOp on tuple operand (structural equality not yet lowered)",
            _ => "BinOp on non-primitive operand",
        };
        return unsupported_and_unit(ctx, expr.span, tag);
    };
    let temp = match ctx.body.as_mut() {
        Some(body) => body.temp(result_ty.clone(), expr.span),
        None => return Operand::Unit,
    };
    let rvalue = Rvalue {
        span: expr.span,
        kind: RvalueKind::BinOp {
            op: map_binop(op),
            lhs: lhs_op,
            rhs: rhs_op,
            prim,
        },
        ty: result_ty,
    };
    push_assign(ctx, expr.span, temp, rvalue);
    Operand::Move(Place::local(temp))
}

/// Lower `==` / `!=` on a `derive eq` nominal operand to a `Call` into
/// the synthesised `std.core.compare.eq_<T>.eq(a, b) -> bool` comparator.
/// `lhs_op` / `rhs_op` are the operands
/// the caller already lowered. Returns `None` (the unused operands are
/// dropped) when `op` is not `==`/`!=`, the operand is not nominal, or
/// the type has no registered comparator.
fn lower_nominal_eq(
    ctx: &mut LoweringContext<'_>,
    span: edda_span::Span,
    op: AstBinOp,
    lhs_ty: TyId,
    lhs_op: Operand,
    rhs_op: Operand,
) -> Option<Operand> {
    if !matches!(op, AstBinOp::Eq | AstBinOp::Ne) {
        return None;
    }
    let TyKind::Nominal(type_binding) = ctx.ty_interner.kind(lhs_ty) else {
        return None;
    };
    let type_binding = *type_binding;
    let cmp_binding = ctx.eq_comparators.get(&type_binding).copied()?;
    let body_id = ctx.function_map.get(&cmp_binding).copied()?;

    let bool_ty = MirType::prim(MirPrim::Bool);
    let dest = ctx.body.as_mut()?.temp(bool_ty.clone(), span);
    let post_bb = alloc_block(ctx)?;
    seal_terminator(
        ctx,
        Terminator {
            span,
            kind: TerminatorKind::Call {
                func: FuncRef::Body(body_id),
                args: vec![
                    CallArg {
                        mode: CallMode::Read,
                        operand: lhs_op,
                    },
                    CallArg {
                        mode: CallMode::Read,
                        operand: rhs_op,
                    },
                ],
                capabilities: Vec::new(),
                destination: Place::local(dest),
                target: post_bb,
                on_error: None,
            },
        },
    );
    ctx.current_bb = Some(post_bb);

    if matches!(op, AstBinOp::Ne) {
        let negated = ctx.body.as_mut()?.temp(bool_ty.clone(), span);
        push_assign(
            ctx,
            span,
            negated,
            Rvalue {
                span,
                kind: RvalueKind::UnOp {
                    op: UnOp::Not,
                    arg: Operand::Move(Place::local(dest)),
                    prim: MirPrim::Bool,
                },
                ty: bool_ty,
            },
        );
        return Some(Operand::Move(Place::local(negated)));
    }
    Some(Operand::Move(Place::local(dest)))
}

/// How a single slice element pair is compared for equality inside the
/// [`lower_slice_eq`] loop body.
enum ElemEq {
    /// Scalar element: an inline `BinOp::Eq` at this `MirPrim`.
    Prim(MirPrim),
    /// Nominal element with a registered `derive eq` comparator: a
    /// `Call` into that comparator body (parallels [`lower_nominal_eq`]).
    Comparator(BodyId),
}

/// Resolve how the slice-eq loop should compare two elements of type
/// `elem_ty`. A primitive element uses an inline `BinOp::Eq`; a nominal
/// element delegates to its `derive eq` comparator (the same map
/// [`lower_nominal_eq`] consults). Tuple and nested-slice elements have
/// no comparator yet and return `None`.
fn resolve_elem_eq(ctx: &LoweringContext<'_>, elem_ty: TyId) -> Option<ElemEq> {
    if let Some(prim) = ty_to_prim(ctx.ty_interner, elem_ty) {
        return Some(ElemEq::Prim(prim));
    }
    if let TyKind::Nominal(binding) = ctx.ty_interner.kind(elem_ty) {
        let cmp_binding = ctx.eq_comparators.get(binding).copied()?;
        let body_id = ctx.function_map.get(&cmp_binding).copied()?;
        return Some(ElemEq::Comparator(body_id));
    }
    None
}

/// Lower `==` / `!=` on a primitive-element slice operand to an inline
/// length-check + element-compare loop.
///
/// Layout (for `lhs == rhs`):
/// ```text
///   bb_pre:
///       result = true; i = 0
///       len_l = ArrayLen(lhs); len_r = ArrayLen(rhs)
///       len_eq = Eq(len_l, len_r)
///       SwitchBool(len_eq, true_bb: bb_head, false_bb: bb_neq)
///   bb_head:
///       cond = Lt(i, len_l)
///       SwitchBool(cond, true_bb: bb_body, false_bb: bb_join)
///   bb_body:
///       a_i = ArrayIndex(lhs, i); b_i = ArrayIndex(rhs, i)
///       elem_eq = Eq(a_i, b_i)
///       SwitchBool(elem_eq, true_bb: bb_incr, false_bb: bb_neq)
///   bb_incr:
///       i = Add(i, 1); Goto(bb_head)
///   bb_neq:
///       result = false; Goto(bb_join)
///   bb_join:
/// ```
/// `!=` negates `result` in the join. Returns `None` when `op` is not
/// `==`/`!=`, the operand is not a slice, or the element type has no
/// scalar lowering.
fn lower_slice_eq(
    ctx: &mut LoweringContext<'_>,
    span: edda_span::Span,
    op: AstBinOp,
    lhs_ty: TyId,
    lhs_op: Operand,
    rhs_op: Operand,
) -> Option<Operand> {
    if !matches!(op, AstBinOp::Eq | AstBinOp::Ne) {
        return None;
    }
    let TyKind::Slice(elem_ty) = ctx.ty_interner.kind(lhs_ty) else {
        return None;
    };
    let elem_ty = *elem_ty;
    let elem_eq = resolve_elem_eq(ctx, elem_ty)?;
    let elem_mir_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, elem_ty);

    let bool_ty = MirType::prim(MirPrim::Bool);
    let usize_ty = MirType::prim(MirPrim::Usize);
    let body = ctx.body.as_mut()?;
    let result_local = body.temp(bool_ty.clone(), span);
    let counter_local = body.temp(usize_ty.clone(), span);
    let len_l_local = body.temp(usize_ty.clone(), span);
    let len_r_local = body.temp(usize_ty.clone(), span);

    let true_id = ctx.program.push_const(Const {
        ty: bool_ty.clone(),
        value: ConstValue::Bool(true),
    });
    let false_id = ctx.program.push_const(Const {
        ty: bool_ty.clone(),
        value: ConstValue::Bool(false),
    });
    let zero_id = ctx.program.push_const(Const {
        ty: usize_ty.clone(),
        value: ConstValue::Uint(0),
    });
    let one_id = ctx.program.push_const(Const {
        ty: usize_ty.clone(),
        value: ConstValue::Uint(1),
    });

    assign_into(ctx, span, result_local, Operand::Const(true_id), bool_ty.clone());
    assign_into(ctx, span, counter_local, Operand::Const(zero_id), usize_ty.clone());
    push_assign(
        ctx,
        span,
        len_l_local,
        Rvalue {
            span,
            kind: RvalueKind::ArrayLen { array: lhs_op.clone() },
            ty: usize_ty.clone(),
        },
    );
    push_assign(
        ctx,
        span,
        len_r_local,
        Rvalue {
            span,
            kind: RvalueKind::ArrayLen { array: rhs_op.clone() },
            ty: usize_ty.clone(),
        },
    );
    let len_eq_local = ctx.body.as_mut()?.temp(bool_ty.clone(), span);
    push_assign(
        ctx,
        span,
        len_eq_local,
        Rvalue {
            span,
            kind: RvalueKind::BinOp {
                op: BinOp::Eq,
                lhs: Operand::Copy(Place::local(len_l_local)),
                rhs: Operand::Copy(Place::local(len_r_local)),
                prim: MirPrim::Usize,
            },
            ty: bool_ty.clone(),
        },
    );

    let bb_head = alloc_block(ctx)?;
    let bb_body = alloc_block(ctx)?;
    let bb_incr = alloc_block(ctx)?;
    let bb_neq = alloc_block(ctx)?;
    let bb_join = alloc_block(ctx)?;

    seal_switch_bool(
        ctx,
        span,
        Operand::Copy(Place::local(len_eq_local)),
        bb_head,
        bb_neq,
    );

    ctx.current_bb = Some(bb_head);
    let cond_local = ctx.body.as_mut()?.temp(bool_ty.clone(), span);
    push_assign(
        ctx,
        span,
        cond_local,
        Rvalue {
            span,
            kind: RvalueKind::BinOp {
                op: BinOp::Lt,
                lhs: Operand::Copy(Place::local(counter_local)),
                rhs: Operand::Copy(Place::local(len_l_local)),
                prim: MirPrim::Usize,
            },
            ty: bool_ty.clone(),
        },
    );
    seal_switch_bool(
        ctx,
        span,
        Operand::Copy(Place::local(cond_local)),
        bb_body,
        bb_join,
    );

    ctx.current_bb = Some(bb_body);
    let a_elem = ctx.body.as_mut()?.temp(elem_mir_ty.clone(), span);
    let b_elem = ctx.body.as_mut()?.temp(elem_mir_ty.clone(), span);
    push_assign(
        ctx,
        span,
        a_elem,
        Rvalue {
            span,
            kind: RvalueKind::ArrayIndex {
                array: lhs_op,
                idx: Operand::Copy(Place::local(counter_local)),
            },
            ty: elem_mir_ty.clone(),
        },
    );
    push_assign(
        ctx,
        span,
        b_elem,
        Rvalue {
            span,
            kind: RvalueKind::ArrayIndex {
                array: rhs_op,
                idx: Operand::Copy(Place::local(counter_local)),
            },
            ty: elem_mir_ty,
        },
    );
    let elem_eq_local = ctx.body.as_mut()?.temp(bool_ty.clone(), span);
    match elem_eq {
        ElemEq::Prim(elem_prim) => {
            push_assign(
                ctx,
                span,
                elem_eq_local,
                Rvalue {
                    span,
                    kind: RvalueKind::BinOp {
                        op: BinOp::Eq,
                        lhs: Operand::Move(Place::local(a_elem)),
                        rhs: Operand::Move(Place::local(b_elem)),
                        prim: elem_prim,
                    },
                    ty: bool_ty.clone(),
                },
            );
        }
        ElemEq::Comparator(body_id) => {
            // Delegate the element compare to the element type's `derive
            // eq` body (parallels `lower_nominal_eq`). The `Call` splits
            // the loop body: `elem_eq_local` is filled in `post_bb`,
            // which then drives the equal/not-equal switch below.
            let post_bb = alloc_block(ctx)?;
            seal_terminator(
                ctx,
                Terminator {
                    span,
                    kind: TerminatorKind::Call {
                        func: FuncRef::Body(body_id),
                        args: vec![
                            CallArg {
                                mode: CallMode::Read,
                                operand: Operand::Move(Place::local(a_elem)),
                            },
                            CallArg {
                                mode: CallMode::Read,
                                operand: Operand::Move(Place::local(b_elem)),
                            },
                        ],
                        capabilities: Vec::new(),
                        destination: Place::local(elem_eq_local),
                        target: post_bb,
                        on_error: None,
                    },
                },
            );
            ctx.current_bb = Some(post_bb);
        }
    }
    seal_switch_bool(
        ctx,
        span,
        Operand::Copy(Place::local(elem_eq_local)),
        bb_incr,
        bb_neq,
    );

    ctx.current_bb = Some(bb_incr);
    push_assign(
        ctx,
        span,
        counter_local,
        Rvalue {
            span,
            kind: RvalueKind::BinOp {
                op: BinOp::Add,
                lhs: Operand::Copy(Place::local(counter_local)),
                rhs: Operand::Const(one_id),
                prim: MirPrim::Usize,
            },
            ty: usize_ty,
        },
    );
    goto(ctx, span, bb_head);

    ctx.current_bb = Some(bb_neq);
    assign_into(ctx, span, result_local, Operand::Const(false_id), bool_ty.clone());
    goto(ctx, span, bb_join);

    ctx.current_bb = Some(bb_join);
    if matches!(op, AstBinOp::Ne) {
        let negated = ctx.body.as_mut()?.temp(bool_ty.clone(), span);
        push_assign(
            ctx,
            span,
            negated,
            Rvalue {
                span,
                kind: RvalueKind::UnOp {
                    op: UnOp::Not,
                    arg: Operand::Move(Place::local(result_local)),
                    prim: MirPrim::Bool,
                },
                ty: bool_ty,
            },
        );
        return Some(Operand::Move(Place::local(negated)));
    }
    Some(Operand::Move(Place::local(result_local)))
}

/// Lower a short-circuiting `&&` or `||` as a `SwitchBool` over the lhs
/// followed by a join block reading the result local.
///
/// Layout (for `lhs && rhs`):
/// ```text
///   bb_pre:
///       r = lhs_value
///       SwitchBool(Copy(r), true_bb: bb_rhs, false_bb: bb_join)
///   bb_rhs:
///       r = rhs_value
///       Goto(bb_join)
///   bb_join:
/// ```
/// For `||` the branches are swapped: `true_bb: bb_join, false_bb: bb_rhs`.
fn lower_short_circuit(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    op: AstBinOp,
    lhs: &HirExpr,
    rhs: &HirExpr,
) -> Operand {
    let bool_ty = MirType::prim(MirPrim::Bool);
    let result_local = match ctx.body.as_mut() {
        Some(body) => body.temp(bool_ty.clone(), expr.span),
        None => return Operand::Unit,
    };
    let lhs_op = lower_expr_to_operand(ctx, lhs);
    if ctx.current_bb.is_none() {
        // lhs diverged — no fall-through block exists for the
        // short-circuit dispatch. Return the placeholder operand so the
        // caller's surrounding control flow continues to follow the
        // diverging trail without us spawning an unreachable join.
        return Operand::Unit;
    }
    assign_into(ctx, lhs.span, result_local, lhs_op, bool_ty.clone());
    let (bb_rhs, bb_join) = match (alloc_block(ctx), alloc_block(ctx)) {
        (Some(r), Some(j)) => (r, j),
        _ => return Operand::Unit,
    };
    let (true_bb, false_bb) = match op {
        AstBinOp::And => (bb_rhs, bb_join),
        AstBinOp::Or => (bb_join, bb_rhs),
        _ => unreachable!("lower_short_circuit called with non-And/Or operator"),
    };
    seal_switch_bool(
        ctx,
        expr.span,
        Operand::Copy(Place::local(result_local)),
        true_bb,
        false_bb,
    );
    ctx.current_bb = Some(bb_rhs);
    let rhs_op = lower_expr_to_operand(ctx, rhs);
    if ctx.current_bb.is_some() {
        assign_into(ctx, rhs.span, result_local, rhs_op, bool_ty);
        goto(ctx, rhs.span, bb_join);
    }
    ctx.current_bb = Some(bb_join);
    Operand::Move(Place::local(result_local))
}

/// Lower a unary expression to a fresh temp populated with the operation's
/// result.
pub(super) fn lower_unary(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    op: AstUnOp,
    inner: &HirExpr,
) -> Operand {
    let arg = lower_expr_to_operand(ctx, inner);
    let result_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, expr.ty);
    let prim = ty_to_prim(ctx.ty_interner, inner.ty).unwrap_or(MirPrim::I64);
    let temp = match ctx.body.as_mut() {
        Some(body) => body.temp(result_ty.clone(), expr.span),
        None => return Operand::Unit,
    };
    let rvalue = Rvalue {
        span: expr.span,
        kind: RvalueKind::UnOp {
            op: map_unop(op),
            arg,
            prim,
        },
        ty: result_ty,
    };
    push_assign(ctx, expr.span, temp, rvalue);
    Operand::Move(Place::local(temp))
}

/// `ast::BinOp` -> `mir::BinOp`. Variant names match 1:1.
fn map_binop(op: AstBinOp) -> BinOp {
    match op {
        AstBinOp::Add => BinOp::Add,
        AstBinOp::Sub => BinOp::Sub,
        AstBinOp::Mul => BinOp::Mul,
        AstBinOp::Div => BinOp::Div,
        AstBinOp::Mod => BinOp::Mod,
        AstBinOp::WrapAdd => BinOp::WrapAdd,
        AstBinOp::WrapSub => BinOp::WrapSub,
        AstBinOp::WrapMul => BinOp::WrapMul,
        AstBinOp::SatAdd => BinOp::SatAdd,
        AstBinOp::SatSub => BinOp::SatSub,
        AstBinOp::SatMul => BinOp::SatMul,
        // Checked variants are intercepted by `lower_binary` before
        // reaching this table; they have no MIR `BinOp` counterpart.
        AstBinOp::CheckAdd | AstBinOp::CheckSub | AstBinOp::CheckMul | AstBinOp::CheckMod => {
            unreachable!("checked-arithmetic ops are routed before map_binop")
        }
        AstBinOp::Eq => BinOp::Eq,
        AstBinOp::Ne => BinOp::Ne,
        AstBinOp::Lt => BinOp::Lt,
        AstBinOp::Le => BinOp::Le,
        AstBinOp::Gt => BinOp::Gt,
        AstBinOp::Ge => BinOp::Ge,
        // `&&` / `||` are dispatched to `lower_short_circuit` before they
        // reach this table — the mappings below are unreachable in practice
        // but retained for completeness so the catalogue stays exhaustive.
        AstBinOp::And => BinOp::And,
        AstBinOp::Or => BinOp::Or,
        AstBinOp::BitAnd => BinOp::BitAnd,
        AstBinOp::BitOr => BinOp::BitOr,
        AstBinOp::BitXor => BinOp::BitXor,
        AstBinOp::Shl => BinOp::Shl,
        AstBinOp::Shr => BinOp::Shr,
    }
}

/// `ast::UnOp` -> `mir::UnOp`. Variant names match 1:1.
fn map_unop(op: AstUnOp) -> UnOp {
    match op {
        AstUnOp::Neg => UnOp::Neg,
        AstUnOp::Not => UnOp::Not,
        AstUnOp::BitNot => UnOp::BitNot,
    }
}
