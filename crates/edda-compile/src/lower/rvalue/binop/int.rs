//! Integer-class [`BinOp`] lowering.
//!
//! Signed vs. unsigned divergence (`sdiv`/`udiv`, `ashr`/`lshr`,
//! `slt`/`ult`) is keyed by the `signed` flag. Signed modulo uses the
//! Euclidean convention per B-016 — the result is non-negative
//! (`0 <= result < abs(b)`), matching SMT-LIB `(mod x y)`; saturating
//! arithmetic lowers through the `llvm.{s,u}{add,sub}.sat` /
//! `llvm.{s,u}mul.with.overflow` intrinsic families plus a `select`.

use edda_mir::BinOp;
use inkwell::builder::Builder;
use inkwell::values::{BasicValueEnum, IntValue};
use inkwell::IntPredicate;

use crate::error::CompileError;

use crate::lower::LowerCtx;

use super::icmp;

/// Lower a [`BinOp`] on an integer-typed pair of operands. `signed`
/// picks the signed/unsigned variants for div, rem, shr, the ordered
/// comparisons, and the saturating-arithmetic intrinsic family.
pub(super) fn lower_binop_int<'ctx>(
    op: BinOp,
    l: IntValue<'ctx>,
    r: IntValue<'ctx>,
    signed: bool,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    let builder = cx.builder;
    let body_name = cx.body_name;
    let v: BasicValueEnum<'ctx> = match op {
        BinOp::Add => builder.build_int_add(l, r, "add").unwrap().into(),
        BinOp::Sub => builder.build_int_sub(l, r, "sub").unwrap().into(),
        BinOp::Mul => builder.build_int_mul(l, r, "mul").unwrap().into(),
        // Wrapping forms have modulo two's-complement semantics. Plain
        // LLVM `add`/`sub`/`mul` (no `nsw`/`nuw` flags) already wraps,
        // so the lowering is identical to the trapping forms above.
        // Trapping semantics for the default ops are not yet implemented: a
        // refinement-discharged precondition prevents overflow at the
        // call site, or the explicit-mode operator selects this path.
        BinOp::WrapAdd => builder.build_int_add(l, r, "wrap_add").unwrap().into(),
        BinOp::WrapSub => builder.build_int_sub(l, r, "wrap_sub").unwrap().into(),
        BinOp::WrapMul => builder.build_int_mul(l, r, "wrap_mul").unwrap().into(),
        // Saturating forms clamp to the operand width's MIN/MAX. Add /
        // sub lower through the standard LLVM saturating intrinsics
        // `llvm.{s,u}{add,sub}.sat.iN`. Multiplication lowers through
        // `llvm.{s,u}mul.with.overflow.iN` plus a saturation `select`:
        // signed → INT_MIN if operand signs differ, otherwise INT_MAX;
        // unsigned → UINT_MAX. The width is derived from the LLVM type
        // of `l`, matching the `MirPrim` width is_integer admits.
        BinOp::SatAdd => sat_addsub(l, r, signed, /*subtract=*/ false, cx),
        BinOp::SatSub => sat_addsub(l, r, signed, /*subtract=*/ true, cx),
        BinOp::SatMul => sat_mul(l, r, signed, cx),
        BinOp::Div if signed => builder.build_int_signed_div(l, r, "sdiv").unwrap().into(),
        BinOp::Div => builder.build_int_unsigned_div(l, r, "udiv").unwrap().into(),
        // Signed modulo uses the Euclidean convention per B-016: when
        // LLVM's `srem` (truncating, sign of dividend) returns a negative
        // remainder, add `abs(divisor)` so the result is non-negative —
        // `0 <= result < abs(b)`, matching SMT-LIB `(mod x y)`.
        // `(-5) % 3 == 1` and `17 % -5 == 2`, not `-2` / `-3` as plain
        // `srem` / a floored fixup would produce.
        BinOp::Mod if signed => signed_euclidean_mod(l, r, builder).into(),
        BinOp::Mod => builder.build_int_unsigned_rem(l, r, "urem").unwrap().into(),
        BinOp::Eq => icmp(builder, IntPredicate::EQ, l, r, "eq"),
        BinOp::Ne => icmp(builder, IntPredicate::NE, l, r, "ne"),
        BinOp::Lt => icmp(builder, signed_predicate(signed, IntPredicate::SLT, IntPredicate::ULT), l, r, "lt"),
        BinOp::Le => icmp(builder, signed_predicate(signed, IntPredicate::SLE, IntPredicate::ULE), l, r, "le"),
        BinOp::Gt => icmp(builder, signed_predicate(signed, IntPredicate::SGT, IntPredicate::UGT), l, r, "gt"),
        BinOp::Ge => icmp(builder, signed_predicate(signed, IntPredicate::SGE, IntPredicate::UGE), l, r, "ge"),
        BinOp::BitAnd => builder.build_and(l, r, "and").unwrap().into(),
        BinOp::BitOr => builder.build_or(l, r, "or").unwrap().into(),
        BinOp::BitXor => builder.build_xor(l, r, "xor").unwrap().into(),
        BinOp::Shl => builder.build_left_shift(l, r, "shl").unwrap().into(),
        // inkwell's build_right_shift: the `sign_extend` flag picks `ashr` vs `lshr`.
        BinOp::Shr => builder.build_right_shift(l, r, signed, "shr").unwrap().into(),
        BinOp::And | BinOp::Or => {
            return Err(CompileError::UnsupportedMirShape {
                shape: "boolean-and-or-on-integer",
                detail: format!(
                    "body {body_name:?}: {op:?} requires a bool operand; \
                     MIR validation should have caught this"
                ),
            });
        }
    };
    Ok(v)
}

