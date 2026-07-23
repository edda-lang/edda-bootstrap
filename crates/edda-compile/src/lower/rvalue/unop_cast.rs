//! Primitive [`UnOp`] and primitive `Cast`.
//!
//! [`lower_unop`] consumes the
//! [`crate::ops::llvm_unop_shape`] dispatch table. [`lower_cast`]
//! dispatches on the (int, float) × (int, float) cross product to
//! one of four helpers.

use edda_mir::{MirPrim, UnOp};
use edda_target::Arch;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, IntValue};

use crate::error::CompileError;
use crate::mir_prim::{is_float, is_integer, is_signed_integer};

use super::super::ty::inkwell_prim_type;

/// Lower a primitive [`UnOp`] given an already-lowered operand.
pub(crate) fn lower_unop<'ctx>(
    op: UnOp,
    value: BasicValueEnum<'ctx>,
    operand_prim: MirPrim,
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    body_name: &str,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    use crate::ops::{LlvmUnOpShape, llvm_unop_shape};

    let shape = llvm_unop_shape(op, operand_prim).ok_or_else(|| {
        CompileError::UnsupportedMirShape {
            shape: "unop-operand-mismatch",
            detail: format!(
                "body {body_name:?}: {op:?} on operand of type {operand_prim:?} \
                 has no defined LLVM lowering"
            ),
        }
    })?;

    Ok(match shape {
        LlvmUnOpShape::NegInt => {
            let int = value.into_int_value();
            builder.build_int_neg(int, "neg").unwrap().into()
        }
        LlvmUnOpShape::NegFloat => {
            let f = value.into_float_value();
            builder.build_float_neg(f, "fneg").unwrap().into()
        }
        LlvmUnOpShape::NotBool => {
            let bit: IntValue<'ctx> = value.into_int_value();
            let one = context.bool_type().const_int(1, false);
            builder.build_xor(bit, one, "not").unwrap().into()
        }
        LlvmUnOpShape::BitNotInt => {
            let int = value.into_int_value();
            let all_ones = int.get_type().const_all_ones();
            builder.build_xor(int, all_ones, "bnot").unwrap().into()
        }
    })
}

/// Lower a primitive [`edda_mir::RvalueKind::Cast`] given an
/// already-lowered source operand. Dispatches to one of four
/// (int, float) × (int, float) helpers.
pub(crate) fn lower_cast<'ctx>(
    value: BasicValueEnum<'ctx>,
    src_prim: MirPrim,
    dst_prim: MirPrim,
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    arch: Arch,
    body_name: &str,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    if matches!(src_prim, MirPrim::Str) || matches!(dst_prim, MirPrim::Str) {
        return Err(CompileError::UnsupportedMirShape {
            shape: "cast-str",
            detail: format!(
                "body {body_name:?} casts {src_prim:?} -> {dst_prim:?}; \
                 `str` casts are not yet supported (pending string-pool support)"
            ),
        });
    }

    let dst_ty = inkwell_prim_type(context, dst_prim, arch).ok_or_else(|| {
        CompileError::UnsupportedMirShape {
            shape: "cast-non-primitive-dst",
            detail: format!(
                "body {body_name:?} casts to non-primitive {dst_prim:?}"
            ),
        }
    })?;

    match (is_integer(src_prim), is_float(src_prim), is_integer(dst_prim), is_float(dst_prim)) {
        (true, _, true, _) => Ok(cast_int_to_int(value, dst_ty, src_prim, builder)),
        (true, _, _, true) => Ok(cast_int_to_float(value, dst_ty, src_prim, builder)),
        (_, true, true, _) => Ok(cast_float_to_int(value, dst_ty, dst_prim, builder)),
        (_, true, _, true) => Ok(cast_float_to_float(value, dst_ty, builder)),
        _ => Err(CompileError::UnsupportedMirShape {
            shape: "cast-unsupported-pair",
            detail: format!(
                "body {body_name:?}: cast from {src_prim:?} to {dst_prim:?} \
                 has no defined LLVM lowering"
            ),
        }),
    }
}

/// Integer-to-integer cast. The sign flag picks zext vs sext on
/// widening (from the *source*'s signedness); on narrowing LLVM
/// always truncates and the flag is a no-op.
fn cast_int_to_int<'ctx>(
    value: BasicValueEnum<'ctx>,
    dst_ty: BasicTypeEnum<'ctx>,
    src_prim: MirPrim,
    builder: &Builder<'ctx>,
) -> BasicValueEnum<'ctx> {
    let v = value.into_int_value();
    let dst_int = dst_ty.into_int_type();
    let signed = is_signed_integer(src_prim);
    builder
        .build_int_cast_sign_flag(v, dst_int, signed, "icast")
        .unwrap()
        .into()
}

