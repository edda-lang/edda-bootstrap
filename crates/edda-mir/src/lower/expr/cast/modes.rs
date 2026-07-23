//! Checked and saturating narrowing-cast CFG synthesis.
//!
//! The `checked` mode raises `err: Overflow` when a narrowing integer
//! cast loses information; the `saturating` mode clamps the source to the
//! destination's representable range. Split out of `cast/mod.rs` (the
//! dispatch + trapping/wrapping path) to keep both files under the
//! 500-line cap.

use edda_span::Span;

use crate::constant::{Const, ConstValue};
use crate::operand::Operand;
use crate::place::Place;
use crate::rvalue::{BinOp, Rvalue, RvalueKind};
use crate::terminator::{Terminator, TerminatorKind};
use crate::ty::{MirPrim, MirType, MirTypeKind};

use super::super::super::cfg::{alloc_block, assign_into, goto, push_assign, seal_switch_bool, seal_terminator};
use super::super::super::ctx::LoweringContext;
use super::super::super::overflow::{find_overflow_adt, find_overflow_variant, int_min_for};
use super::super::unsupported_and_unit;
use super::int_width_signed;

//            dst → re-extend to src → compare to original → SwitchBool branch
//            → err_bb raises `Overflow.overflow`, ok_bb continues with the
//            truncated result), leaves `current_bb` set to a fresh ok_bb,
//            returns `Move(result_local)`
//            and returns `Unit` — the typer's row check should have caught this
//            already; defensive guard against packages omitting the import
/// Lower an `expr as T checked` narrowing integer cast into MIR.
/// Truncates to the destination width, re-extends back, compares to
/// the original; any difference means the narrow result lost
/// information and we raise `Overflow.overflow` to the caller via the
/// function's declared `err: Overflow` effect row.
pub(super) fn lower_checked_cast(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    src: Operand,
    src_prim: MirPrim,
    dst_prim: MirPrim,
    result_ty: MirType,
) -> Operand {
    let Some(overflow_adt) = find_overflow_adt(ctx) else {
        return unsupported_and_unit(
            ctx,
            span,
            "checked cast requires `std.overflow.Overflow` to be in scope; \
             add `import std.overflow` to the file",
        );
    };
    let Some(overflow_variant) = find_overflow_variant(ctx, overflow_adt) else {
        return unsupported_and_unit(
            ctx,
            span,
            "internal: `std.overflow.Overflow` is missing the `overflow` variant",
        );
    };
    if ctx.current_bb.is_none() {
        return Operand::Unit;
    }

    let src_ty = MirType::prim(src_prim);
    let bool_ty = MirType::prim(MirPrim::Bool);
    let adt_ty = MirType::new(MirTypeKind::Adt(overflow_adt));

    let (src_local, narrowed_local, check_wide, overflow_flag, overflow_value, result_local) = {
        let body = match ctx.body.as_mut() {
            Some(b) => b,
            None => return Operand::Unit,
        };
        let src_local = body.temp(src_ty.clone(), span);
        let narrowed_local = body.temp(result_ty.clone(), span);
        let check_wide = body.temp(src_ty.clone(), span);
        let overflow_flag = body.temp(bool_ty.clone(), span);
        let overflow_value = body.temp(adt_ty.clone(), span);
        let result_local = body.temp(result_ty.clone(), span);
        (
            src_local,
            narrowed_local,
            check_wide,
            overflow_flag,
            overflow_value,
            result_local,
        )
    };

    // Stash src so it can be referenced twice (once narrowed, once
    // compared).
    assign_into(ctx, span, src_local, src, src_ty.clone());

    // narrowed = src as <dst>
    push_assign(ctx, span, narrowed_local, Rvalue {
        span,
        kind: RvalueKind::Cast {
            src: Operand::Copy(Place::local(src_local)),
            src_prim,
            dst_prim,
        },
        ty: result_ty.clone(),
    });
    // check_wide = narrowed as <src>  (sign/zero-extend back; LLVM picks
    // the right extension via the source's signedness).
    push_assign(ctx, span, check_wide, Rvalue {
        span,
        kind: RvalueKind::Cast {
            src: Operand::Copy(Place::local(narrowed_local)),
            src_prim: dst_prim,
            dst_prim: src_prim,
        },
        ty: src_ty.clone(),
    });
    // overflow_flag = (check_wide != src)
    push_assign(ctx, span, overflow_flag, Rvalue {
        span,
        kind: RvalueKind::BinOp {
            op: BinOp::Ne,
            lhs: Operand::Copy(Place::local(check_wide)),
            rhs: Operand::Copy(Place::local(src_local)),
            prim: src_prim,
        },
        ty: bool_ty,
    });

    // Route overflow through any enclosing `handle err: Overflow ->
    // recovery { ... }` — same pattern lower_checked_arith uses.
    let handler_recovery = ctx
        .handler_stack
        .iter()
        .rev()
        .find(|f| f.handled_adt == overflow_adt)
        .map(|f| f.recovery_bb);

    let ok_bb = match alloc_block(ctx) {
        Some(b) => b,
        None => return Operand::Unit,
    };
    let err_bb = match handler_recovery {
        Some(recovery) => recovery,
        None => match alloc_block(ctx) {
            Some(b) => b,
            None => return Operand::Unit,
        },
    };
    seal_switch_bool(
        ctx,
        span,
        Operand::Copy(Place::local(overflow_flag)),
        err_bb,
        ok_bb,
    );

    if handler_recovery.is_none() {
        ctx.current_bb = Some(err_bb);
        push_assign(ctx, span, overflow_value, Rvalue {
            span,
            kind: RvalueKind::MakeVariant {
                adt: overflow_adt,
                variant: overflow_variant,
                fields: Vec::new(),
            },
            ty: adt_ty,
        });
        seal_terminator(ctx, Terminator {
            span,
            kind: TerminatorKind::Raise {
                err_adt: overflow_adt,
                value: Operand::Move(Place::local(overflow_value)),
            },
        });
    }

    // ok_bb: result = narrowed.
    ctx.current_bb = Some(ok_bb);
    assign_into(
        ctx,
        span,
        result_local,
        Operand::Move(Place::local(narrowed_local)),
        result_ty,
    );
    Operand::Move(Place::local(result_local))
}

