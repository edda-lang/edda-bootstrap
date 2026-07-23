//! Numeric `as` cast evaluation for the comptime HIR evaluator.
//!
//! `expr as T [mode]` over the numeric-to-numeric surface the
//! typechecker admits (`infer::comp::cast` — integer↔integer,
//! integer↔float, float↔float). Cast modes
//! ([`CastMode`](edda_syntax::ast::CastMode)) only carry distinct
//! semantics for *narrowing* integer→integer casts, mirroring the MIR
//! lowering in `edda-mir/src/lower/expr/cast`:
//! - `Trap` (bare `as`) — comptime-panics when the source value does
//!   not fit the destination range.
//! - `Wrapping` — two's-complement truncation to the destination width.
//! - `Saturating` — clamps to the destination's MIN/MAX.
//! - `Checked` — originates `err: Overflow`, so the comptime-purity
//!   rule rejects it before it reaches here; as defence-in-depth it
//!   behaves like `Trap` (same posture the `op` module takes for the
//!   checked arithmetic operators).
//!
//! Widening / same-width-same-sign int casts, int↔float, and float↔float
//! are total: the mode is irrelevant and the value converts directly.

use std::cmp::Ordering;

use edda_span::Span;
use edda_syntax::ast::CastMode;
use edda_types::{HirExpr, Primitive, TyId, TyKind};

use crate::eval::expr::diag::{push_not_supported, push_panic};
use crate::eval::expr::{EvalCx, eval_expr};
use crate::value::{FloatValue, IntValue, Value};

pub(super) fn eval_cast(
    inner: &HirExpr,
    target_ty: TyId,
    mode: CastMode,
    span: Span,
    cx: &mut EvalCx<'_>,
) -> Option<Value> {
    let src = eval_expr(inner, cx)?;
    let TyKind::Primitive(dst) = cx.ty_interner.kind(target_ty) else {
        push_not_supported(cx.diags, span, "cast to a non-primitive type");
        return None;
    };
    let dst = *dst;
    if !dst.is_numeric() {
        push_not_supported(cx.diags, span, "cast to a non-numeric type");
        return None;
    }
    match src {
        Value::Int(iv) => Some(cast_from_int(iv, dst, mode, span, cx)?),
        Value::Float(fv) => Some(cast_from_float(fv, dst)),
        other => {
            push_panic(
                cx.diags,
                span,
                format!("cannot cast value of kind `{}`", other.kind().name()),
            );
            None
        }
    }
}

fn cast_from_int(
    iv: IntValue,
    dst: Primitive,
    mode: CastMode,
    span: Span,
    cx: &mut EvalCx<'_>,
) -> Option<Value> {
    if dst.is_float() {
        return Some(Value::Float(int_to_float(iv, dst)));
    }
    // dst is an integer primitive (numeric, non-float).
    let raw = raw_bits(iv);
    let wrapped = wrap_to(dst, raw);
    let sv = sign_mag(iv);
    let (min_sm, max_sm) = dst_bounds(dst);
    let below = cmp_sm(sv, min_sm) == Ordering::Less;
    let above = cmp_sm(sv, max_sm) == Ordering::Greater;

    match mode {
        CastMode::Wrapping => Some(Value::Int(wrapped)),
        CastMode::Saturating => {
            if above {
                Some(Value::Int(dst_bound(dst, true)))
            } else if below {
                Some(Value::Int(dst_bound(dst, false)))
            } else {
                Some(Value::Int(wrapped))
            }
        }
        CastMode::Trap | CastMode::Checked => {
            if below || above {
                push_panic(
                    cx.diags,
                    span,
                    format!(
                        "cast of `{iv}` to `{}` is out of range",
                        dst.name()
                    ),
                );
                None
            } else {
                Some(Value::Int(wrapped))
            }
        }
    }
}

