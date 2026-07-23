//! Floating-point binary operator semantics on comptime [`Value`]s.

use edda_syntax::ast::BinOp;

use crate::eval::op::OpError;
use crate::eval::op::name::op_name;
use crate::value::{FloatValue, Value};

pub(super) fn apply_binary_float(op: BinOp, lhs: FloatValue, rhs: FloatValue) -> Result<Value, OpError> {
    match (lhs, rhs) {
        (FloatValue::F32(l), FloatValue::F32(r)) => Ok(float_binary_f32(op, l, r)?),
        (FloatValue::F64(l), FloatValue::F64(r)) => Ok(float_binary_f64(op, l, r)?),
        _ => Err(OpError::KindMismatch {
            op: op_name(op),
            operands: ("float".to_string(), "float".to_string()),
        }),
    }
}

fn float_binary_f32(op: BinOp, l: f32, r: f32) -> Result<Value, OpError> {
    Ok(match op {
        BinOp::Add => Value::Float(FloatValue::F32(l + r)),
        BinOp::Sub => Value::Float(FloatValue::F32(l - r)),
        BinOp::Mul => Value::Float(FloatValue::F32(l * r)),
        BinOp::Div => Value::Float(FloatValue::F32(l / r)),
        BinOp::Eq => Value::Bool(l == r),
        BinOp::Ne => Value::Bool(l != r),
        BinOp::Lt => Value::Bool(l < r),
        BinOp::Le => Value::Bool(l <= r),
        BinOp::Gt => Value::Bool(l > r),
        BinOp::Ge => Value::Bool(l >= r),
        _ => {
            return Err(OpError::KindMismatch {
                op: op_name(op),
                operands: ("float".to_string(), "float".to_string()),
            });
        }
    })
}

fn float_binary_f64(op: BinOp, l: f64, r: f64) -> Result<Value, OpError> {
    Ok(match op {
        BinOp::Add => Value::Float(FloatValue::F64(l + r)),
        BinOp::Sub => Value::Float(FloatValue::F64(l - r)),
        BinOp::Mul => Value::Float(FloatValue::F64(l * r)),
        BinOp::Div => Value::Float(FloatValue::F64(l / r)),
        BinOp::Eq => Value::Bool(l == r),
        BinOp::Ne => Value::Bool(l != r),
        BinOp::Lt => Value::Bool(l < r),
        BinOp::Le => Value::Bool(l <= r),
        BinOp::Gt => Value::Bool(l > r),
        BinOp::Ge => Value::Bool(l >= r),
        _ => {
            return Err(OpError::KindMismatch {
                op: op_name(op),
                operands: ("float".to_string(), "float".to_string()),
            });
        }
    })
}
