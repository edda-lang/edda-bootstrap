//! Smart constructors for [`Predicate`] that enforce the predicate-fragment invariants.
//!
//! The LIA literal-constant rule (`refinement-decidability.md` §4) is the
//! load-bearing invariant here: [`Predicate::mul_lit`] / [`Predicate::div_lit`]
//! are the only legitimate way to build a multiplication or division and
//! structurally enforce that one operand is an [`IntLit`]. Direct enum-variant
//! construction is reachable but unsupported — callers go through these
//! constructors so the IR cannot represent an undecidable arithmetic term.

use super::{ArithOp, BoolBinOp, CmpOp, IntLit, Predicate, Variable};
use crate::sort::{FieldRef, IntSort, VariantRef};

impl Predicate {
    /// Multiplication of `expr` by a literal constant `c`. Per
    /// refinement-decidability.md §4, multiplication where neither operand is
    /// a compile-time-constant literal is not required-decidable; this
    /// constructor — the only way to build a multiplication in the IR —
    /// enforces the rule structurally.
    pub fn mul_lit(c: IntLit, expr: Predicate) -> Predicate {
        Predicate::MulLit {
            c,
            expr: Box::new(expr),
        }
    }

    /// Division of `expr` by a literal constant `c`. Same LIA rule as
    /// [`Predicate::mul_lit`].
    pub fn div_lit(expr: Predicate, c: IntLit) -> Predicate {
        Predicate::DivLit {
            expr: Box::new(expr),
            c,
        }
    }

    /// Euclidean remainder of `expr` by a literal constant `c`. Same LIA
    /// rule as [`Predicate::div_lit`].
    pub fn mod_lit(expr: Predicate, c: IntLit) -> Predicate {
        Predicate::ModLit {
            expr: Box::new(expr),
            c,
        }
    }

    /// `lhs + rhs`.
    pub fn add(lhs: Predicate, rhs: Predicate) -> Predicate {
        Predicate::Arith {
            op: ArithOp::Add,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        }
    }

    /// `lhs - rhs`.
    pub fn sub(lhs: Predicate, rhs: Predicate) -> Predicate {
        Predicate::Arith {
            op: ArithOp::Sub,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        }
    }

    /// Comparison.
    pub fn cmp(op: CmpOp, lhs: Predicate, rhs: Predicate) -> Predicate {
        Predicate::Cmp {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        }
    }

    /// `lhs && rhs`.
    pub fn and(lhs: Predicate, rhs: Predicate) -> Predicate {
        Predicate::BoolBinOp {
            op: BoolBinOp::And,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        }
    }

    /// `lhs || rhs`.
    pub fn or(lhs: Predicate, rhs: Predicate) -> Predicate {
        Predicate::BoolBinOp {
            op: BoolBinOp::Or,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        }
    }

    /// `!operand`.
    pub fn not(operand: Predicate) -> Predicate {
        Predicate::Not(Box::new(operand))
    }

    /// `-operand`.
    pub fn neg(operand: Predicate) -> Predicate {
        Predicate::Neg(Box::new(operand))
    }

    /// `if cond { then_br } else { else_br }`.
    pub fn if_then_else(cond: Predicate, then_br: Predicate, else_br: Predicate) -> Predicate {
        Predicate::If {
            cond: Box::new(cond),
            then_br: Box::new(then_br),
            else_br: Box::new(else_br),
        }
    }

    /// `base.field`.
    pub fn field_proj(base: Predicate, field: FieldRef) -> Predicate {
        Predicate::FieldProj {
            base: Box::new(base),
            field,
        }
    }

    /// `slice.len()`.
    pub fn slice_len(slice: Predicate) -> Predicate {
        Predicate::SliceLen {
            slice: Box::new(slice),
        }
    }

    /// `slice[index]`.
    pub fn slice_index(slice: Predicate, index: Predicate) -> Predicate {
        Predicate::SliceIndex {
            slice: Box::new(slice),
            index: Box::new(index),
        }
    }

    /// `store(slice, index, value)`.
    pub fn slice_store(slice: Predicate, index: Predicate, value: Predicate) -> Predicate {
        Predicate::SliceStore {
            slice: Box::new(slice),
            index: Box::new(index),
            value: Box::new(value),
        }
    }

    /// `value as <to>`.
    pub fn cast(value: Predicate, to: IntSort) -> Predicate {
        Predicate::Cast {
            value: Box::new(value),
            to,
        }
    }

    /// Tag equality on a payload-free variant.
    pub fn tag_eq(value: Predicate, variant: VariantRef) -> Predicate {
        Predicate::TagEq {
            value: Box::new(value),
            variant,
        }
    }

    /// `forall <bound> in [<lower>, <upper>): <body>` — bounded universal
    /// quantifier. `bound.sort` must be `Sort::Int`; `body` must be
    /// `Bool`-sorted. The Z3 translator emits this as a Z3 `forall_const`
    /// under solver logic `AUFLIA`.
    pub fn forall(bound: Variable, lower: Predicate, upper: Predicate, body: Predicate) -> Predicate {
        Predicate::Forall {
            bound,
            lower: Box::new(lower),
            upper: Box::new(upper),
            body: Box::new(body),
        }
    }

    /// `exists <bound> in [<lower>, <upper>): <body>` — bounded existential
    /// quantifier. Mirror of [`Predicate::forall`].
    pub fn exists(bound: Variable, lower: Predicate, upper: Predicate, body: Predicate) -> Predicate {
        Predicate::Exists {
            bound,
            lower: Box::new(lower),
            upper: Box::new(upper),
            body: Box::new(body),
        }
    }
}
