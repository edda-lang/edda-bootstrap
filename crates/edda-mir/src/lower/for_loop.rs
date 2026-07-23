//! `for pat in iter { body }` lowering — desugar to indexed loop.
//!
//! Per `corpus/edda-codex/language/01-syntax.md` §7, `for` is a
//! statement form: the surrounding expression's value is always `()`.
//! Two iter shapes are currently admitted, which the backend can lower today:
//!
//! - **Range** (`a..<b`, `a..=b`) — desugar to an indexed loop where
//!   the counter is the binder, the bound check uses the high
//!   endpoint, and the post-iteration increment advances the counter
//!   by 1. The implicit `Range_<T>` spec invocation registered by the
//!   typechecker is not consumed at this layer — the desugar bypasses
//!   the runtime iterator protocol entirely so the spec
//!   materialisation is irrelevant to the bound-form.
//! - **Slice** (`xs` where `xs: [T]`) — desugar to an indexed loop
//!   over `0..xs.len()` with the binder rebound to `xs[i]` at the top
//!   of each iteration.
//!
//! Any other iter shape (open-ended ranges, non-slice value types)
//! surfaces `UnsupportedHirVariant` so users see the gap at lower time
//! rather than at runtime.
//!
//! # Termination
//!
//! Both desugarings produce a loop whose continue block always observes
//! a strictly decreasing distance to the bound. The MIR layer does not
//! re-prove termination — that obligation was discharged at the
//! source-form level by the typechecker's `for`-loop typing rule. The
//! desugared `loop` carries no `decreases` clause because MIR has no
//! such notion; the resulting CFG is structurally bounded by the
//! counter / length comparison.

use edda_span::Span;
use edda_syntax::ast::RangeKind;
use edda_types::{HirBlock, HirExpr, HirExprKind, HirPat, HirPatKind, TyKind};

use crate::body::Mutability;
use crate::constant::{Const, ConstValue};
use crate::ids::LocalId;
use crate::operand::Operand;
use crate::place::Place;
use crate::rvalue::{BinOp, Rvalue, RvalueKind};
use crate::ty::{MirPrim, MirType};

use super::cfg::{alloc_block, assign_into, goto, push_assign, seal_switch_bool};
use super::ctx::{LoopFrame, LoweringContext};
use super::expr::{lower_block_expr, lower_expr_to_operand, unsupported_and_unit};
use super::ty::{lower_ty, ty_to_prim};

/// Lower `for pat in iter { body }`. Dispatches on the iter's HIR shape
/// (range vs slice value) and emits the indexed-loop CFG.
pub(super) fn lower_for(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    pat: &HirPat,
    iter: &HirExpr,
    body: &HirBlock,
) -> Operand {
    if let HirExprKind::Range { lo: Some(lo), hi: Some(hi), kind } = &iter.kind {
        return lower_for_range(ctx, expr.span, pat, lo, hi, *kind, body);
    }
    if let TyKind::Slice(_) = ctx.ty_interner.kind(iter.ty) {
        return lower_for_slice(ctx, expr.span, pat, iter, body);
    }
    unsupported_and_unit(
        ctx,
        expr.span,
        "For (iter must be a closed range or a slice)",
    )
}

/// Range form: `for i in lo..<hi { body }` (or `..=`).
///
/// Lowers to:
/// ```text
///   <i materialised as a fresh user local, seeded from lo>
///   Goto(header_bb)
/// header_bb:
///   tmp_cmp = i CMP hi      // < for HalfOpen, <= for Closed
///   SwitchBool(tmp_cmp, body_bb, break_bb)
/// body_bb:
///   <body>          // `continue` jumps to latch_bb
///   Goto(latch_bb)
/// latch_bb:
///   i = i + 1
///   Goto(header_bb)
/// break_bb:
/// ```
fn lower_for_range(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    pat: &HirPat,
    lo: &HirExpr,
    hi: &HirExpr,
    kind: RangeKind,
    body: &HirBlock,
) -> Operand {
    let HirPatKind::Binding(ident) = &pat.kind else {
        return unsupported_and_unit(ctx, pat.span, "For pat (range form must be a bare binder)");
    };
    let counter_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, lo.ty);
    let Some(prim) = ty_to_prim(ctx.ty_interner, lo.ty) else {
        return unsupported_and_unit(ctx, lo.span, "For (range endpoint type must be primitive)");
    };
    let lo_op = lower_expr_to_operand(ctx, lo);
    let hi_op = lower_expr_to_operand(ctx, hi);
    let (counter_local, hi_local) = match ctx.body.as_mut() {
        Some(b) => (
            b.user_local(ident.name, Mutability::Mut, counter_ty.clone(), pat.span),
            b.temp(counter_ty.clone(), hi.span),
        ),
        None => return Operand::Unit,
    };
    assign_into(ctx, span, counter_local, lo_op, counter_ty.clone());
    assign_into(ctx, span, hi_local, hi_op, counter_ty.clone());
    let cmp_op = match kind {
        RangeKind::HalfOpen => BinOp::Lt,
        RangeKind::Closed => BinOp::Le,
    };
    run_indexed_loop(
        ctx,
        IndexedLoop {
            span,
            binder: ident.name,
            counter_local,
            counter_ty: counter_ty.clone(),
            prim,
            bound_local: hi_local,
            cmp_op,
            elem: None,
            body,
        },
    )
}

