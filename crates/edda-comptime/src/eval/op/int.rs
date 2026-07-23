//! Integer binary and unary operator semantics on comptime [`Value`]s.

use edda_syntax::ast::BinOp;
use edda_types::Primitive;

use crate::eval::op::OpError;
use crate::eval::op::name::op_name;
use crate::value::{IntValue, Value};

pub(super) fn apply_binary_int(op: BinOp, lhs: IntValue, rhs: IntValue) -> Result<Value, OpError> {
    if lhs.width() != rhs.width() {
        return Err(OpError::WidthMismatch {
            op: op_name(op),
            widths: (lhs.width(), rhs.width()),
        });
    }
    let width = lhs.width();
    match op {
        BinOp::Add => arith_int(width, lhs, rhs, "+", i128::checked_add, u128::checked_add),
        BinOp::Sub => arith_int(width, lhs, rhs, "-", i128::checked_sub, u128::checked_sub),
        BinOp::Mul => arith_int(width, lhs, rhs, "*", i128::checked_mul, u128::checked_mul),
        // Wrapping arithmetic uses bit-pattern-level two's-complement
        // arithmetic — for the same bit width, signed and unsigned
        // wrapping produce the same byte sequence, so we compute in
        // u128 and let `int_value` re-interpret per signedness.
        BinOp::WrapAdd => Ok(int_value(width, lhs.bits().wrapping_add(rhs.bits()))),
        BinOp::WrapSub => Ok(int_value(width, lhs.bits().wrapping_sub(rhs.bits()))),
        BinOp::WrapMul => Ok(int_value(width, lhs.bits().wrapping_mul(rhs.bits()))),
        // Saturating arithmetic clamps to the operand width's MIN/MAX.
        // We compute in i128/u128 (which has headroom for every width
        // narrower than i128/u128) and then clamp; at the full 128-bit
        // widths the operand range matches i128/u128 itself, so the
        // host's checked / saturating ops detect overflow and we
        // saturate to the width's MIN/MAX based on operand signs.
        BinOp::SatAdd => sat_add_int(width, lhs, rhs),
        BinOp::SatSub => sat_sub_int(width, lhs, rhs),
        BinOp::SatMul => sat_mul_int(width, lhs, rhs),
        // Checked arithmetic: in comptime context the typer's
        // comptime-purity rule rejects `err: Overflow`-bearing call
        // sites, so the comptime evaluator never sees a live `+?`.
        // Defense in depth — route through trapping arith.
        BinOp::CheckAdd => arith_int(width, lhs, rhs, "+?", i128::checked_add, u128::checked_add),
        BinOp::CheckSub => arith_int(width, lhs, rhs, "-?", i128::checked_sub, u128::checked_sub),
        BinOp::CheckMul => arith_int(width, lhs, rhs, "*?", i128::checked_mul, u128::checked_mul),
        // `%?` shares semantics with `%` in comptime: the only overflow case
        // is the runtime `INT_MIN % -1` trap, which `rem_int`'s checked-rem
        // path already surfaces as a comptime error.
        BinOp::CheckMod => rem_int(width, lhs, rhs),
        BinOp::Div => div_int(width, lhs, rhs),
        BinOp::Mod => rem_int(width, lhs, rhs),
        BinOp::Eq => Ok(Value::Bool(lhs.bits() == rhs.bits())),
        BinOp::Ne => Ok(Value::Bool(lhs.bits() != rhs.bits())),
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => Ok(Value::Bool(cmp_int(op, lhs, rhs))),
        BinOp::BitAnd => Ok(int_value(width, lhs.bits() & rhs.bits())),
        BinOp::BitOr => Ok(int_value(width, lhs.bits() | rhs.bits())),
        BinOp::BitXor => Ok(int_value(width, lhs.bits() ^ rhs.bits())),
        BinOp::Shl => shift_int(width, lhs, rhs, false),
        BinOp::Shr => shift_int(width, lhs, rhs, true),
        BinOp::And | BinOp::Or => Err(OpError::KindMismatch {
            op: op_name(op),
            operands: ("int".to_string(), "int".to_string()),
        }),
    }
}

/// Generic checked arithmetic dispatcher, branching on signedness.
fn arith_int(
    width: Primitive,
    lhs: IntValue,
    rhs: IntValue,
    op_str: &'static str,
    signed: fn(i128, i128) -> Option<i128>,
    unsigned: fn(u128, u128) -> Option<u128>,
) -> Result<Value, OpError> {
    if width.is_signed_integer() {
        let l = lhs.as_i128().expect("signed width");
        let r = rhs.as_i128().expect("signed width");
        let v = signed(l, r).ok_or(OpError::Overflow { op: op_str, width })?;
        let truncated = IntValue::new_signed(width, v);
        // Reconstruct the i128 and compare — if the truncation changes
        // the value, the result didn't fit in the width.
        if truncated.as_i128() != Some(v) {
            return Err(OpError::Overflow { op: op_str, width });
        }
        Ok(Value::Int(truncated))
    } else {
        let l = lhs.as_u128().expect("unsigned width");
        let r = rhs.as_u128().expect("unsigned width");
        let v = unsigned(l, r).ok_or(OpError::Overflow { op: op_str, width })?;
        let truncated = IntValue::new_unsigned(width, v);
        if truncated.as_u128() != Some(v) {
            return Err(OpError::Overflow { op: op_str, width });
        }
        Ok(Value::Int(truncated))
    }
}

