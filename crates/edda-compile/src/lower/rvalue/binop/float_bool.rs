//! Float-class and bool-class [`BinOp`] lowering.
//!
//! Float comparisons use the *ordered* (`o*`) predicate family — NaN
//! renders to `false`, matching IEEE-754 default semantics. Bool
//! operands admit only logical / bitwise combinators and equality.

use edda_mir::BinOp;
use inkwell::builder::Builder;
use inkwell::values::{BasicValueEnum, FloatValue, IntValue};
use inkwell::{FloatPredicate, IntPredicate};

use crate::error::CompileError;

use super::{fcmp, icmp};

/// Lower a [`BinOp`] on a float-typed pair of operands.
pub(super) fn lower_binop_float<'ctx>(
    op: BinOp,
    l: FloatValue<'ctx>,
    r: FloatValue<'ctx>,
    builder: &Builder<'ctx>,
    body_name: &str,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    let v: BasicValueEnum<'ctx> = match op {
        BinOp::Add => builder.build_float_add(l, r, "fadd").unwrap().into(),
        BinOp::Sub => builder.build_float_sub(l, r, "fsub").unwrap().into(),
        BinOp::Mul => builder.build_float_mul(l, r, "fmul").unwrap().into(),
        BinOp::Div => builder.build_float_div(l, r, "fdiv").unwrap().into(),
        BinOp::Mod => builder.build_float_rem(l, r, "frem").unwrap().into(),
        BinOp::Eq => fcmp(builder, FloatPredicate::OEQ, l, r, "feq"),
        BinOp::Ne => fcmp(builder, FloatPredicate::ONE, l, r, "fne"),
        BinOp::Lt => fcmp(builder, FloatPredicate::OLT, l, r, "flt"),
        BinOp::Le => fcmp(builder, FloatPredicate::OLE, l, r, "fle"),
        BinOp::Gt => fcmp(builder, FloatPredicate::OGT, l, r, "fgt"),
        BinOp::Ge => fcmp(builder, FloatPredicate::OGE, l, r, "fge"),
        BinOp::And | BinOp::Or | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor
        | BinOp::Shl | BinOp::Shr => {
            return Err(CompileError::UnsupportedMirShape {
                shape: "logical-or-bitwise-on-float",
                detail: format!(
                    "body {body_name:?}: {op:?} is not valid on a float operand"
                ),
            });
        }
        BinOp::WrapAdd | BinOp::WrapSub | BinOp::WrapMul => {
            return Err(CompileError::UnsupportedMirShape {
                shape: "wrapping-arithmetic-on-float",
                detail: format!(
                    "body {body_name:?}: {op:?} is integer-only; \
                     typecheck should have rejected the float operand"
                ),
            });
        }
        BinOp::SatAdd | BinOp::SatSub | BinOp::SatMul => {
            return Err(CompileError::UnsupportedMirShape {
                shape: "saturating-arithmetic-on-float",
                detail: format!(
                    "body {body_name:?}: {op:?} is integer-only; \
                     typecheck should have rejected the float operand"
                ),
            });
        }
    };
    Ok(v)
}

/// Lower a [`BinOp`] on a bool-typed pair of operands.
pub(super) fn lower_binop_bool<'ctx>(
    op: BinOp,
    l: IntValue<'ctx>,
    r: IntValue<'ctx>,
    builder: &Builder<'ctx>,
    body_name: &str,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    let v: BasicValueEnum<'ctx> = match op {
        BinOp::And | BinOp::BitAnd => builder.build_and(l, r, "and").unwrap().into(),
        BinOp::Or | BinOp::BitOr => builder.build_or(l, r, "or").unwrap().into(),
        BinOp::BitXor => builder.build_xor(l, r, "xor").unwrap().into(),
        BinOp::Eq => icmp(builder, IntPredicate::EQ, l, r, "eq"),
        BinOp::Ne => icmp(builder, IntPredicate::NE, l, r, "ne"),
        _ => {
            return Err(CompileError::UnsupportedMirShape {
                shape: "arithmetic-or-compare-on-bool",
                detail: format!(
                    "body {body_name:?}: {op:?} is not valid on a bool operand"
                ),
            });
        }
    };
    Ok(v)
}
