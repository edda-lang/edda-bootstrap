//! The `a OP? b` / `a %? b` checked-arithmetic lowerings (widen-narrow-compare
//! and the `INT_MIN % -1` guard). Split from the `overflow` module file to keep
//! each file under the size limit; the shared `Overflow`-ADT lookups and
//! `int_min_for` live in the parent and are reached via `super::`.

use edda_syntax::ast::BinOp as AstBinOp;
use edda_types::HirExpr;

use crate::constant::{Const, ConstValue};
use crate::operand::Operand;
use crate::place::Place;
use crate::rvalue::{BinOp, Rvalue, RvalueKind};
use crate::terminator::{Terminator, TerminatorKind};
use crate::ty::{MirPrim, MirType, MirTypeKind};

use super::super::cfg::{alloc_block, assign_into, goto, push_assign, seal_switch_bool, seal_terminator};
use super::super::ctx::LoweringContext;
use super::super::expr::{lower_expr_to_operand, unsupported_and_unit};
use super::super::ty::{lower_ty, ty_to_prim};
use super::{find_overflow_adt, find_overflow_variant, int_min_for};

/// Lower a checked-arithmetic expression `a OP? b` into MIR. Computes
/// the operation in the next-wider integer width, narrows the result
/// back, re-widens, and compares — any difference means the narrow
/// result lost information and we raise `Overflow.overflow` to the
/// caller via the function's declared `err: Overflow` effect row.
pub(crate) fn lower_checked_arith(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    op: AstBinOp,
    lhs: &HirExpr,
    rhs: &HirExpr,
) -> Operand {
    let prim = ty_to_prim(ctx.ty_interner, lhs.ty).unwrap_or(MirPrim::I64);
    let (wide_prim, narrow_prim) = match prim {
        // Widen to i128 for signed widths. `isize` is 64-bit on every
        // v0.1 target — narrowing back through `Isize` produces the
        // same bit pattern as `I64` so the round-trip check works.
        MirPrim::I8 | MirPrim::I16 | MirPrim::I32 | MirPrim::I64 | MirPrim::Isize => {
            (MirPrim::I128, prim)
        }
        // Widen to u128 for unsigned widths. `usize` mirrors `Usize`
        // narrowing for the same reason.
        MirPrim::U8 | MirPrim::U16 | MirPrim::U32 | MirPrim::U64 | MirPrim::Usize => {
            (MirPrim::U128, prim)
        }
        _ => {
            return unsupported_and_unit(
                ctx,
                expr.span,
                "checked arithmetic operand must be a primitive integer narrower than 128 bits",
            );
        }
    };
    let Some(overflow_adt) = find_overflow_adt(ctx) else {
        return unsupported_and_unit(
            ctx,
            expr.span,
            "checked arithmetic requires `std.overflow.Overflow` to be in scope; \
             add `import std.overflow` to the file",
        );
    };
    let Some(overflow_variant) = find_overflow_variant(ctx, overflow_adt) else {
        return unsupported_and_unit(
            ctx,
            expr.span,
            "internal: `std.overflow.Overflow` is missing the `overflow` variant",
        );
    };
    let wide_op = match op {
        AstBinOp::CheckAdd => BinOp::WrapAdd,
        AstBinOp::CheckSub => BinOp::WrapSub,
        AstBinOp::CheckMul => BinOp::WrapMul,
        _ => unreachable!("lower_checked_arith called with non-Check operator"),
    };

    let lhs_op = lower_expr_to_operand(ctx, lhs);
    let rhs_op = lower_expr_to_operand(ctx, rhs);
    if ctx.current_bb.is_none() {
        return Operand::Unit;
    }

    let result_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, expr.ty);
    let wide_ty = MirType::prim(wide_prim);
    let bool_ty = MirType::prim(MirPrim::Bool);
    let adt_ty = MirType::new(MirTypeKind::Adt(overflow_adt));

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
    let overflow_value = body.temp(adt_ty.clone(), expr.span);

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

    // If an enclosing `handle err: Overflow -> recovery { ... }` is on
    // the handler stack, route the overflow path either straight to
    // its recovery block (no binder) or to a fresh err_bb that builds
    // the Overflow.overflow value into the handler's binder local
    // before jumping to recovery — same pattern `?` uses (see
    // `lower_try::fill_binder_extract_block`).
    let handler_match = ctx
        .handler_stack
        .iter()
        .rev()
        .find(|f| f.handled_adt == overflow_adt)
        .map(|f| (f.recovery_bb, f.binder_local));

    let ok_bb = match alloc_block(ctx) {
        Some(b) => b,
        None => return Operand::Unit,
    };
    let err_bb = match handler_match {
        Some((recovery, None)) => recovery,
        Some((_, Some(_))) | None => match alloc_block(ctx) {
            Some(b) => b,
            None => return Operand::Unit,
        },
    };
    seal_switch_bool(
        ctx,
        expr.span,
        Operand::Copy(Place::local(overflow_flag)),
        err_bb,
        ok_bb,
    );

    match handler_match {
        // Handler with no binder: err_bb IS the recovery block —
        // `lower_handle` fills it later, nothing to emit here.
        Some((_, None)) => {}
        // Handler with a binder: err_bb is a fresh fill block that
        // builds Overflow.overflow into the binder local, then
        // jumps to recovery.
        Some((recovery_bb, Some(binder_local))) => {
            ctx.current_bb = Some(err_bb);
            push_assign(ctx, expr.span, binder_local, Rvalue {
                span: expr.span,
                kind: RvalueKind::MakeVariant {
                    adt: overflow_adt,
                    variant: overflow_variant,
                    fields: Vec::new(),
                },
                ty: adt_ty,
            });
            goto(ctx, expr.span, recovery_bb);
        }
        // No handler: err_bb builds Overflow.overflow and raises to
        // the caller.
        None => {
            ctx.current_bb = Some(err_bb);
            push_assign(ctx, expr.span, overflow_value, Rvalue {
                span: expr.span,
                kind: RvalueKind::MakeVariant {
                    adt: overflow_adt,
                    variant: overflow_variant,
                    fields: Vec::new(),
                },
                ty: adt_ty,
            });
            seal_terminator(
                ctx,
                Terminator {
                    span: expr.span,
                    kind: TerminatorKind::Raise {
                        err_adt: overflow_adt,
                        value: Operand::Move(Place::local(overflow_value)),
                    },
                },
            );
        }
    }

    // Continue in ok_bb with the result.
    ctx.current_bb = Some(ok_bb);
    Operand::Move(Place::local(result_local))
}