//            max(min(src, DST_MAX_in_src_width), DST_MIN_in_src_width)`, then
//            truncates to the destination width; returns `Move(result_local)`
//            and leaves `current_bb` on a fresh continuation block
//            clamp is needed (the lower bound is structurally `0`); the
//            CFG omits the lower comparison so the generated IR stays small
/// Lower an `expr as T saturating` narrowing integer cast into MIR.
/// Clamps the source value to the destination type's representable
/// range — values above the destination's MAX become MAX; values below
/// MIN become MIN (only meaningful for signed destinations) — then
/// truncates to the destination width.
pub(super) fn lower_saturating_cast(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    src: Operand,
    src_prim: MirPrim,
    dst_prim: MirPrim,
    result_ty: MirType,
) -> Operand {
    if ctx.current_bb.is_none() {
        return Operand::Unit;
    }

    let (_, dst_signed) = match int_width_signed(dst_prim) {
        Some(p) => p,
        None => unreachable!("lower_saturating_cast invoked on non-integer dst_prim"),
    };
    let (_, src_signed) = match int_width_signed(src_prim) {
        Some(p) => p,
        None => unreachable!("lower_saturating_cast invoked on non-integer src_prim"),
    };

    // Compute the destination type's MIN / MAX as i128 values
    // representable in the source's primitive (the comparisons happen
    // at src width). When the source can't even represent dst's
    // bounds (e.g. casting i8 to u8), the in-range values are still
    // bounded — bound checks fold to comparisons against the source's
    // own representable range.
    let dst_max_i128 = int_max_for(dst_prim);
    let dst_min_i128 = if dst_signed { int_min_for(dst_prim) } else { 0 };

    let src_ty = MirType::prim(src_prim);
    let bool_ty = MirType::prim(MirPrim::Bool);

    let dst_max_const = ctx.program.push_const(Const {
        ty: src_ty.clone(),
        value: ConstValue::Int(dst_max_i128),
    });
    let dst_min_const = ctx.program.push_const(Const {
        ty: src_ty.clone(),
        value: ConstValue::Int(dst_min_i128),
    });

    let (src_local, clamped_high, clamped_low, narrowed_local, gt_max_flag, lt_min_flag) = {
        let body = match ctx.body.as_mut() {
            Some(b) => b,
            None => return Operand::Unit,
        };
        let src_local = body.temp(src_ty.clone(), span);
        let clamped_high = body.temp(src_ty.clone(), span);
        let clamped_low = body.temp(src_ty.clone(), span);
        let narrowed_local = body.temp(result_ty.clone(), span);
        let gt_max_flag = body.temp(bool_ty.clone(), span);
        let lt_min_flag = body.temp(bool_ty.clone(), span);
        (
            src_local,
            clamped_high,
            clamped_low,
            narrowed_local,
            gt_max_flag,
            lt_min_flag,
        )
    };

    // Stash src so we can re-read it.
    assign_into(ctx, span, src_local, src, src_ty.clone());

    // Upper clamp: gt_max_flag = (src > DST_MAX); clamped_high = gt ? DST_MAX : src.
    push_assign(ctx, span, gt_max_flag, Rvalue {
        span,
        kind: RvalueKind::BinOp {
            op: BinOp::Gt,
            lhs: Operand::Copy(Place::local(src_local)),
            rhs: Operand::Const(dst_max_const),
            prim: src_prim,
        },
        ty: bool_ty.clone(),
    });

    let pre_high_bb = ctx
        .current_bb
        .expect("current_bb populated by upper-clamp assign");
    let high_then_bb = match alloc_block(ctx) {
        Some(b) => b,
        None => return Operand::Unit,
    };
    let high_else_bb = match alloc_block(ctx) {
        Some(b) => b,
        None => return Operand::Unit,
    };
    let high_join_bb = match alloc_block(ctx) {
        Some(b) => b,
        None => return Operand::Unit,
    };
    let _ = pre_high_bb;
    seal_switch_bool(
        ctx,
        span,
        Operand::Copy(Place::local(gt_max_flag)),
        high_then_bb,
        high_else_bb,
    );
    ctx.current_bb = Some(high_then_bb);
    assign_into(
        ctx,
        span,
        clamped_high,
        Operand::Const(dst_max_const),
        src_ty.clone(),
    );
    goto(ctx, span, high_join_bb);
    ctx.current_bb = Some(high_else_bb);
    assign_into(
        ctx,
        span,
        clamped_high,
        Operand::Copy(Place::local(src_local)),
        src_ty.clone(),
    );
    goto(ctx, span, high_join_bb);
    ctx.current_bb = Some(high_join_bb);

    // Lower clamp: only needed when either dst is signed (lower bound
    // is non-zero) or src is signed (a negative value otherwise
    // wouldn't be representable as an unsigned dst). For
    // unsigned→unsigned the source is already non-negative; the upper
    // clamp covers all out-of-range cases.
    let need_lower_clamp = dst_signed || src_signed;
    if !need_lower_clamp {
        // narrowed = clamped_high as dst
        push_assign(ctx, span, narrowed_local, Rvalue {
            span,
            kind: RvalueKind::Cast {
                src: Operand::Copy(Place::local(clamped_high)),
                src_prim,
                dst_prim,
            },
            ty: result_ty.clone(),
        });
        return Operand::Move(Place::local(narrowed_local));
    }

    push_assign(ctx, span, lt_min_flag, Rvalue {
        span,
        kind: RvalueKind::BinOp {
            op: BinOp::Lt,
            lhs: Operand::Copy(Place::local(clamped_high)),
            rhs: Operand::Const(dst_min_const),
            prim: src_prim,
        },
        ty: bool_ty,
    });

    let low_then_bb = match alloc_block(ctx) {
        Some(b) => b,
        None => return Operand::Unit,
    };
    let low_else_bb = match alloc_block(ctx) {
        Some(b) => b,
        None => return Operand::Unit,
    };
    let low_join_bb = match alloc_block(ctx) {
        Some(b) => b,
        None => return Operand::Unit,
    };
    seal_switch_bool(
        ctx,
        span,
        Operand::Copy(Place::local(lt_min_flag)),
        low_then_bb,
        low_else_bb,
    );
    ctx.current_bb = Some(low_then_bb);
    assign_into(
        ctx,
        span,
        clamped_low,
        Operand::Const(dst_min_const),
        src_ty.clone(),
    );
    goto(ctx, span, low_join_bb);
    ctx.current_bb = Some(low_else_bb);
    assign_into(
        ctx,
        span,
        clamped_low,
        Operand::Copy(Place::local(clamped_high)),
        src_ty,
    );
    goto(ctx, span, low_join_bb);
    ctx.current_bb = Some(low_join_bb);

    push_assign(ctx, span, narrowed_local, Rvalue {
        span,
        kind: RvalueKind::Cast {
            src: Operand::Copy(Place::local(clamped_low)),
            src_prim,
            dst_prim,
        },
        ty: result_ty,
    });
    Operand::Move(Place::local(narrowed_local))
}

//            value fits; for unsigned prims up to `u64` the value fits; `u128`
//            saturates to `i128::MAX` which is still a faithful upper bound at
//            src widths up to 128 bits
/// `prim::MAX` widened to `i128` for use as an `Operand::Const`. The
/// `u128::MAX` case clamps to `i128::MAX` — saturating from `u128` to a
/// narrower destination via this routine still produces correct results
/// because any source value exceeding `i128::MAX` is also above every
/// narrower destination's MAX.
fn int_max_for(prim: MirPrim) -> i128 {
    match prim {
        MirPrim::I8 => i8::MAX as i128,
        MirPrim::I16 => i16::MAX as i128,
        MirPrim::I32 => i32::MAX as i128,
        MirPrim::I64 | MirPrim::Isize => i64::MAX as i128,
        MirPrim::I128 => i128::MAX,
        MirPrim::U8 => u8::MAX as i128,
        MirPrim::U16 => u16::MAX as i128,
        MirPrim::U32 => u32::MAX as i128,
        MirPrim::U64 | MirPrim::Usize => u64::MAX as i128,
        MirPrim::U128 => i128::MAX,
        _ => unreachable!("int_max_for called on non-integer primitive {prim:?}"),
    }
}
