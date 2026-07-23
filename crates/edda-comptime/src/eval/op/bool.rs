//! Boolean binary operator semantics on comptime [`Value`]s.

use edda_syntax::ast::BinOp;

use crate::eval::op::OpError;
use crate::eval::op::name::op_name;
use crate::value::Value;

pub(super) fn apply_binary_bool(op: BinOp, l: bool, r: bool) -> Result<Value, OpError> {
    Ok(match op {
        BinOp::And => Value::Bool(l && r),
        BinOp::Or => Value::Bool(l || r),
        BinOp::Eq => Value::Bool(l == r),
        BinOp::Ne => Value::Bool(l != r),
        BinOp::BitAnd => Value::Bool(l & r),
        BinOp::BitOr => Value::Bool(l | r),
        BinOp::BitXor => Value::Bool(l ^ r),
        _ => {
            return Err(OpError::KindMismatch {
                op: op_name(op),
                operands: ("bool".to_string(), "bool".to_string()),
            });
        }
    })
}