/// Slice form: `for x in xs { body }` where `xs.ty = [T]`.
///
/// Lowers to:
/// ```text
///   xs_op = <iter>
///   len = array_len(xs_op)
///   i: usize = 0
///   Goto(header_bb)
/// header_bb:
///   tmp_cmp = i < len
///   SwitchBool(tmp_cmp, body_bb, break_bb)
/// body_bb:
///   x = xs_op[i]
///   <body>          // `continue` jumps to latch_bb
///   Goto(latch_bb)
/// latch_bb:
///   i = i + 1
///   Goto(header_bb)
/// break_bb:
/// ```
fn lower_for_slice(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    pat: &HirPat,
    iter: &HirExpr,
    body: &HirBlock,
) -> Operand {
    let HirPatKind::Binding(ident) = &pat.kind else {
        return unsupported_and_unit(ctx, pat.span, "For pat (slice form must be a bare binder)");
    };
    let TyKind::Slice(elem_ty_id) = ctx.ty_interner.kind(iter.ty) else {
        return unsupported_and_unit(ctx, iter.span, "For slice (iter is not a slice)");
    };
    let elem_ty_id = *elem_ty_id;
    let elem_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, elem_ty_id);
    let usize_ty = MirType::prim(MirPrim::Usize);
    let iter_op = lower_expr_to_operand(ctx, iter);
    let (counter_local, len_local, binder_local) = match ctx.body.as_mut() {
        Some(b) => (
            b.temp(usize_ty.clone(), span),
            b.temp(usize_ty.clone(), iter.span),
            b.user_local(ident.name, Mutability::Imm, elem_ty.clone(), pat.span),
        ),
        None => return Operand::Unit,
    };
    // i = 0
    let zero_id = ctx.program.push_const(Const {
        ty: usize_ty.clone(),
        value: ConstValue::Uint(0),
    });
    assign_into(ctx, span, counter_local, Operand::Const(zero_id), usize_ty.clone());
    // len = array_len(xs)
    push_assign(
        ctx,
        iter.span,
        len_local,
        Rvalue {
            span: iter.span,
            kind: RvalueKind::ArrayLen { array: iter_op.clone() },
            ty: usize_ty.clone(),
        },
    );
    run_indexed_loop(
        ctx,
        IndexedLoop {
            span,
            binder: ident.name,
            counter_local,
            counter_ty: usize_ty,
            prim: MirPrim::Usize,
            bound_local: len_local,
            cmp_op: BinOp::Lt,
            elem: Some(SliceElem {
                source: iter_op,
                binder_local,
                elem_ty,
            }),
            body,
        },
    )
}

/// Shared loop-CFG emitter for both `for` desugarings.
struct IndexedLoop<'h> {
    span: Span,
    binder: edda_intern::Symbol,
    counter_local: LocalId,
    counter_ty: MirType,
    prim: MirPrim,
    bound_local: LocalId,
    cmp_op: BinOp,
    elem: Option<SliceElem>,
    body: &'h HirBlock,
}

/// Slice-form per-iteration element binding state. `binder_local`
/// holds `source[counter]` at the top of each iteration.
struct SliceElem {
    source: Operand,
    binder_local: LocalId,
    elem_ty: MirType,
}