/// Lower a checked-modulo expression `a %? b` into MIR. The only
/// overflow condition is `INT_MIN % -1` (every other dividend / divisor
/// pair has a well-defined modulus). On signed operands the lowering
/// inserts the explicit guard CFG; on unsigned operands it emits a
/// plain `BinOp::Mod` rvalue because unsigned modulo never overflows.
pub(crate) fn lower_checked_mod(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    lhs: &HirExpr,
    rhs: &HirExpr,
) -> Operand {
    let prim = match ty_to_prim(ctx.ty_interner, lhs.ty) {
        Some(p) => p,
        None => {
            return unsupported_and_unit(
                ctx,
                expr.span,
                "checked modulo operand must be a primitive integer",
            );
        }
    };
    let signed = match prim {
        MirPrim::I8
        | MirPrim::I16
        | MirPrim::I32
        | MirPrim::I64
        | MirPrim::I128
        | MirPrim::Isize => true,
        MirPrim::U8
        | MirPrim::U16
        | MirPrim::U32
        | MirPrim::U64
        | MirPrim::U128
        | MirPrim::Usize => false,
        _ => {
            return unsupported_and_unit(
                ctx,
                expr.span,
                "checked modulo operand must be a primitive integer",
            );
        }
    };

    let lhs_op = lower_expr_to_operand(ctx, lhs);
    let rhs_op = lower_expr_to_operand(ctx, rhs);
    if ctx.current_bb.is_none() {
        return Operand::Unit;
    }
    let result_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, expr.ty);

    // Unsigned: no overflow case — degenerate to plain `Mod`.
    if !signed {
        let body = match ctx.body.as_mut() {
            Some(b) => b,
            None => return Operand::Unit,
        };
        let result_local = body.temp(result_ty.clone(), expr.span);
        push_assign(ctx, expr.span, result_local, Rvalue {
            span: expr.span,
            kind: RvalueKind::BinOp {
                op: BinOp::Mod,
                lhs: lhs_op,
                rhs: rhs_op,
                prim,
            },
            ty: result_ty,
        });
        return Operand::Move(Place::local(result_local));
    }

    // Signed path: guard the `INT_MIN % -1` case.
    let Some(overflow_adt) = find_overflow_adt(ctx) else {
        return unsupported_and_unit(
            ctx,
            expr.span,
            "checked modulo requires `std.overflow.Overflow` to be in scope; \
             add `import std.overflow` to the file",
        );
    };
    let Some(overflow_variant) = find_overflow_variant(ctx, overflow_adt) else {
        return unsupported_and_unit(
            ctx,
            expr.span,
            "internal: `std.overflow.Overflow` is missing the `overflow` variant",
        );
    };

    let prim_ty = MirType::prim(prim);
    let bool_ty = MirType::prim(MirPrim::Bool);
    let adt_ty = MirType::new(MirTypeKind::Adt(overflow_adt));

    let int_min_const = ctx.program.push_const(Const {
        ty: prim_ty.clone(),
        value: ConstValue::Int(int_min_for(prim)),
    });
    let neg_one_const = ctx.program.push_const(Const {
        ty: prim_ty.clone(),
        value: ConstValue::Int(-1),
    });

    let body = match ctx.body.as_mut() {
        Some(b) => b,
        None => return Operand::Unit,
    };
    let result_local = body.temp(result_ty.clone(), expr.span);
    let lhs_local = body.temp(prim_ty.clone(), expr.span);
    let rhs_local = body.temp(prim_ty.clone(), expr.span);
    let is_min_local = body.temp(bool_ty.clone(), expr.span);
    let is_neg_one_local = body.temp(bool_ty.clone(), expr.span);
    let overflow_flag = body.temp(bool_ty.clone(), expr.span);
    let overflow_value = body.temp(adt_ty.clone(), expr.span);

    // Stash both operands so we can reference them twice (once for the
    // overflow comparisons, once for the actual modulo).
    assign_into(ctx, expr.span, lhs_local, lhs_op, prim_ty.clone());
    assign_into(ctx, expr.span, rhs_local, rhs_op, prim_ty.clone());

    // is_min = (lhs == INT_MIN)
    push_assign(ctx, expr.span, is_min_local, Rvalue {
        span: expr.span,
        kind: RvalueKind::BinOp {
            op: BinOp::Eq,
            lhs: Operand::Copy(Place::local(lhs_local)),
            rhs: Operand::Const(int_min_const),
            prim,
        },
        ty: bool_ty.clone(),
    });
    // is_neg_one = (rhs == -1)
    push_assign(ctx, expr.span, is_neg_one_local, Rvalue {
        span: expr.span,
        kind: RvalueKind::BinOp {
            op: BinOp::Eq,
            lhs: Operand::Copy(Place::local(rhs_local)),
            rhs: Operand::Const(neg_one_const),
            prim,
        },
        ty: bool_ty.clone(),
    });
    // overflow = is_min & is_neg_one  (BitAnd on the bool layer is
    // logical-AND because both operands are already known booleans —
    // no short-circuit needed because side effects are not possible
    // on already-stored locals).
    push_assign(ctx, expr.span, overflow_flag, Rvalue {
        span: expr.span,
        kind: RvalueKind::BinOp {
            op: BinOp::BitAnd,
            lhs: Operand::Copy(Place::local(is_min_local)),
            rhs: Operand::Copy(Place::local(is_neg_one_local)),
            prim: MirPrim::Bool,
        },
        ty: bool_ty,
    });

    // Route the overflow path to an enclosing `handle err: Overflow`
    // recovery block when one is on the stack — same pattern `?` and
    // `lower_checked_arith` use. When the handler has a binder,
    // build Overflow.overflow into the binder local before jumping
    // to recovery.
    let handler_match = ctx
        .handler_stack
        .iter()
        .rev()
        .find(|f| f.handled_adt == overflow_adt)
        .map(|f| (f.recovery_bb, f.binder_local));

    let ok_bb = match alloc_block(ctx) {
        Some(b) => b,
        None => return Operand::Unit,
    };
    let err_bb = match handler_match {
        Some((recovery, None)) => recovery,
        Some((_, Some(_))) | None => match alloc_block(ctx) {
            Some(b) => b,
            None => return Operand::Unit,
        },
    };
    seal_switch_bool(
        ctx,
        expr.span,
        Operand::Copy(Place::local(overflow_flag)),
        err_bb,
        ok_bb,
    );

    match handler_match {
        Some((_, None)) => {}
        Some((recovery_bb, Some(binder_local))) => {
            ctx.current_bb = Some(err_bb);
            push_assign(ctx, expr.span, binder_local, Rvalue {
                span: expr.span,
                kind: RvalueKind::MakeVariant {
                    adt: overflow_adt,
                    variant: overflow_variant,
                    fields: Vec::new(),
                },
                ty: adt_ty,
            });
            goto(ctx, expr.span, recovery_bb);
        }
        None => {
            ctx.current_bb = Some(err_bb);
            push_assign(ctx, expr.span, overflow_value, Rvalue {
                span: expr.span,
                kind: RvalueKind::MakeVariant {
                    adt: overflow_adt,
                    variant: overflow_variant,
                    fields: Vec::new(),
                },
                ty: adt_ty,
            });
            seal_terminator(
                ctx,
                Terminator {
                    span: expr.span,
                    kind: TerminatorKind::Raise {
                        err_adt: overflow_adt,
                        value: Operand::Move(Place::local(overflow_value)),
                    },
                },
            );
        }
    }

    // ok_bb: result = lhs % rhs.
    ctx.current_bb = Some(ok_bb);
    push_assign(ctx, expr.span, result_local, Rvalue {
        span: expr.span,
        kind: RvalueKind::BinOp {
            op: BinOp::Mod,
            lhs: Operand::Copy(Place::local(lhs_local)),
            rhs: Operand::Copy(Place::local(rhs_local)),
            prim,
        },
        ty: result_ty,
    });
    Operand::Move(Place::local(result_local))
}