fn div_int(width: Primitive, lhs: IntValue, rhs: IntValue) -> Result<Value, OpError> {
    if width.is_signed_integer() {
        let l = lhs.as_i128().expect("signed width");
        let r = rhs.as_i128().expect("signed width");
        if r == 0 {
            return Err(OpError::DivByZero { op: "/" });
        }
        let v = l.checked_div(r).ok_or(OpError::Overflow {
            op: "/",
            width,
        })?;
        Ok(Value::Int(IntValue::new_signed(width, v)))
    } else {
        let l = lhs.as_u128().expect("unsigned width");
        let r = rhs.as_u128().expect("unsigned width");
        if r == 0 {
            return Err(OpError::DivByZero { op: "/" });
        }
        Ok(Value::Int(IntValue::new_unsigned(width, l / r)))
    }
}

fn rem_int(width: Primitive, lhs: IntValue, rhs: IntValue) -> Result<Value, OpError> {
    if width.is_signed_integer() {
        let l = lhs.as_i128().expect("signed width");
        let r = rhs.as_i128().expect("signed width");
        if r == 0 {
            return Err(OpError::DivByZero { op: "%" });
        }
        // Match the runtime's Euclidean semantics per B-016: Rust's `%`
        // truncates toward zero (sign of dividend), so lift a negative
        // remainder into `[0, abs(b))` by adding `abs(divisor)`. The
        // `wrapping_*` ops mirror the LLVM-emitted `signed_euclidean_mod`
        // fixup exactly, including `b == INT_MIN` at width i128.
        let raw = l % r;
        let result = if raw < 0 { raw.wrapping_add(r.wrapping_abs()) } else { raw };
        Ok(Value::Int(IntValue::new_signed(width, result)))
    } else {
        let l = lhs.as_u128().expect("unsigned width");
        let r = rhs.as_u128().expect("unsigned width");
        if r == 0 {
            return Err(OpError::DivByZero { op: "%" });
        }
        Ok(Value::Int(IntValue::new_unsigned(width, l % r)))
    }
}

fn cmp_int(op: BinOp, lhs: IntValue, rhs: IntValue) -> bool {
    if lhs.width().is_signed_integer() {
        let l = lhs.as_i128().expect("signed");
        let r = rhs.as_i128().expect("signed");
        match op {
            BinOp::Lt => l < r,
            BinOp::Le => l <= r,
            BinOp::Gt => l > r,
            BinOp::Ge => l >= r,
            _ => unreachable!(),
        }
    } else {
        let l = lhs.as_u128().expect("unsigned");
        let r = rhs.as_u128().expect("unsigned");
        match op {
            BinOp::Lt => l < r,
            BinOp::Le => l <= r,
            BinOp::Gt => l > r,
            BinOp::Ge => l >= r,
            _ => unreachable!(),
        }
    }
}

fn shift_int(
    width: Primitive,
    lhs: IntValue,
    rhs: IntValue,
    right: bool,
) -> Result<Value, OpError> {
    // Shift amount is interpreted as unsigned. For signed widths the
    // value's bits are still well-defined (two's-complement bag).
    let amount = if rhs.width().is_signed_integer() {
        rhs.as_i128().expect("signed")
    } else {
        rhs.as_u128().expect("unsigned") as i128
    };
    if amount < 0 {
        return Err(OpError::Overflow {
            op: if right { ">>" } else { "<<" },
            width,
        });
    }
    let amt = amount as u32;
    let bits = lhs.bits();
    let shifted = if right {
        if width.is_signed_integer() {
            // Arithmetic shift: sign-extend.
            let signed = lhs.as_i128().expect("signed");
            if amt >= 128 {
                if signed < 0 { u128::MAX } else { 0 }
            } else {
                (signed >> amt) as u128
            }
        } else if amt >= 128 {
            0
        } else {
            bits >> amt
        }
    } else if amt >= 128 {
        0
    } else {
        bits << amt
    };
    Ok(int_value(width, shifted))
}

pub(super) fn negate_int(v: IntValue) -> Result<Value, OpError> {
    if !v.width().is_signed_integer() {
        return Err(OpError::KindMismatch {
            op: "-",
            operands: (v.width().name().to_string(), String::new()),
        });
    }
    let value = v.as_i128().expect("signed");
    let negated = value.checked_neg().ok_or(OpError::Overflow {
        op: "-",
        width: v.width(),
    })?;
    let truncated = IntValue::new_signed(v.width(), negated);
    if truncated.as_i128() != Some(negated) {
        return Err(OpError::Overflow {
            op: "-",
            width: v.width(),
        });
    }
    Ok(Value::Int(truncated))
}

pub(super) fn bit_not_int(v: IntValue) -> Result<Value, OpError> {
    Ok(int_value(v.width(), !v.bits()))
}