/// Integer-to-float cast. `sitofp` for signed sources, `uitofp` for
/// unsigned.
fn cast_int_to_float<'ctx>(
    value: BasicValueEnum<'ctx>,
    dst_ty: BasicTypeEnum<'ctx>,
    src_prim: MirPrim,
    builder: &Builder<'ctx>,
) -> BasicValueEnum<'ctx> {
    let v = value.into_int_value();
    let dst_float = dst_ty.into_float_type();
    if is_signed_integer(src_prim) {
        builder.build_signed_int_to_float(v, dst_float, "sitofp").unwrap().into()
    } else {
        builder.build_unsigned_int_to_float(v, dst_float, "uitofp").unwrap().into()
    }
}

/// Float-to-integer cast. `fptosi` for signed destinations, `fptoui`
/// for unsigned.
fn cast_float_to_int<'ctx>(
    value: BasicValueEnum<'ctx>,
    dst_ty: BasicTypeEnum<'ctx>,
    dst_prim: MirPrim,
    builder: &Builder<'ctx>,
) -> BasicValueEnum<'ctx> {
    let v = value.into_float_value();
    let dst_int = dst_ty.into_int_type();
    if is_signed_integer(dst_prim) {
        builder.build_float_to_signed_int(v, dst_int, "fptosi").unwrap().into()
    } else {
        builder.build_float_to_unsigned_int(v, dst_int, "fptoui").unwrap().into()
    }
}

/// Float-to-float cast. inkwell's `build_float_cast` picks `fpext` or
/// `fptrunc` from the relative widths of source and destination.
fn cast_float_to_float<'ctx>(
    value: BasicValueEnum<'ctx>,
    dst_ty: BasicTypeEnum<'ctx>,
    builder: &Builder<'ctx>,
) -> BasicValueEnum<'ctx> {
    let v = value.into_float_value();
    let dst_float = dst_ty.into_float_type();
    builder.build_float_cast(v, dst_float, "fcast").unwrap().into()
}

#[cfg(test)]
mod tests {
    use crate::Emitter;
    use edda_intern::Interner;
    use edda_mir::{
        BodyBuilder, MirPrim, MirType, Operand, ParamMode, Place, ProgramBuilder, Rvalue,
        RvalueKind,
    };
    use edda_span::Span;

    use super::super::super::test_fixtures::{build_unop_body, linux_x86_64, lower_and_ir};

    #[test]
    fn unop_neg_int() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let interner = Interner::new();
        let (body, program) = build_unop_body(&interner, "neg_i32", edda_mir::UnOp::Neg, MirPrim::I32);
        let ir = lower_and_ir(&emitter, &target, &interner, &body, &program);
        assert!(ir.contains("sub i32 0,"), "missing int neg: {ir}");
    }

    #[test]
    fn unop_neg_float() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let interner = Interner::new();
        let (body, program) = build_unop_body(&interner, "neg_f64", edda_mir::UnOp::Neg, MirPrim::F64);
        let ir = lower_and_ir(&emitter, &target, &interner, &body, &program);
        assert!(ir.contains("fneg double"), "missing fneg: {ir}");
    }

    #[test]
    fn unop_not_bool() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let interner = Interner::new();
        let (body, program) = build_unop_body(&interner, "not_bool", edda_mir::UnOp::Not, MirPrim::Bool);
        let ir = lower_and_ir(&emitter, &target, &interner, &body, &program);
        assert!(ir.contains("xor i1"), "missing xor (not): {ir}");
    }

    #[test]
    fn int_cast_widening_signed_uses_sext() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();
        let name = interner.intern("cast_i32_to_i64");
        let dst_ty = MirType::prim(MirPrim::I64);
        let mut bb = BodyBuilder::new(name, Span::DUMMY, dst_ty.clone());
        let ret_local = bb.return_slot(dst_ty.clone(), Span::DUMMY);
        let a = bb.param(ParamMode::Let, MirType::prim(MirPrim::I32), Span::DUMMY);
        let mut block = bb.block();
        let block_id = block.id();
        block.assign(
            Span::DUMMY,
            Place::local(ret_local),
            Rvalue {
                span: Span::DUMMY,
                kind: RvalueKind::Cast {
                    src: Operand::Copy(Place::local(a)),
                    src_prim: MirPrim::I32,
                    dst_prim: MirPrim::I64,
                },
                ty: dst_ty.clone(),
            },
        );
        block.return_(Span::DUMMY, Operand::Copy(Place::local(ret_local)));
        bb.set_entry(block_id);
        let body = bb.finish();
        let program = ProgramBuilder::new().finish();
        let function = emitter
            .declare_function(&module, &body, &program, &interner, target.triple().arch())
            .unwrap();
        emitter
            .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
            .expect("widening signed cast must lower");
        let ir = module.print_to_string().to_string();
        assert!(ir.contains("sext i32"), "missing sext i32: {ir}");
    }
}
