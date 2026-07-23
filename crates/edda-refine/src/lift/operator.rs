//! Binary and unary operator lifts: arithmetic, comparison, boolean, negation.

use edda_span::Span;
use edda_syntax::ast::{BinOp, Expr, UnOp};

use crate::error::LiftError;
use crate::predicate::{CmpOp, Predicate};

use super::env::PredicateEnv;
use super::literal::match_int_lit;
use super::lift_predicate;

pub(super) fn lift_binary(
    op: BinOp,
    lhs: &Expr,
    rhs: &Expr,
    span: Span,
    env: &dyn PredicateEnv,
) -> Result<Predicate, LiftError> {
    match op {
        BinOp::Add => Ok(Predicate::add(
            lift_predicate(lhs, env)?,
            lift_predicate(rhs, env)?,
        )),
        BinOp::Sub => Ok(Predicate::sub(
            lift_predicate(lhs, env)?,
            lift_predicate(rhs, env)?,
        )),
        BinOp::Mul => lift_mul(lhs, rhs, span, env),
        BinOp::Div => lift_div(lhs, rhs, span, env),
        BinOp::Mod => lift_mod(lhs, rhs, span, env),
        BinOp::Eq => Ok(Predicate::cmp(
            CmpOp::Eq,
            lift_predicate(lhs, env)?,
            lift_predicate(rhs, env)?,
        )),
        BinOp::Ne => Ok(Predicate::cmp(
            CmpOp::Ne,
            lift_predicate(lhs, env)?,
            lift_predicate(rhs, env)?,
        )),
        BinOp::Lt => Ok(Predicate::cmp(
            CmpOp::Lt,
            lift_predicate(lhs, env)?,
            lift_predicate(rhs, env)?,
        )),
        BinOp::Le => Ok(Predicate::cmp(
            CmpOp::Le,
            lift_predicate(lhs, env)?,
            lift_predicate(rhs, env)?,
        )),
        BinOp::Gt => Ok(Predicate::cmp(
            CmpOp::Gt,
            lift_predicate(lhs, env)?,
            lift_predicate(rhs, env)?,
        )),
        BinOp::Ge => Ok(Predicate::cmp(
            CmpOp::Ge,
            lift_predicate(lhs, env)?,
            lift_predicate(rhs, env)?,
        )),
        BinOp::And => Ok(Predicate::and(
            lift_predicate(lhs, env)?,
            lift_predicate(rhs, env)?,
        )),
        BinOp::Or => Ok(Predicate::or(
            lift_predicate(lhs, env)?,
            lift_predicate(rhs, env)?,
        )),
        BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
            Err(LiftError::NotAdmittedInPredicate {
                form: "bitwise / shift operators",
                span,
            })
        }
        BinOp::WrapAdd | BinOp::WrapSub | BinOp::WrapMul => {
            Err(LiftError::NotAdmittedInPredicate {
                form: "wrapping-arithmetic operators (`+%` / `-%` / `*%`)",
                span,
            })
        }
        BinOp::CheckAdd | BinOp::CheckSub | BinOp::CheckMul | BinOp::CheckMod => {
            Err(LiftError::NotAdmittedInPredicate {
                form: "checked-arithmetic operators (`+?` / `-?` / `*?` / `%?`)",
                span,
            })
        }
        BinOp::SatAdd | BinOp::SatSub | BinOp::SatMul => {
            Err(LiftError::NotAdmittedInPredicate {
                form: "saturating-arithmetic operators (`+|` / `-|` / `*|`)",
                span,
            })
        }
    }
}

// LIA literal-constant rule: at least one operand must be an Int literal.
pub(super) fn lift_mul(
    lhs: &Expr,
    rhs: &Expr,
    span: Span,
    env: &dyn PredicateEnv,
) -> Result<Predicate, LiftError> {
    if let Some(lit) = match_int_lit(lhs, env)? {
        return Ok(Predicate::mul_lit(lit, lift_predicate(rhs, env)?));
    }
    if let Some(lit) = match_int_lit(rhs, env)? {
        return Ok(Predicate::mul_lit(lit, lift_predicate(lhs, env)?));
    }
    Err(LiftError::Unsupported {
        what: "multiplication of two non-literal operands is non-linear and \
               outside the required-decidable fragment per \
               `refinement-decidability.md` §4"
            .to_string(),
        span,
    })
}

pub(super) fn lift_div(
    lhs: &Expr,
    rhs: &Expr,
    span: Span,
    env: &dyn PredicateEnv,
) -> Result<Predicate, LiftError> {
    if let Some(lit) = match_int_lit(rhs, env)? {
        return Ok(Predicate::div_lit(lift_predicate(lhs, env)?, lit));
    }
    Err(LiftError::Unsupported {
        what: "division by a non-literal divisor is non-linear and outside \
               the required-decidable fragment per \
               `refinement-decidability.md` §4"
            .to_string(),
        span,
    })
}

pub(super) fn lift_mod(
    lhs: &Expr,
    rhs: &Expr,
    span: Span,
    env: &dyn PredicateEnv,
) -> Result<Predicate, LiftError> {
    if let Some(lit) = match_int_lit(rhs, env)? {
        return Ok(Predicate::mod_lit(lift_predicate(lhs, env)?, lit));
    }
    Err(LiftError::Unsupported {
        what: "`%` by a non-literal divisor is non-linear and outside the \
               required-decidable fragment per `refinement-decidability.md` \
               §4"
            .to_string(),
        span,
    })
}

pub(super) fn lift_unary(
    op: UnOp,
    operand: &Expr,
    span: Span,
    env: &dyn PredicateEnv,
) -> Result<Predicate, LiftError> {
    match op {
        UnOp::Neg => Ok(Predicate::neg(lift_predicate(operand, env)?)),
        UnOp::Not => Ok(Predicate::not(lift_predicate(operand, env)?)),
        UnOp::BitNot => Err(LiftError::NotAdmittedInPredicate {
            form: "bitwise complement `~`",
            span,
        }),
    }
}
