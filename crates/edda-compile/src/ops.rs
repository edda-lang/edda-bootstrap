//! MIR [`UnOp`] -> LLVM lowering shape.
//!
//! The emitter dispatches unary operations through [`llvm_unop_shape`]
//! to pick between integer negation (`sub <ty> 0, %x`), float negation
//! (`fneg`), boolean `xor i1 %x, true`, and bitwise complement
//! (`xor <ty> %x, -1`). Signedness divergence is keyed by the operand's
//! [`MirPrim`].
//!
//! Pure functions; no LLVM dependency required.
//!
//! Binary-op dispatch lives directly in [`crate::lower::rvalue::lower_binop`]
//! against the inkwell typed-builder methods — there is no parallel
//! text-mnemonic table.
//!
//! # Validation responsibility
//!
//! Returning `None` here is **not** the diagnostic surface — MIR
//! validation in `edda-mir` is expected to reject impossible (op,
//! operand) pairings before they reach this crate. `None` is a defensive
//! contract: if the impossible pairing arrives anyway, callers can
//! produce a `CompileError::ParseError`-class diagnostic rather than
//! emit nonsense IR.

use edda_mir::{MirPrim, UnOp};

use crate::mir_prim::{is_float, is_integer, is_signed_integer};

/// LLVM-level lowering shape for a MIR [`UnOp`].
///
/// LLVM does not have a single-operand integer-negation instruction —
/// `Neg` on integers lowers to `sub <ty> 0, %x`; `BitNot` lowers to
/// `xor <ty> %x, -1`; `Not` on `Bool` lowers to `xor i1 %x, true`. The
/// emitter dispatches on this enum rather than parsing a string.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum LlvmUnOpShape {
    /// Integer negation. Emit `sub <ty> 0, %x`.
    NegInt,
    /// Float negation. Emit `fneg <ty> %x`.
    NegFloat,
    /// Boolean `not`. Emit `xor i1 %x, true`.
    NotBool,
    /// Bitwise complement. Emit `xor <ty> %x, -1` (using the operand's
    /// integer all-ones constant).
    BitNotInt,
}

/// Lowering shape for the (op, operand-primitive) pair.
///
/// - [`UnOp::Neg`] on signed integer -> [`LlvmUnOpShape::NegInt`]; on
///   float -> [`LlvmUnOpShape::NegFloat`]. Unsigned integer negation is
///   not produced by Edda (the typechecker rejects `-x` on `u32`); the
///   function returns `None` for that case.
/// - [`UnOp::Not`] on `Bool` -> [`LlvmUnOpShape::NotBool`].
/// - [`UnOp::BitNot`] on any integer -> [`LlvmUnOpShape::BitNotInt`].
pub fn llvm_unop_shape(op: UnOp, operand: MirPrim) -> Option<LlvmUnOpShape> {
    match op {
        UnOp::Neg => {
            if is_signed_integer(operand) {
                Some(LlvmUnOpShape::NegInt)
            } else if is_float(operand) {
                Some(LlvmUnOpShape::NegFloat)
            } else {
                None
            }
        }
        UnOp::Not => matches!(operand, MirPrim::Bool).then_some(LlvmUnOpShape::NotBool),
        UnOp::BitNot => {
            if is_integer(operand) && !matches!(operand, MirPrim::Bool) {
                Some(LlvmUnOpShape::BitNotInt)
            } else {
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unop_neg_on_signed_integer() {
        for prim in [MirPrim::I8, MirPrim::I32, MirPrim::I64, MirPrim::I128, MirPrim::Isize] {
            assert_eq!(llvm_unop_shape(UnOp::Neg, prim), Some(LlvmUnOpShape::NegInt));
        }
    }

    #[test]
    fn unop_neg_on_unsigned_returns_none() {
        for prim in [MirPrim::U8, MirPrim::U32, MirPrim::Usize, MirPrim::Bool, MirPrim::Char] {
            assert_eq!(llvm_unop_shape(UnOp::Neg, prim), None);
        }
    }

    #[test]
    fn unop_neg_on_float() {
        for prim in [MirPrim::F32, MirPrim::F64] {
            assert_eq!(llvm_unop_shape(UnOp::Neg, prim), Some(LlvmUnOpShape::NegFloat));
        }
    }

    #[test]
    fn unop_not_only_on_bool() {
        assert_eq!(llvm_unop_shape(UnOp::Not, MirPrim::Bool), Some(LlvmUnOpShape::NotBool));
        for prim in [MirPrim::I32, MirPrim::U32, MirPrim::F32, MirPrim::Char] {
            assert_eq!(llvm_unop_shape(UnOp::Not, prim), None);
        }
    }

    #[test]
    fn unop_bitnot_on_integers_except_bool() {
        for prim in [MirPrim::I8, MirPrim::U64, MirPrim::I128, MirPrim::Isize, MirPrim::Char] {
            assert_eq!(llvm_unop_shape(UnOp::BitNot, prim), Some(LlvmUnOpShape::BitNotInt));
        }
        assert_eq!(llvm_unop_shape(UnOp::BitNot, MirPrim::Bool), None);
        assert_eq!(llvm_unop_shape(UnOp::BitNot, MirPrim::F32), None);
    }

    #[test]
    fn unop_on_str_is_none() {
        for op in [UnOp::Neg, UnOp::Not, UnOp::BitNot] {
            assert_eq!(llvm_unop_shape(op, MirPrim::Str), None);
        }
    }
}