const fn signed_predicate(signed: bool, s: IntPredicate, u: IntPredicate) -> IntPredicate {
    if signed { s } else { u }
}

//            `srem`: when `srem`'s (sign-of-dividend) remainder is
//            negative, `abs(divisor)` is added back so the result is
//            non-negative; otherwise the `srem` value is preserved
//            B-016: `0 <= result < abs(b)` for every `b != 0`
//            (no `nsw`), so `b == INT_MIN` wraps exactly — `-3 % INT_MIN`
//            is `2^63 - 3`, not a trap
//            lowering single-block
/// Emit the Euclidean adjustment over LLVM's `srem`. The returned value
/// has the same width as `l` / `r` and is always non-negative
/// (`0 <= result < abs(r)`), matching SMT-LIB `(mod a b)` — distinct from
/// both the truncating-toward-zero `srem` default and the floored
/// (sign-of-divisor) modulo.
fn signed_euclidean_mod<'ctx>(
    l: IntValue<'ctx>,
    r: IntValue<'ctx>,
    builder: &Builder<'ctx>,
) -> IntValue<'ctx> {
    let raw = builder
        .build_int_signed_rem(l, r, "srem")
        .expect("srem in a positioned block");
    let zero = raw.get_type().const_zero();
    // abs(r) via wrapping negate (plain `sub`, no nsw) so r == INT_MIN
    // maps to its own bit pattern and the trailing add stays exact.
    let neg_r = builder
        .build_int_sub(zero, r, "mod.neg.divisor")
        .expect("integer sub in a positioned block");
    let divisor_neg = builder
        .build_int_compare(IntPredicate::SLT, r, zero, "mod.divisor.neg")
        .expect("icmp slt in a positioned block");
    let abs_r = builder
        .build_select(divisor_neg, neg_r, r, "mod.abs.divisor")
        .expect("select in a positioned block")
        .into_int_value();
    let raw_plus_absb = builder
        .build_int_add(raw, abs_r, "mod.adj")
        .expect("integer add in a positioned block");
    // The truncated remainder carries the dividend's sign; a negative
    // remainder is the only case needing a lift into `[0, abs(b))`.
    let raw_neg = builder
        .build_int_compare(IntPredicate::SLT, raw, zero, "mod.rem.neg")
        .expect("icmp slt in a positioned block");
    builder
        .build_select(raw_neg, raw_plus_absb, raw, "mod.result")
        .expect("select in a positioned block")
        .into_int_value()
}