/// Build a width-aware integer value, taking the same bit pattern
/// for signed and unsigned interpretations.
fn int_value(width: Primitive, bits: u128) -> Value {
    if width.is_signed_integer() {
        // Reinterpret bits as i128 via two's complement; constructor
        // truncates back to the width and discards high bits.
        Value::Int(IntValue::new_signed(width, bits as i128))
    } else {
        Value::Int(IntValue::new_unsigned(width, bits))
    }
}

/// Saturating add — clamp to the operand width's MIN/MAX on overflow.
fn sat_add_int(width: Primitive, lhs: IntValue, rhs: IntValue) -> Result<Value, OpError> {
    if width.is_signed_integer() {
        let l = lhs.as_i128().expect("signed width");
        let r = rhs.as_i128().expect("signed width");
        let min = width_signed_min(width);
        let max = width_signed_max(width);
        let v = match l.checked_add(r) {
            Some(v) => v,
            None => if l >= 0 { max } else { min },
        };
        let clamped = v.clamp(min, max);
        Ok(Value::Int(IntValue::new_signed(width, clamped)))
    } else {
        let l = lhs.as_u128().expect("unsigned width");
        let r = rhs.as_u128().expect("unsigned width");
        let max = width_unsigned_max(width);
        let v = l.saturating_add(r);
        Ok(Value::Int(IntValue::new_unsigned(width, v.min(max))))
    }
}

/// Saturating sub — clamp to the operand width's MIN/MAX on overflow.
fn sat_sub_int(width: Primitive, lhs: IntValue, rhs: IntValue) -> Result<Value, OpError> {
    if width.is_signed_integer() {
        let l = lhs.as_i128().expect("signed width");
        let r = rhs.as_i128().expect("signed width");
        let min = width_signed_min(width);
        let max = width_signed_max(width);
        let v = match l.checked_sub(r) {
            Some(v) => v,
            None => if r > 0 { min } else { max },
        };
        let clamped = v.clamp(min, max);
        Ok(Value::Int(IntValue::new_signed(width, clamped)))
    } else {
        let l = lhs.as_u128().expect("unsigned width");
        let r = rhs.as_u128().expect("unsigned width");
        let max = width_unsigned_max(width);
        let v = l.saturating_sub(r);
        Ok(Value::Int(IntValue::new_unsigned(width, v.min(max))))
    }
}

/// Saturating mul — clamp to the operand width's MIN/MAX on overflow.
fn sat_mul_int(width: Primitive, lhs: IntValue, rhs: IntValue) -> Result<Value, OpError> {
    if width.is_signed_integer() {
        let l = lhs.as_i128().expect("signed width");
        let r = rhs.as_i128().expect("signed width");
        let min = width_signed_min(width);
        let max = width_signed_max(width);
        // `signs_differ` is computed before clamping so the saturation
        // direction matches the LLVM backend's select pattern: same
        // sign → MAX, different sign → MIN.
        let signs_differ = (l < 0) ^ (r < 0);
        let v = match l.checked_mul(r) {
            Some(v) => v,
            None => if signs_differ { min } else { max },
        };
        let clamped = if v > max {
            max
        } else if v < min {
            min
        } else {
            v
        };
        Ok(Value::Int(IntValue::new_signed(width, clamped)))
    } else {
        let l = lhs.as_u128().expect("unsigned width");
        let r = rhs.as_u128().expect("unsigned width");
        let max = width_unsigned_max(width);
        let v = l.saturating_mul(r);
        Ok(Value::Int(IntValue::new_unsigned(width, v.min(max))))
    }
}

/// Signed-width MIN as `i128`. `isize` mirrors `i64` per the v0.1
/// target matrix's pointer width.
fn width_signed_min(width: Primitive) -> i128 {
    match width {
        Primitive::I8 => i8::MIN as i128,
        Primitive::I16 => i16::MIN as i128,
        Primitive::I32 => i32::MIN as i128,
        Primitive::I64 | Primitive::Isize => i64::MIN as i128,
        Primitive::I128 => i128::MIN,
        _ => unreachable!("width_signed_min on non-signed-integer width {width:?}"),
    }
}

/// Signed-width MAX as `i128`.
fn width_signed_max(width: Primitive) -> i128 {
    match width {
        Primitive::I8 => i8::MAX as i128,
        Primitive::I16 => i16::MAX as i128,
        Primitive::I32 => i32::MAX as i128,
        Primitive::I64 | Primitive::Isize => i64::MAX as i128,
        Primitive::I128 => i128::MAX,
        _ => unreachable!("width_signed_max on non-signed-integer width {width:?}"),
    }
}

/// Unsigned-width MAX as `u128`.
fn width_unsigned_max(width: Primitive) -> u128 {
    match width {
        Primitive::U8 => u8::MAX as u128,
        Primitive::U16 => u16::MAX as u128,
        Primitive::U32 => u32::MAX as u128,
        Primitive::U64 | Primitive::Usize => u64::MAX as u128,
        Primitive::U128 => u128::MAX,
        _ => unreachable!("width_unsigned_max on non-unsigned-integer width {width:?}"),
    }
}
