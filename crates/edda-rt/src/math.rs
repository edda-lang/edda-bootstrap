//! Floating-point math intrinsics: `__edda_<op>_f64` / `_f32` scalar shims.
//!
//! Each symbol is a thin C-ABI wrapper over the corresponding Rust/libm
//! operation. They carry no allocation and no trap: domain errors follow
//! IEEE-754 (e.g. `sqrt(-1.0)` → NaN, `ln(0.0)` → -inf), never a panic.
//!
//! Two stability tiers, surfaced for the `std.math.float` `@abi` wrappers:
//!   - correctly-rounded / exact (`sqrt` `floor` `ceil` `round` `trunc` `fmod`)
//!     are bit-identical across targets → stable-eligible;
//!   - transcendentals (`sin` `cos` `tan` `exp` `ln` `log2` `pow` `atan2`
//!     `hypot` `cbrt`) route to the platform libm and are NOT bit-identical
//!     across targets → unstable.
//!
//! Not comptime-evaluable: `cteval` rejects `function_extern`, so these cannot
//! appear in `where` / `requires` / `ensures` predicates (that is why
//! `std.math.constants` precomputes its literals).

/// IEEE-754 square root.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_sqrt_f64(value: f64) -> f64 {
    value.sqrt()
}

/// IEEE-754 square root (single precision).
#[unsafe(no_mangle)]
pub extern "C" fn __edda_sqrt_f32(value: f32) -> f32 {
    value.sqrt()
}

/// Round toward negative infinity.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_floor_f64(value: f64) -> f64 {
    value.floor()
}

/// Round toward negative infinity (single precision).
#[unsafe(no_mangle)]
pub extern "C" fn __edda_floor_f32(value: f32) -> f32 {
    value.floor()
}

/// Round toward positive infinity.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_ceil_f64(value: f64) -> f64 {
    value.ceil()
}

/// Round toward positive infinity (single precision).
#[unsafe(no_mangle)]
pub extern "C" fn __edda_ceil_f32(value: f32) -> f32 {
    value.ceil()
}

/// Round to nearest integer, ties away from zero.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_round_f64(value: f64) -> f64 {
    value.round()
}

/// Round to nearest integer, ties away from zero (single precision).
#[unsafe(no_mangle)]
pub extern "C" fn __edda_round_f32(value: f32) -> f32 {
    value.round()
}

/// Truncate toward zero.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_trunc_f64(value: f64) -> f64 {
    value.trunc()
}

/// Truncate toward zero (single precision).
#[unsafe(no_mangle)]
pub extern "C" fn __edda_trunc_f32(value: f32) -> f32 {
    value.trunc()
}

/// Truncated floating-point remainder (C `fmod`).
#[unsafe(no_mangle)]
pub extern "C" fn __edda_fmod_f64(a: f64, b: f64) -> f64 {
    a % b
}

/// Truncated floating-point remainder (C `fmod`, single precision).
#[unsafe(no_mangle)]
pub extern "C" fn __edda_fmod_f32(a: f32, b: f32) -> f32 {
    a % b
}

/// Sine of an angle in radians.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_sin_f64(value: f64) -> f64 {
    value.sin()
}

/// Sine of an angle in radians (single precision).
#[unsafe(no_mangle)]
pub extern "C" fn __edda_sin_f32(value: f32) -> f32 {
    value.sin()
}

/// Cosine of an angle in radians.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_cos_f64(value: f64) -> f64 {
    value.cos()
}

/// Cosine of an angle in radians (single precision).
#[unsafe(no_mangle)]
pub extern "C" fn __edda_cos_f32(value: f32) -> f32 {
    value.cos()
}

/// Tangent of an angle in radians.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_tan_f64(value: f64) -> f64 {
    value.tan()
}

/// Tangent of an angle in radians (single precision).
#[unsafe(no_mangle)]
pub extern "C" fn __edda_tan_f32(value: f32) -> f32 {
    value.tan()
}

/// Exponential `e^value`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_exp_f64(value: f64) -> f64 {
    value.exp()
}

/// Exponential `e^value` (single precision).
#[unsafe(no_mangle)]
pub extern "C" fn __edda_exp_f32(value: f32) -> f32 {
    value.exp()
}

/// Natural logarithm.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_ln_f64(value: f64) -> f64 {
    value.ln()
}

/// Natural logarithm (single precision).
#[unsafe(no_mangle)]
pub extern "C" fn __edda_ln_f32(value: f32) -> f32 {
    value.ln()
}

/// Base-2 logarithm.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_log2_f64(value: f64) -> f64 {
    value.log2()
}

/// Base-2 logarithm (single precision).
#[unsafe(no_mangle)]
pub extern "C" fn __edda_log2_f32(value: f32) -> f32 {
    value.log2()
}

/// Cube root.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_cbrt_f64(value: f64) -> f64 {
    value.cbrt()
}

/// Cube root (single precision).
#[unsafe(no_mangle)]
pub extern "C" fn __edda_cbrt_f32(value: f32) -> f32 {
    value.cbrt()
}

/// `base` raised to the power `exp`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_pow_f64(base: f64, exp: f64) -> f64 {
    base.powf(exp)
}

/// `base` raised to the power `exp` (single precision).
#[unsafe(no_mangle)]
pub extern "C" fn __edda_pow_f32(base: f32, exp: f32) -> f32 {
    base.powf(exp)
}

