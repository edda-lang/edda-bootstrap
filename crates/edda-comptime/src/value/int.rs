//! Comptime integer value carrying its width.
//!
//! [`IntValue`] pairs an [`edda_types::Primitive`] integer-width tag
//! with a 128-bit bit pattern that encodes the value modulo
//! `2^width`. The width tag is reused from `edda-types` so the
//! comptime layer and the type system never disagree about which
//! integer primitives exist; the construction-time invariant is that
//! the width passes [`Primitive::is_integer`].
//!
//! Width-narrowing happens on construction and on layout queries;
//! width-coupling arithmetic lands with the HIR
//! expression evaluator.

use std::fmt;

use edda_types::Primitive;

/// A comptime integer carrying its width.
///
/// For signed widths, `bits` is the two's-complement bit pattern
/// reinterpreted as `u128`; use [`Self::as_i128`] to recover the
/// signed value. For unsigned widths, `bits` is the value directly;
/// use [`Self::as_u128`]. Width-narrowing only happens on construction
/// and on size/align queries.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct IntValue {
    width: Primitive,
    bits: u128,
}

impl IntValue {
    /// Construct a signed comptime integer of the given width.
    ///
    /// `value` is truncated to the width if it overflows. Callers
    /// building `IntValue` from layout calculations
    /// (`size_of`, `align_of`) never overflow; the arithmetic
    /// evaluator is responsible for emitting a diagnostic when
    /// surface arithmetic should not silently wrap.
    pub fn new_signed(width: Primitive, value: i128) -> Self {
        debug_assert!(
            width.is_signed_integer(),
            "new_signed called with non-signed-integer width: {width:?}"
        );
        Self {
            width,
            bits: (value as u128) & width_mask(width),
        }
    }

    /// Construct an unsigned comptime integer of the given width.
    pub fn new_unsigned(width: Primitive, value: u128) -> Self {
        debug_assert!(
            width.is_unsigned_integer(),
            "new_unsigned called with non-unsigned-integer width: {width:?}"
        );
        Self {
            width,
            bits: value & width_mask(width),
        }
    }

    /// Width of this integer.
    pub const fn width(self) -> Primitive {
        self.width
    }

    /// Raw bit pattern, masked to the width. Useful for serialisation;
    /// most callers want [`Self::as_i128`] or [`Self::as_u128`].
    pub const fn bits(self) -> u128 {
        self.bits
    }

    /// Interpret the bits as a signed `i128`.
    ///
    /// Returns `None` if the width is unsigned — at that point the
    /// value might not fit in `i128` (`u128::MAX`), so the caller
    /// must use [`Self::as_u128`].
    pub fn as_i128(self) -> Option<i128> {
        if !self.width.is_signed_integer() {
            return None;
        }
        Some(sign_extend(self.bits, self.width))
    }

    /// Interpret the bits as an unsigned `u128`.
    ///
    /// Returns `None` for signed widths — use [`Self::as_i128`] there.
    pub fn as_u128(self) -> Option<u128> {
        if !self.width.is_unsigned_integer() {
            return None;
        }
        Some(self.bits)
    }
}

impl fmt::Display for IntValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(v) = self.as_i128() {
            write!(f, "{v}_{}", self.width.name())
        } else if let Some(v) = self.as_u128() {
            write!(f, "{v}_{}", self.width.name())
        } else {
            unreachable!("IntValue width is either signed or unsigned")
        }
    }
}

/// Mask covering the low bits of a value at the given width. Used to
/// truncate on construction so that the bit pattern is canonical.
fn width_mask(width: Primitive) -> u128 {
    let bits = width_bits_or_max(width);
    if bits >= 128 { u128::MAX } else { (1u128 << bits) - 1 }
}

/// Bit count of a width when the width is layout-independent.
///
/// `isize` and `usize` widths return 128 here because their layout
/// requires a target config; callers building a value at those widths
/// take responsibility for honouring the target's pointer width
/// before constructing the value.
fn width_bits_or_max(width: Primitive) -> u32 {
    match width {
        Primitive::I8 | Primitive::U8 => 8,
        Primitive::I16 | Primitive::U16 => 16,
        Primitive::I32 | Primitive::U32 => 32,
        Primitive::I64 | Primitive::U64 => 64,
        Primitive::I128 | Primitive::U128 => 128,
        Primitive::Isize | Primitive::Usize => 128,
        // Caller must not pass non-integer widths; debug-asserts in the
        // constructors enforce this contract upstream.
        _ => 128,
    }
}

/// Sign-extend `bits` from the given signed width to `i128`.
fn sign_extend(bits: u128, width: Primitive) -> i128 {
    debug_assert!(width.is_signed_integer());
    let w = width_bits_or_max(width);
    if w >= 128 {
        return bits as i128;
    }
    let sign_bit = 1u128 << (w - 1);
    if bits & sign_bit == 0 {
        bits as i128
    } else {
        let upper = u128::MAX << w;
        (bits | upper) as i128
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_round_trip() {
        let v = IntValue::new_signed(Primitive::I32, -1);
        assert_eq!(v.as_i128(), Some(-1));
        assert_eq!(v.bits(), (-1i32 as u32) as u128);
    }

    #[test]
    fn signed_truncates_overflow() {
        let neg = IntValue::new_signed(Primitive::I8, -1);
        assert_eq!(neg.as_i128(), Some(-1));
        let wrap = IntValue::new_signed(Primitive::I8, 256);
        assert_eq!(wrap.as_i128(), Some(0));
    }

    #[test]
    fn unsigned_round_trip() {
        let v = IntValue::new_unsigned(Primitive::U64, 12_345);
        assert_eq!(v.as_u128(), Some(12_345));
        assert_eq!(v.as_i128(), None);
    }

    #[test]
    fn unsigned_truncates_overflow() {
        let v = IntValue::new_unsigned(Primitive::U8, 257);
        assert_eq!(v.as_u128(), Some(1));
    }

    #[test]
    fn signed_accessor_rejects_unsigned() {
        let v = IntValue::new_unsigned(Primitive::U32, 42);
        assert_eq!(v.as_i128(), None);
    }
}
