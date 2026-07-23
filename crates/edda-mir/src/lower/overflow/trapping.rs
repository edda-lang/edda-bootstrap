//! Trapping-arithmetic CFG synthesis for the default `+` / `-` / `*` operators.
//!
//! Mirrors `checked::lower_checked_arith`'s widen-narrow-compare overflow
//! detection, but seals the overflow branch with `TerminatorKind::Panic` (a
//! hard runtime abort) instead of `TerminatorKind::Raise` — default
//! arithmetic traps per CLAUDE.md ("trapping by default"); it does not raise
//! an `err: Overflow` effect, so no `std.overflow.Overflow` import is
//! required. `@unverified` / `@trust` on the enclosing function must not
//! affect this: those annotations gate SMT *discharge* only, never runtime
//! check emission.

use edda_span::Span;
use edda_syntax::ast::BinOp as AstBinOp;
use edda_types::HirExpr;

use crate::constant::{Const, ConstValue};
use crate::operand::Operand;
use crate::place::Place;
use crate::rvalue::{BinOp, Rvalue, RvalueKind};
use crate::terminator::{Terminator, TerminatorKind};
use crate::ty::{MirPrim, MirType};

use super::super::cfg::{alloc_block, push_assign, seal_switch_bool, seal_terminator};
use super::super::ctx::LoweringContext;
use super::super::expr::lower_expr_to_operand;
use super::super::ty::lower_ty;

/// Lower a default-mode (trapping) arithmetic expression `a OP b` into MIR.
/// Computes the operation in the next-wider integer width, narrows the
/// result back, re-widens, and compares — any difference means the narrow
/// result lost information and the program must abort rather than silently
/// wrap, per the locked "trapping by default" contract.
pub(crate) fn lower_trapping_arith(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    op: AstBinOp,
    lhs: &HirExpr,
    rhs: &HirExpr,
    prim: MirPrim,
) -> Operand {
    let (wide_prim, narrow_prim) = match prim {
        MirPrim::I8 | MirPrim::I16 | MirPrim::I32 | MirPrim::I64 | MirPrim::Isize => {
            (MirPrim::I128, prim)
        }
        MirPrim::U8 | MirPrim::U16 | MirPrim::U32 | MirPrim::U64 | MirPrim::Usize => {
            (MirPrim::U128, prim)
        }
        _ => return lower_plain_arith(ctx, expr, op, lhs, rhs, prim),
    };
    let wide_op = match op {
        AstBinOp::Add => BinOp::WrapAdd,
        AstBinOp::Sub => BinOp::WrapSub,
        AstBinOp::Mul => BinOp::WrapMul,
        _ => unreachable!("lower_trapping_arith called with a non-arithmetic operator"),
    };

    let lhs_op = lower_expr_to_operand(ctx, lhs);
    let rhs_op = lower_expr_to_operand(ctx, rhs);
    if ctx.current_bb.is_none() {
        return Operand::Unit;
    }

    let result_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, expr.ty);
    let wide_ty = MirType::prim(wide_prim);
    let bool_ty = MirType::prim(MirPrim::Bool);

    let body = match ctx.body.as_mut() {
        Some(b) => b,
        None => return Operand::Unit,
    };
    let result_local = body.temp(result_ty.clone(), expr.span);
    let lhs_wide = body.temp(wide_ty.clone(), expr.span);
    let rhs_wide = body.temp(wide_ty.clone(), expr.span);
    let result_wide = body.temp(wide_ty.clone(), expr.span);
    let check_wide = body.temp(wide_ty.clone(), expr.span);
    let overflow_flag = body.temp(bool_ty.clone(), expr.span);

    // lhs_wide = lhs as <wide>
    push_assign(ctx, expr.span, lhs_wide, Rvalue {
        span: expr.span,
        kind: RvalueKind::Cast { src: lhs_op, src_prim: narrow_prim, dst_prim: wide_prim },
        ty: wide_ty.clone(),
    });
    // rhs_wide = rhs as <wide>
    push_assign(ctx, expr.span, rhs_wide, Rvalue {
        span: expr.span,
        kind: RvalueKind::Cast { src: rhs_op, src_prim: narrow_prim, dst_prim: wide_prim },
        ty: wide_ty.clone(),
    });
    // result_wide = lhs_wide WRAP_OP rhs_wide  (in wide_prim; cannot overflow because operands fit with room to spare)
    push_assign(ctx, expr.span, result_wide, Rvalue {
        span: expr.span,
        kind: RvalueKind::BinOp {
            op: wide_op,
            lhs: Operand::Copy(Place::local(lhs_wide)),
            rhs: Operand::Copy(Place::local(rhs_wide)),
            prim: wide_prim,
        },
        ty: wide_ty.clone(),
    });
    // result = result_wide as <narrow>  (narrowing wrap)
    push_assign(ctx, expr.span, result_local, Rvalue {
        span: expr.span,
        kind: RvalueKind::Cast {
            src: Operand::Copy(Place::local(result_wide)),
            src_prim: wide_prim,
            dst_prim: narrow_prim,
        },
        ty: result_ty.clone(),
    });
    // check_wide = result as <wide>  (sign/zero-extend back)
    push_assign(ctx, expr.span, check_wide, Rvalue {
        span: expr.span,
        kind: RvalueKind::Cast {
            src: Operand::Copy(Place::local(result_local)),
            src_prim: narrow_prim,
            dst_prim: wide_prim,
        },
        ty: wide_ty.clone(),
    });
    // overflow_flag = (check_wide != result_wide)
    push_assign(ctx, expr.span, overflow_flag, Rvalue {
        span: expr.span,
        kind: RvalueKind::BinOp {
            op: BinOp::Ne,
            lhs: Operand::Copy(Place::local(check_wide)),
            rhs: Operand::Copy(Place::local(result_wide)),
            prim: wide_prim,
        },
        ty: bool_ty,
    });

    let ok_bb = match alloc_block(ctx) {
        Some(b) => b,
        None => return Operand::Unit,
    };
    let trap_bb = match alloc_block(ctx) {
        Some(b) => b,
        None => return Operand::Unit,
    };
    seal_switch_bool(
        ctx,
        expr.span,
        Operand::Copy(Place::local(overflow_flag)),
        trap_bb,
        ok_bb,
    );

    ctx.current_bb = Some(trap_bb);
    let msg = push_str_const(ctx, expr.span, "integer overflow");
    seal_terminator(
        ctx,
        Terminator {
            span: expr.span,
            kind: TerminatorKind::Panic { msg },
        },
    );

    ctx.current_bb = Some(ok_bb);
    Operand::Move(Place::local(result_local))
}