/// Two-argument arctangent `atan2(y, x)`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_atan2_f64(y: f64, x: f64) -> f64 {
    y.atan2(x)
}

/// Two-argument arctangent `atan2(y, x)` (single precision).
#[unsafe(no_mangle)]
pub extern "C" fn __edda_atan2_f32(y: f32, x: f32) -> f32 {
    y.atan2(x)
}

/// Euclidean distance `sqrt(x*x + y*y)`.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_hypot_f64(x: f64, y: f64) -> f64 {
    x.hypot(y)
}

/// Euclidean distance `sqrt(x*x + y*y)` (single precision).
#[unsafe(no_mangle)]
pub extern "C" fn __edda_hypot_f32(x: f32, y: f32) -> f32 {
    x.hypot(y)
}

// Wrapping float -> 128-bit integer cast helpers.
//
// The native backend has no hardware float->i128/u128 instruction, so every
// `f as i128|u128` cast lowers to a runtime call. The trapping/saturating/checked
// modes call compiler-rt's `__fix{,uns}{d,s}fti` (saturate-on-overflow, NaN->0);
// the *wrapping* mode needs distinct, edda-owned symbols because compiler-rt
// saturates rather than wraps. The backend keys both wrapping symbols on the
// source float width only (`df`=f64, `sf`=f32): the low 128 bits of a
// round-toward-zero conversion are identical for signed and unsigned
// destinations, so one symbol per source width serves both i128 and u128.
//
// Semantics: round toward zero to a mathematical integer, then reduce mod 2^128
// (the `wrapping` table entry, `01-syntax.md` §Arithmetic). Non-finite inputs
// (NaN, ±inf) have no mod-2^128 value and yield 0.

/// Low 128 bits of `value` rounded toward zero, as an unsigned magnitude+wrap.
/// Shared core of the `df` cast helper; the `sf` helper widens f32->f64 first
/// (lossless), so both reuse this single decomposition.
fn wrap_f64_to_u128(value: f64) -> u128 {
    let bits = value.to_bits();
    let neg = (bits >> 63) & 1 == 1;
    let exp = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & 0x000f_ffff_ffff_ffff;
    if exp == 0x7ff {
        return 0;
    }
    if exp == 0 {
        return 0;
    }
    let signif: u128 = (frac as u128) | (1u128 << 52);
    let shift = (exp - 1023) - 52;
    let mag: u128 = if shift >= 128 {
        0
    } else if shift >= 0 {
        signif << (shift as u32)
    } else {
        let rs = (-shift) as u32;
        if rs >= 128 { 0 } else { signif >> rs }
    };
    if neg { mag.wrapping_neg() } else { mag }
}

/// Wrapping `f64 as i128|u128` cast (compiler-rt `__fixdfti` shape, wrap not saturate).
#[unsafe(no_mangle)]
pub extern "C" fn __edda_fixwrapdfti(value: f64) -> i128 {
    wrap_f64_to_u128(value) as i128
}

/// Wrapping `f32 as i128|u128` cast (compiler-rt `__fixsfti` shape, wrap not saturate).
#[unsafe(no_mangle)]
pub extern "C" fn __edda_fixwrapsfti(value: f32) -> i128 {
    wrap_f64_to_u128(value as f64) as i128
}

#[cfg(test)]
mod fixwrap_tests {
    use super::*;

    #[test]
    fn in_range_round_toward_zero() {
        assert_eq!(__edda_fixwrapdfti(5.0), 5);
        assert_eq!(__edda_fixwrapsfti(5.0), 5);
        assert_eq!(__edda_fixwrapdfti(0.0), 0);
        assert_eq!(__edda_fixwrapdfti(-0.0), 0);
        assert_eq!(__edda_fixwrapdfti(123_456_789.0), 123_456_789);
        assert_eq!(__edda_fixwrapdfti(3.9), 3);
        assert_eq!(__edda_fixwrapdfti(-3.9), -3);
        assert_eq!(__edda_fixwrapdfti(-7.0), -7);
        assert_eq!(__edda_fixwrapsfti(-7.0), -7);
    }

    #[test]
    fn non_finite_is_zero() {
        assert_eq!(__edda_fixwrapdfti(f64::NAN), 0);
        assert_eq!(__edda_fixwrapdfti(f64::INFINITY), 0);
        assert_eq!(__edda_fixwrapdfti(f64::NEG_INFINITY), 0);
        assert_eq!(__edda_fixwrapsfti(f32::NAN), 0);
        assert_eq!(__edda_fixwrapsfti(f32::INFINITY), 0);
    }

    #[test]
    fn unsigned_view_matches_low_128_bits() {
        // The same symbol backs `as u128 wrapping`; the bits reinterpret as u128.
        assert_eq!(__edda_fixwrapdfti(5.0) as u128, 5u128);
        assert_eq!(__edda_fixwrapdfti(-1.0) as u128, u128::MAX);
    }

    #[test]
    fn wraps_at_and_above_signed_max() {
        let two_127 = 2.0f64.powi(127);
        assert_eq!(__edda_fixwrapdfti(two_127), i128::MIN);
        assert_eq!(__edda_fixwrapdfti(two_127) as u128, 1u128 << 127);
    }

    #[test]
    fn multiple_of_2pow128_wraps_to_zero() {
        assert_eq!(__edda_fixwrapdfti(2.0f64.powi(128)), 0);
        assert_eq!(__edda_fixwrapdfti(2.0f64.powi(200)), 0);
    }
}
