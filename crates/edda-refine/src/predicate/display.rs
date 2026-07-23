//! `Display` implementations for the predicate IR types.
//!
//! Split from `predicate/mod.rs` to keep the IR module focused on the data
//! definitions and smart constructors. The Display surface is the
//! user-facing pretty-print used by diagnostics and by
//! [`Obligation::new`](crate::Obligation::new)'s `predicate_text` fallback.

use std::fmt;

use super::{ArithOp, BoolBinOp, CmpOp, IntLitValue, Predicate};

impl fmt::Display for ArithOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ArithOp::Add => "+",
            ArithOp::Sub => "-",
        })
    }
}

impl fmt::Display for CmpOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CmpOp::Eq => "==",
            CmpOp::Ne => "!=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
        })
    }
}

impl fmt::Display for BoolBinOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            BoolBinOp::And => "&&",
            BoolBinOp::Or => "||",
        })
    }
}

impl fmt::Display for IntLitValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IntLitValue::Signed(v) => v.fmt(f),
            IntLitValue::Unsigned(v) => v.fmt(f),
        }
    }
}

impl fmt::Display for Predicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Predicate::Var(v) => f.write_str(v.name.as_str()),
            Predicate::IntLit(lit) => lit.value.fmt(f),
            Predicate::BoolLit(b) => f.write_str(if *b { "true" } else { "false" }),
            Predicate::Arith { op, lhs, rhs } => write!(f, "({lhs} {op} {rhs})"),
            Predicate::Neg(operand) => write!(f, "(-{operand})"),
            Predicate::MulLit { c, expr } => write!(f, "({} * {expr})", c.value),
            Predicate::DivLit { expr, c } => write!(f, "({expr} / {})", c.value),
            Predicate::ModLit { expr, c } => write!(f, "({expr} % {})", c.value),
            Predicate::Cmp { op, lhs, rhs } => write!(f, "({lhs} {op} {rhs})"),
            Predicate::BoolBinOp { op, lhs, rhs } => write!(f, "({lhs} {op} {rhs})"),
            Predicate::Not(operand) => write!(f, "!{operand}"),
            Predicate::If {
                cond,
                then_br,
                else_br,
            } => write!(f, "(if {cond} then {then_br} else {else_br})"),
            Predicate::FieldProj { base, field } => write!(f, "{base}.{}", field.field),
            Predicate::SliceLen { slice } => write!(f, "{slice}.len()"),
            Predicate::SliceIndex { slice, index } => write!(f, "{slice}[{index}]"),
            Predicate::SliceStore {
                slice,
                index,
                value,
            } => write!(f, "store({slice}, {index}, {value})"),
            Predicate::Cast { value, to } => write!(f, "({value} as {})", to.type_name()),
            Predicate::TagEq { value, variant } => {
                write!(f, "({value} == {}.{})", variant.sum.name(), variant.variant)
            }
            Predicate::Forall {
                bound,
                lower,
                upper,
                body,
            } => write!(
                f,
                "(forall {} in {lower}..<{upper}: {body})",
                bound.name.as_str()
            ),
            Predicate::Exists {
                bound,
                lower,
                upper,
                body,
            } => write!(
                f,
                "(exists {} in {lower}..<{upper}: {body})",
                bound.name.as_str()
            ),
        }
    }
}