/// Plain (non-trapping) arithmetic lowering for operand widths that cannot
/// be widened further (`i128`/`u128`). Preserves the pre-existing behavior
/// for these widths rather than introducing a new diagnostic surface.
fn lower_plain_arith(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    op: AstBinOp,
    lhs: &HirExpr,
    rhs: &HirExpr,
    prim: MirPrim,
) -> Operand {
    let lhs_op = lower_expr_to_operand(ctx, lhs);
    let rhs_op = lower_expr_to_operand(ctx, rhs);
    if ctx.current_bb.is_none() {
        return Operand::Unit;
    }
    let result_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, expr.ty);
    let mir_op = match op {
        AstBinOp::Add => BinOp::Add,
        AstBinOp::Sub => BinOp::Sub,
        AstBinOp::Mul => BinOp::Mul,
        _ => unreachable!("lower_plain_arith called with a non-arithmetic operator"),
    };
    let temp = match ctx.body.as_mut() {
        Some(body) => body.temp(result_ty.clone(), expr.span),
        None => return Operand::Unit,
    };
    push_assign(ctx, expr.span, temp, Rvalue {
        span: expr.span,
        kind: RvalueKind::BinOp { op: mir_op, lhs: lhs_op, rhs: rhs_op, prim },
        ty: result_ty,
    });
    Operand::Move(Place::local(temp))
}

/// Emit a static string constant (interned from `msg`) into a fresh
/// `Str`-typed temp and return `Move(temp)`, for use as a `Panic` message
/// operand that has no source `HirExpr` to lower.
fn push_str_const(ctx: &mut LoweringContext<'_>, span: Span, msg: &str) -> Operand {
    let str_ty = MirType::prim(MirPrim::Str);
    let sym = ctx.interner.intern(msg);
    let id = ctx.program.push_const(Const {
        ty: str_ty.clone(),
        value: ConstValue::Str(sym),
    });
    let temp = match ctx.body.as_mut() {
        Some(body) => body.temp(str_ty.clone(), span),
        None => return Operand::Unit,
    };
    push_assign(ctx, span, temp, Rvalue {
        span,
        kind: RvalueKind::Use(Operand::Const(id)),
        ty: str_ty,
    });
    Operand::Move(Place::local(temp))
}