/// Emit a call to one of the saturating add/sub intrinsics:
/// `llvm.{s,u}{add,sub}.sat.iN`. Returns the saturated result as a
/// [`BasicValueEnum`].
fn sat_addsub<'ctx>(
    l: IntValue<'ctx>,
    r: IntValue<'ctx>,
    signed: bool,
    subtract: bool,
    cx: &LowerCtx<'ctx, '_>,
) -> BasicValueEnum<'ctx> {
    let int_ty = l.get_type();
    let bits = int_ty.get_bit_width();
    let stem = match (signed, subtract) {
        (true, false) => "sadd",
        (false, false) => "uadd",
        (true, true) => "ssub",
        (false, true) => "usub",
    };
    let intrinsic = format!("llvm.{stem}.sat.i{bits}");
    let func = match cx.module.get_function(&intrinsic) {
        Some(f) => f,
        None => {
            let fn_ty = int_ty.fn_type(&[int_ty.into(), int_ty.into()], false);
            cx.module.add_function(&intrinsic, fn_ty, None)
        }
    };
    let call = cx
        .builder
        .build_call(func, &[l.into(), r.into()], "sat")
        .expect("build_call to saturating intrinsic in a positioned block");
    call.try_as_basic_value()
        .left()
        .expect("saturating intrinsic returns an iN value")
}

/// Lower a saturating multiplication via `llvm.{s,u}mul.with.overflow.iN`
/// plus a saturation `select`: on overflow pick `INT_MIN` / `INT_MAX`
/// (signed) or `UINT_MAX` (unsigned); otherwise pick the multiply
/// result.
fn sat_mul<'ctx>(
    l: IntValue<'ctx>,
    r: IntValue<'ctx>,
    signed: bool,
    cx: &LowerCtx<'ctx, '_>,
) -> BasicValueEnum<'ctx> {
    let int_ty = l.get_type();
    let bits = int_ty.get_bit_width();
    let i1_ty = cx.context.bool_type();
    let struct_ty = cx.context.struct_type(&[int_ty.into(), i1_ty.into()], false);
    let stem = if signed { "smul" } else { "umul" };
    let intrinsic = format!("llvm.{stem}.with.overflow.i{bits}");
    let func = match cx.module.get_function(&intrinsic) {
        Some(f) => f,
        None => {
            let fn_ty = struct_ty.fn_type(&[int_ty.into(), int_ty.into()], false);
            cx.module.add_function(&intrinsic, fn_ty, None)
        }
    };
    let call = cx
        .builder
        .build_call(func, &[l.into(), r.into()], "mul.ov.call")
        .expect("build_call to with.overflow intrinsic in a positioned block");
    let pair = call
        .try_as_basic_value()
        .left()
        .expect("with.overflow intrinsic returns a struct value")
        .into_struct_value();
    let val = cx
        .builder
        .build_extract_value(pair, 0, "mul.val")
        .expect("extractvalue 0 on {iN, i1}")
        .into_int_value();
    let ov = cx
        .builder
        .build_extract_value(pair, 1, "mul.ov")
        .expect("extractvalue 1 on {iN, i1}")
        .into_int_value();
    let sat_value: IntValue<'ctx> = if signed {
        let one = int_ty.const_int(1, false);
        let shift = int_ty.const_int((bits - 1) as u64, false);
        let int_min = cx
            .builder
            .build_left_shift(one, shift, "imin")
            .expect("shl in a positioned block");
        let int_max = cx
            .builder
            .build_not(int_min, "imax")
            .expect("not in a positioned block");
        let sign_xor = cx
            .builder
            .build_xor(l, r, "sign.xor")
            .expect("xor in a positioned block");
        let zero = int_ty.const_zero();
        let signs_differ = cx
            .builder
            .build_int_compare(IntPredicate::SLT, sign_xor, zero, "signs.differ")
            .expect("icmp slt in a positioned block");
        cx.builder
            .build_select(signs_differ, int_min, int_max, "sat.signed")
            .expect("select in a positioned block")
            .into_int_value()
    } else {
        int_ty.const_all_ones()
    };
    cx.builder
        .build_select(ov, sat_value, val, "satmul")
        .expect("select in a positioned block")
}