fn cast_from_float(fv: FloatValue, dst: Primitive) -> Value {
    let val = match fv {
        FloatValue::F32(v) => v as f64,
        FloatValue::F64(v) => v,
    };
    if dst.is_float() {
        return Value::Float(match dst {
            Primitive::F32 => FloatValue::F32(val as f32),
            _ => FloatValue::F64(val),
        });
    }
    // float → integer: truncate toward zero (Rust `as` saturates the
    // float-to-int conversion and maps NaN to 0), then place the value
    // into the destination width.
    let truncated = val as i128;
    Value::Int(wrap_to(dst, truncated as u128))
}

fn int_to_float(iv: IntValue, dst: Primitive) -> FloatValue {
    let val = if let Some(s) = iv.as_i128() {
        s as f64
    } else {
        iv.as_u128().unwrap_or(0) as f64
    };
    match dst {
        Primitive::F32 => FloatValue::F32(val as f32),
        _ => FloatValue::F64(val),
    }
}

/// Bit width of an integer primitive. `isize`/`usize` resolve to 64 —
/// every v0.1 target triple has a 64-bit address space, matching the
/// MIR lowering's `int_width_signed`.
fn int_bits(p: Primitive) -> u32 {
    match p {
        Primitive::I8 | Primitive::U8 => 8,
        Primitive::I16 | Primitive::U16 => 16,
        Primitive::I32 | Primitive::U32 => 32,
        Primitive::I64 | Primitive::U64 | Primitive::Isize | Primitive::Usize => 64,
        Primitive::I128 | Primitive::U128 => 128,
        _ => 128,
    }
}

/// The source value's mathematical integer as a sign-extended 128-bit
/// two's-complement pattern. Used as the raw input for width-truncating
/// construction of the destination value.
fn raw_bits(iv: IntValue) -> u128 {
    if let Some(s) = iv.as_i128() {
        s as u128
    } else {
        iv.as_u128().unwrap_or(0)
    }
}

/// `(is_negative, magnitude)` of the source value — a signed/unsigned
/// agnostic representation that compares correctly across widths and
/// the full `u128` range (where the value may exceed `i128::MAX`).
fn sign_mag(iv: IntValue) -> (bool, u128) {
    if let Some(s) = iv.as_i128() {
        if s < 0 {
            (true, s.unsigned_abs())
        } else {
            (false, s as u128)
        }
    } else {
        (false, iv.as_u128().unwrap_or(0))
    }
}

fn cmp_sm(a: (bool, u128), b: (bool, u128)) -> Ordering {
    match (a.0, b.0) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => a.1.cmp(&b.1),
        (true, true) => b.1.cmp(&a.1),
    }
}

/// `(min, max)` of the destination integer primitive as sign-magnitude.
fn dst_bounds(dst: Primitive) -> ((bool, u128), (bool, u128)) {
    let bits = int_bits(dst);
    if dst.is_signed_integer() {
        let max = (1u128 << (bits - 1)) - 1;
        let min_mag = 1u128 << (bits - 1);
        ((true, min_mag), (false, max))
    } else {
        let max = if bits >= 128 {
            u128::MAX
        } else {
            (1u128 << bits) - 1
        };
        ((false, 0), (false, max))
    }
}

/// Construct the destination MIN (`upper = false`) or MAX
/// (`upper = true`) value — the saturating-clamp endpoints.
fn dst_bound(dst: Primitive, upper: bool) -> IntValue {
    let bits = int_bits(dst);
    if dst.is_signed_integer() {
        let pattern = if upper {
            (1u128 << (bits - 1)) - 1
        } else {
            1u128 << (bits - 1)
        };
        wrap_to(dst, pattern)
    } else if upper {
        let pattern = if bits >= 128 {
            u128::MAX
        } else {
            (1u128 << bits) - 1
        };
        wrap_to(dst, pattern)
    } else {
        wrap_to(dst, 0)
    }
}

