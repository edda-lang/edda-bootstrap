//! Primitive [`BinOp`] dispatch (int / bool / float / str classes).
//!
//! [`lower_binop`] keys on `operand_prim` to route to the per-class
//! helper. Signed vs. unsigned divergence (`sdiv`/`udiv`,
//! `ashr`/`lshr`, `slt`/`ult`) is also keyed there. Float comparisons
//! use the *ordered* (`o*`) predicate family — NaN renders to `false`,
//! matching IEEE-754 default semantics. `Str` equality (`Eq` / `Ne`)
//! is lowered through the `__edda_str_eq` runtime extern; the call
//! site decomposes each `{ ptr, len }` fat pointer into its two scalar
//! components so the wire ABI has no aggregate parameters and the
//! win64 byval translation does not apply.
//!
//! # Submodule layout
//! - [`int`] — integer-class dispatch plus the Euclidean-modulo and
//!   saturating-arithmetic helpers.
//! - [`float_bool`] — float-class and bool-class dispatch.
//! - [`str`] — `Str`-class `Eq` / `Ne` via the `__edda_str_eq` extern.

mod float_bool;
mod int;
mod str;

use edda_mir::{BinOp, MirPrim};
use inkwell::builder::Builder;
use inkwell::values::{BasicValueEnum, FloatValue, IntValue};
use inkwell::{FloatPredicate, IntPredicate};

use crate::error::CompileError;
use crate::mir_prim::{is_float, is_integer, is_signed_integer};

use crate::lower::LowerCtx;

use float_bool::{lower_binop_bool, lower_binop_float};
use int::lower_binop_int;
use str::lower_binop_str;

/// Lower a primitive [`BinOp`] given already-lowered operands. Signed
/// vs. unsigned divergence (sdiv/udiv, ashr/lshr, slt/ult) is keyed
/// by `operand_prim`. Dispatches to per-class helpers
/// ([`lower_binop_float`], [`lower_binop_bool`], [`lower_binop_int`],
/// [`lower_binop_str`]).
pub(crate) fn lower_binop<'ctx>(
    op: BinOp,
    lhs: BasicValueEnum<'ctx>,
    rhs: BasicValueEnum<'ctx>,
    operand_prim: MirPrim,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    if matches!(operand_prim, MirPrim::Str) {
        return lower_binop_str(op, lhs, rhs, cx);
    }

    let builder = cx.builder;
    let body_name = cx.body_name;

    if is_float(operand_prim) {
        return lower_binop_float(op, lhs.into_float_value(), rhs.into_float_value(), builder, body_name);
    }

    if matches!(operand_prim, MirPrim::Bool) {
        return lower_binop_bool(op, lhs.into_int_value(), rhs.into_int_value(), builder, body_name);
    }

    if !is_integer(operand_prim) {
        return Err(CompileError::UnsupportedMirShape {
            shape: "binop-non-scalar-operand",
            detail: format!(
                "body {body_name:?} BinOp on operand of type {operand_prim:?}"
            ),
        });
    }

    lower_binop_int(
        op,
        lhs.into_int_value(),
        rhs.into_int_value(),
        is_signed_integer(operand_prim),
        cx,
    )
}

pub(super) fn icmp<'ctx>(
    builder: &Builder<'ctx>,
    pred: IntPredicate,
    l: IntValue<'ctx>,
    r: IntValue<'ctx>,
    name: &str,
) -> BasicValueEnum<'ctx> {
    builder.build_int_compare(pred, l, r, name).unwrap().into()
}

pub(super) fn fcmp<'ctx>(
    builder: &Builder<'ctx>,
    pred: FloatPredicate,
    l: FloatValue<'ctx>,
    r: FloatValue<'ctx>,
    name: &str,
) -> BasicValueEnum<'ctx> {
    builder.build_float_compare(pred, l, r, name).unwrap().into()
}

#[cfg(test)]
mod tests {
    use crate::Emitter;
    use edda_intern::Interner;
    use edda_mir::MirPrim;

    use crate::lower::test_fixtures::{build_binop_body, linux_x86_64, lower_and_ir};

    #[test]
    fn int_add() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let interner = Interner::new();
        let (body, program) = build_binop_body(&interner, "add_i32", edda_mir::BinOp::Add, MirPrim::I32);
        let ir = lower_and_ir(&emitter, &target, &interner, &body, &program);
        assert!(ir.contains("add i32"), "missing add: {ir}");
    }

    #[test]
    fn float_add_uses_fadd() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let interner = Interner::new();
        let (body, program) = build_binop_body(&interner, "fadd_f64", edda_mir::BinOp::Add, MirPrim::F64);
        let ir = lower_and_ir(&emitter, &target, &interner, &body, &program);
        assert!(ir.contains("fadd double"), "missing fadd: {ir}");
    }

    #[test]
    fn signed_div_uses_sdiv() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let interner = Interner::new();
        let (body, program) = build_binop_body(&interner, "sdiv_i32", edda_mir::BinOp::Div, MirPrim::I32);
        let ir = lower_and_ir(&emitter, &target, &interner, &body, &program);
        assert!(ir.contains("sdiv i32"), "missing sdiv: {ir}");
    }

    #[test]
    fn unsigned_div_uses_udiv() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let interner = Interner::new();
        let (body, program) = build_binop_body(&interner, "udiv_u32", edda_mir::BinOp::Div, MirPrim::U32);
        let ir = lower_and_ir(&emitter, &target, &interner, &body, &program);
        assert!(ir.contains("udiv i32"), "missing udiv: {ir}");
    }

    #[test]
    fn signed_compare_uses_slt_predicate() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let interner = Interner::new();
        let (body, program) = build_binop_body(&interner, "lt_i32", edda_mir::BinOp::Lt, MirPrim::I32);
        let ir = lower_and_ir(&emitter, &target, &interner, &body, &program);
        assert!(ir.contains("icmp slt"), "missing icmp slt: {ir}");
    }

    #[test]
    fn unsigned_compare_uses_ult_predicate() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let interner = Interner::new();
        let (body, program) = build_binop_body(&interner, "lt_u32", edda_mir::BinOp::Lt, MirPrim::U32);
        let ir = lower_and_ir(&emitter, &target, &interner, &body, &program);
        assert!(ir.contains("icmp ult"), "missing icmp ult: {ir}");
    }

    #[test]
    fn float_compare_uses_ordered_predicate() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let interner = Interner::new();
        let (body, program) = build_binop_body(&interner, "lt_f64", edda_mir::BinOp::Lt, MirPrim::F64);
        let ir = lower_and_ir(&emitter, &target, &interner, &body, &program);
        assert!(ir.contains("fcmp olt"), "missing fcmp olt: {ir}");
    }

    #[test]
    fn signed_shr_uses_ashr() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let interner = Interner::new();
        let (body, program) = build_binop_body(&interner, "ashr_i32", edda_mir::BinOp::Shr, MirPrim::I32);
        let ir = lower_and_ir(&emitter, &target, &interner, &body, &program);
        assert!(ir.contains("ashr i32"), "missing ashr: {ir}");
    }

    #[test]
    fn unsigned_shr_uses_lshr() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let interner = Interner::new();
        let (body, program) = build_binop_body(&interner, "lshr_u32", edda_mir::BinOp::Shr, MirPrim::U32);
        let ir = lower_and_ir(&emitter, &target, &interner, &body, &program);
        assert!(ir.contains("lshr i32"), "missing lshr: {ir}");
    }
}