/// Emit the header / body / latch / break CFG given a fully-initialised
/// counter and bound. Used by both `lower_for_range` and
/// `lower_for_slice`.
fn run_indexed_loop(ctx: &mut LoweringContext<'_>, plan: IndexedLoop<'_>) -> Operand {
    let (header_bb, body_bb, latch_bb, break_bb) = match (
        alloc_block(ctx),
        alloc_block(ctx),
        alloc_block(ctx),
        alloc_block(ctx),
    ) {
        (Some(h), Some(b), Some(l), Some(k)) => (h, b, l, k),
        _ => return Operand::Unit,
    };
    goto(ctx, plan.span, header_bb);
    ctx.current_bb = Some(header_bb);
    let cmp_op = emit_loop_cmp(ctx, &plan);
    seal_switch_bool(ctx, plan.span, cmp_op, body_bb, break_bb);

    ctx.current_bb = Some(body_bb);
    bind_slice_elem(ctx, &plan);
    ctx.bindings.insert(plan.binder, body_binder_local(&plan));
    // `continue` targets the latch, not the header: the counter increment
    // lives in the latch, so the continue path advances the induction
    // variable exactly as the fall-through path does. Targeting the header
    // re-tested the same element forever — a silent infinite loop with no
    // diagnostic.
    ctx.loop_stack.push(LoopFrame {
        continue_bb: latch_bb,
        break_bb,
        loop_value: None,
    });
    let _ = lower_block_expr(ctx, plan.body);
    if ctx.current_bb.is_some() {
        goto(ctx, plan.body.span, latch_bb);
    }
    ctx.loop_stack.pop();
    ctx.bindings.remove(&plan.binder);

    // Latch: advance the counter, then re-test at the header. Every path
    // that keeps the loop going — the body's fall-through and every
    // `continue` — routes through here, so the increment fires exactly
    // once per iteration. The latch stays well-formed even when the body
    // always diverges (it then has no predecessors and is simply
    // unreachable, like the break block of an infinite `loop`).
    ctx.current_bb = Some(latch_bb);
    emit_counter_increment(ctx, &plan);
    goto(ctx, plan.body.span, header_bb);

    ctx.current_bb = Some(break_bb);
    Operand::Unit
}

/// The local the body sees under `binder`: the slice element for the
/// slice form, the counter itself for the range form.
fn body_binder_local(plan: &IndexedLoop<'_>) -> LocalId {
    match &plan.elem {
        Some(slice_elem) => slice_elem.binder_local,
        None => plan.counter_local,
    }
}

/// Emit `tmp_cmp = counter CMP bound` and return `Move(tmp_cmp)`.
fn emit_loop_cmp(ctx: &mut LoweringContext<'_>, plan: &IndexedLoop<'_>) -> Operand {
    let bool_ty = MirType::prim(MirPrim::Bool);
    let cmp_temp = match ctx.body.as_mut() {
        Some(b) => b.temp(bool_ty.clone(), plan.span),
        None => return Operand::Unit,
    };
    push_assign(
        ctx,
        plan.span,
        cmp_temp,
        Rvalue {
            span: plan.span,
            kind: RvalueKind::BinOp {
                op: plan.cmp_op,
                lhs: Operand::Copy(Place::local(plan.counter_local)),
                rhs: Operand::Copy(Place::local(plan.bound_local)),
                prim: plan.prim,
            },
            ty: bool_ty,
        },
    );
    Operand::Move(Place::local(cmp_temp))
}

/// For the slice form, emit `binder = source[counter]` at the top of
/// the body block. For the range form this is a no-op.
fn bind_slice_elem(ctx: &mut LoweringContext<'_>, plan: &IndexedLoop<'_>) {
    let Some(slice_elem) = &plan.elem else { return };
    push_assign(
        ctx,
        plan.span,
        slice_elem.binder_local,
        Rvalue {
            span: plan.span,
            kind: RvalueKind::ArrayIndex {
                array: slice_elem.source.clone(),
                idx: Operand::Copy(Place::local(plan.counter_local)),
            },
            ty: slice_elem.elem_ty.clone(),
        },
    );
}

/// Emit the per-iteration `counter += 1` assignment.
fn emit_counter_increment(ctx: &mut LoweringContext<'_>, plan: &IndexedLoop<'_>) {
    let one_value = match plan.prim {
        MirPrim::I8
        | MirPrim::I16
        | MirPrim::I32
        | MirPrim::I64
        | MirPrim::I128
        | MirPrim::Isize => ConstValue::Int(1),
        _ => ConstValue::Uint(1),
    };
    let one_id = ctx.program.push_const(Const {
        ty: plan.counter_ty.clone(),
        value: one_value,
    });
    push_assign(
        ctx,
        plan.span,
        plan.counter_local,
        Rvalue {
            span: plan.span,
            kind: RvalueKind::BinOp {
                op: BinOp::Add,
                lhs: Operand::Copy(Place::local(plan.counter_local)),
                rhs: Operand::Const(one_id),
                prim: plan.prim,
            },
            ty: plan.counter_ty.clone(),
        },
    );
}
