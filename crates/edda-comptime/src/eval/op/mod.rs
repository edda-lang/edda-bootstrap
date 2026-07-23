//! Binary and unary operator semantics on comptime [`Value`]s.
//!
//! Each apply function takes the operator and the operand value(s) and
//! returns the result `Value`. The result type's width is derived
//! from the operands' widths (matching the typechecker's contract:
//! mixed-width operations don't reach this layer because inference
//! has already unified them).
//!
//! Default arithmetic on integers is *checked*: overflow returns
//! [`OpError::Overflow`] rather than silently wrapping. The locked
//! explicit-mode operators each take their value-producing
//! interpretation: `+%` / `-%` / `*%` wrap modulo two's-complement;
//! `+|` / `-|` / `*|` saturate to the operand width's MIN/MAX. The
//! checked-arithmetic operators (`+?` / `-?` / `*?` / `%?`) are
//! comptime-impure (they originate `err: Overflow`); the typer's
//! comptime-purity rule rejects them at call sites, so they should
//! never reach this layer — when they do, we route through trapping
//! arithmetic as a defence-in-depth.

mod bool;
mod float;
mod int;
mod name;

use edda_syntax::ast::{BinOp, UnOp};
use edda_types::Primitive;

use crate::eval::op::bool::apply_binary_bool;
use crate::eval::op::float::apply_binary_float;
use crate::eval::op::int::{apply_binary_int, bit_not_int, negate_int};
use crate::eval::op::name::{op_name, unary_op_name};
use crate::value::{FloatValue, Value};

/// Reasons a binary or unary application fails. Surfaced through the
/// HIR evaluator as `ComptimeError::Panic` for arithmetic problems
/// (overflow, division by zero) or as a typecheck error for shape
/// problems.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum OpError {
    /// Operands' shapes don't match what the operator expects (e.g.
    /// `&&` on integers, `+` on a String and a bool). Carries the
    /// operator name and the surface description of the offending
    /// operand kinds for the diagnostic.
    KindMismatch {
        /// Operator spelling (`"+"`, `"&&"`, `"-"` (neg), …).
        op: &'static str,
        /// Operand surface kinds, in `(lhs, rhs)` order for binary
        /// ops, `(operand, "")` for unary.
        operands: (String, String),
    },
    /// Integer arithmetic overflowed its width.
    Overflow {
        /// Operator name.
        op: &'static str,
        /// Width of the integer operands.
        width: Primitive,
    },
    /// Division (or modulus) by zero.
    DivByZero {
        /// `"/"` or `"%"`.
        op: &'static str,
    },
    /// Two integer operands had different widths. The HIR evaluator
    /// trusts the typechecker to unify widths before calling here, so
    /// this surfaces only when bypassing the typechecker (manual HIR
    /// construction in tests).
    WidthMismatch {
        /// Operator name.
        op: &'static str,
        /// `(lhs_width, rhs_width)`.
        widths: (Primitive, Primitive),
    },
}

impl OpError {
    /// Diagnostic-ready message describing the failure.
    pub fn message(&self) -> String {
        match self {
            Self::KindMismatch {
                op,
                operands: (l, r),
            } => {
                if r.is_empty() {
                    format!("`{op}` cannot be applied to operand of kind `{l}`")
                } else {
                    format!("`{op}` cannot be applied to `{l}` and `{r}`")
                }
            }
            Self::Overflow { op, width } => {
                format!("`{op}` overflowed `{}`", width.name())
            }
            Self::DivByZero { op } => format!("`{op}` by zero"),
            Self::WidthMismatch {
                op,
                widths: (l, r),
            } => format!(
                "`{op}` operands have mismatched widths `{}` and `{}`",
                l.name(),
                r.name()
            ),
        }
    }
}

/// Apply a binary operator to two comptime values.
pub fn apply_binary(op: BinOp, lhs: &Value, rhs: &Value) -> Result<Value, OpError> {
    match (lhs, rhs) {
        (Value::Int(l), Value::Int(r)) => apply_binary_int(op, *l, *r),
        (Value::Float(l), Value::Float(r)) => apply_binary_float(op, *l, *r),
        (Value::Bool(l), Value::Bool(r)) => apply_binary_bool(op, *l, *r),
        _ => Err(OpError::KindMismatch {
            op: op_name(op),
            operands: (lhs.kind().name().to_string(), rhs.kind().name().to_string()),
        }),
    }
}

/// Apply a unary prefix operator to a comptime value.
pub fn apply_unary(op: UnOp, operand: &Value) -> Result<Value, OpError> {
    match (op, operand) {
        (UnOp::Neg, Value::Int(i)) => negate_int(*i),
        (UnOp::Neg, Value::Float(FloatValue::F32(v))) => Ok(Value::Float(FloatValue::F32(-*v))),
        (UnOp::Neg, Value::Float(FloatValue::F64(v))) => Ok(Value::Float(FloatValue::F64(-*v))),
        (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
        (UnOp::BitNot, Value::Int(i)) => bit_not_int(*i),
        _ => Err(OpError::KindMismatch {
            op: unary_op_name(op),
            operands: (operand.kind().name().to_string(), String::new()),
        }),
    }
}

#[cfg(test)]
use crate::value::IntValue;

#[cfg(test)]
#[path = "../op_tests.rs"]
mod tests;