/// Truncate a 128-bit pattern to the destination width and build the
/// destination [`IntValue`]. For signed destinations the masked bits are
/// reinterpreted as a two's-complement value of the destination width.
fn wrap_to(dst: Primitive, raw: u128) -> IntValue {
    let bits = int_bits(dst);
    let masked = if bits >= 128 {
        raw
    } else {
        raw & ((1u128 << bits) - 1)
    };
    if dst.is_signed_integer() {
        IntValue::new_signed(dst, sign_extend_to_i128(masked, bits))
    } else {
        IntValue::new_unsigned(dst, masked)
    }
}

fn sign_extend_to_i128(masked: u128, width: u32) -> i128 {
    if width >= 128 {
        return masked as i128;
    }
    let sign = 1u128 << (width - 1);
    if masked & sign == 0 {
        masked as i128
    } else {
        (masked | (u128::MAX << width)) as i128
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn si(p: Primitive, v: i128) -> IntValue {
        IntValue::new_signed(p, v)
    }
    fn ui(p: Primitive, v: u128) -> IntValue {
        IntValue::new_unsigned(p, v)
    }

    #[test]
    fn widen_signed_preserves_value() {
        let v = wrap_to(Primitive::I32, raw_bits(si(Primitive::I8, -5)));
        assert_eq!(v.as_i128(), Some(-5));
    }

    #[test]
    fn narrow_wrapping_truncates() {
        // 300 as i8 wrapping -> 300 mod 256 = 44
        let v = wrap_to(Primitive::I8, raw_bits(si(Primitive::I32, 300)));
        assert_eq!(v.as_i128(), Some(44));
    }

    #[test]
    fn saturating_clamps_high_and_low() {
        let max = dst_bound(Primitive::I8, true);
        assert_eq!(max.as_i128(), Some(127));
        let min = dst_bound(Primitive::I8, false);
        assert_eq!(min.as_i128(), Some(-128));
        let umax = dst_bound(Primitive::U8, true);
        assert_eq!(umax.as_u128(), Some(255));
    }

    #[test]
    fn signed_to_unsigned_wraps_two_complement() {
        // -1 as u8 wrapping -> 255
        let v = wrap_to(Primitive::U8, raw_bits(si(Primitive::I32, -1)));
        assert_eq!(v.as_u128(), Some(255));
    }

    #[test]
    fn range_classification() {
        let (min_sm, max_sm) = dst_bounds(Primitive::I8);
        assert_eq!(cmp_sm(sign_mag(si(Primitive::I32, 200)), max_sm), Ordering::Greater);
        assert_eq!(cmp_sm(sign_mag(si(Primitive::I32, -200)), min_sm), Ordering::Less);
        assert_eq!(cmp_sm(sign_mag(si(Primitive::I32, 100)), max_sm), Ordering::Less);
    }

    #[test]
    fn i128_bounds_do_not_overflow() {
        let (min_sm, max_sm) = dst_bounds(Primitive::I128);
        assert_eq!(max_sm, (false, i128::MAX as u128));
        assert_eq!(min_sm, (true, 1u128 << 127));
        let max = dst_bound(Primitive::I128, true);
        assert_eq!(max.as_i128(), Some(i128::MAX));
        let min = dst_bound(Primitive::I128, false);
        assert_eq!(min.as_i128(), Some(i128::MIN));
    }

    #[test]
    fn u128_max_classified_above_signed_dst() {
        let (_, max_sm) = dst_bounds(Primitive::I64);
        assert_eq!(cmp_sm(sign_mag(ui(Primitive::U128, u128::MAX)), max_sm), Ordering::Greater);
    }

    #[test]
    fn int_to_float_then_back() {
        let f = int_to_float(si(Primitive::I32, 42), Primitive::F64);
        let back = cast_from_float(f, Primitive::I32);
        match back {
            Value::Int(iv) => assert_eq!(iv.as_i128(), Some(42)),
            _ => panic!("expected int"),
        }
    }
}
