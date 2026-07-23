//! Integer-literal narrowing helpers (`T-IntLit-Check`).
//!
//! `Literal::Int.value` is `u128`. The literal itself carries no sign
//! — unary negation is handled at the [`HirExprKind::Unary`] level.
//! [`fits_in_primitive`] checks the literal's positive range against
//! the target primitive type. [`fits_in_primitive_negated`] is the
//! `Unary::Neg`-aware variant: when a literal is the immediate operand
//! of unary minus the admissible range for signed targets widens by
//! one to capture `T::MIN` (whose magnitude is `T::MAX_POSITIVE + 1`).
//! Unsigned targets admit only `0` under negation. The negation-aware
//! call site lives in [`super::op::check_unary`].

use crate::prim::Primitive;

/// Returns `true` iff `value` fits in the positive range of `prim`.
///
/// `prim` must be an integer primitive (signed or unsigned); for any
/// non-integer primitive this returns `false`. For signed types the
/// limit is `T::MAX_POSITIVE = 2^(bits-1) - 1`; for unsigned, `2^bits - 1`
/// (saturating at `u128::MAX` for `u128` / `usize` on 128-bit-pointer
/// targets — `isize`/`usize` are treated as 64-bit-wide for range
/// purposes, matching the dominant target).
pub(super) fn fits_in_primitive(value: u128, prim: Primitive) -> bool {
    match prim {
        Primitive::I8 => value <= i8::MAX as u128,
        Primitive::I16 => value <= i16::MAX as u128,
        Primitive::I32 => value <= i32::MAX as u128,
        Primitive::I64 => value <= i64::MAX as u128,
        Primitive::I128 => value <= i128::MAX as u128,
        Primitive::Isize => value <= i64::MAX as u128, // see module doc — treated as 64-bit
        Primitive::U8 => value <= u8::MAX as u128,
        Primitive::U16 => value <= u16::MAX as u128,
        Primitive::U32 => value <= u32::MAX as u128,
        Primitive::U64 => value <= u64::MAX as u128,
        Primitive::U128 => true,
        Primitive::Usize => value <= u64::MAX as u128,
        _ => false,
    }
}

/// Returns `true` iff `value` is admissible as the magnitude of a
/// negated integer literal against `prim`.
///
/// Signed integer targets admit `0 ..= 2^(bits-1)`. The extra `+1`
/// over [`fits_in_primitive`] captures `T::MIN` whose magnitude is
/// one larger than `T::MAX`. Unsigned targets admit only `0` (any
/// other negated unsigned value is out of range).
pub(super) fn fits_in_primitive_negated(value: u128, prim: Primitive) -> bool {
    match prim {
        Primitive::I8 => value <= i8::MAX as u128 + 1,
        Primitive::I16 => value <= i16::MAX as u128 + 1,
        Primitive::I32 => value <= i32::MAX as u128 + 1,
        Primitive::I64 => value <= i64::MAX as u128 + 1,
        Primitive::I128 => value <= i128::MAX as u128 + 1,
        Primitive::Isize => value <= i64::MAX as u128 + 1,
        Primitive::U8
        | Primitive::U16
        | Primitive::U32
        | Primitive::U64
        | Primitive::U128
        | Primitive::Usize => value == 0,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_positive_ranges() {
        assert!(fits_in_primitive(0, Primitive::I8));
        assert!(fits_in_primitive(127, Primitive::I8));
        assert!(!fits_in_primitive(128, Primitive::I8));
        assert!(fits_in_primitive(32_767, Primitive::I16));
        assert!(!fits_in_primitive(32_768, Primitive::I16));
        assert!(fits_in_primitive(i32::MAX as u128, Primitive::I32));
        assert!(!fits_in_primitive(i32::MAX as u128 + 1, Primitive::I32));
    }

    #[test]
    fn unsigned_ranges() {
        assert!(fits_in_primitive(255, Primitive::U8));
        assert!(!fits_in_primitive(256, Primitive::U8));
        assert!(fits_in_primitive(u64::MAX as u128, Primitive::U64));
        assert!(!fits_in_primitive(u64::MAX as u128 + 1, Primitive::U64));
        assert!(fits_in_primitive(u128::MAX, Primitive::U128));
    }

    #[test]
    fn pointer_sized_treated_as_64_bit() {
        assert!(fits_in_primitive(i64::MAX as u128, Primitive::Isize));
        assert!(!fits_in_primitive(i64::MAX as u128 + 1, Primitive::Isize));
        assert!(fits_in_primitive(u64::MAX as u128, Primitive::Usize));
        assert!(!fits_in_primitive(u64::MAX as u128 + 1, Primitive::Usize));
    }

    #[test]
    fn non_integer_primitives_reject() {
        for p in [
            Primitive::F32,
            Primitive::F64,
            Primitive::Bool,
            Primitive::Unit,
            Primitive::Never,
            Primitive::Type,
            Primitive::String,
        ] {
            assert!(!fits_in_primitive(0, p), "{p:?} should reject");
        }
    }
}
