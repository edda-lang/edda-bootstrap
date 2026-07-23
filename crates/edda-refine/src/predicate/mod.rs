//! Typed predicate IR over the required-decidable fragment.
//!
//! The IR mirrors `docs/types/refinement-decidability.md` §2's EUF + LIA +
//! boolean + extensional-arrays fragment. Every term carries enough sort
//! information to drive the Z3 translation pass without re-querying
//! the typechecker.
//!
//! # Decidability enforced at the IR level
//!
//! The spec's "multiplication and division by literal constants only" rule
//! (§4) is encoded in the IR: dedicated [`Predicate::MulLit`] and
//! [`Predicate::DivLit`] variants take an [`IntLit`] on one side and a
//! [`Predicate`] on the other. Constructing `x * y` for two non-literal
//! [`Predicate`]s is unrepresentable; the typechecker that lowers Edda's
//! surface refinement expression into this IR rejects non-LIA multiplication
//! at lowering time and produces a `typecheck.refinement_unproven` diagnostic.
//!
//! # Sort inference
//!
//! [`Predicate::sort`] walks the term and reports the top-level sort under the
//! assumption that the term is well-sorted. Construction does not validate
//! sort consistency between operands — that work belongs to the Z3 translator,
//! which would fail to translate the offending term anyway.

mod build;
mod display;

use smol_str::SmolStr;

use crate::sort::{FieldRef, IntSort, Sort, VariantRef};

/// Integer-literal payload. Tagged so u128 max values do not lose precision
/// when the paired sort is unsigned.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum IntLitValue {
    /// Signed value. Used when the paired [`IntSort`] is signed.
    Signed(i128),
    /// Unsigned value. Used when the paired [`IntSort`] is unsigned.
    Unsigned(u128),
}

/// Integer literal. Carries its sort so the predicate IR is fully typed without
/// an ambient environment.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct IntLit {
    /// Literal value.
    pub(crate) value: IntLitValue,
    /// Sort the literal occupies.
    pub(crate) sort: IntSort,
}

impl IntLit {
    /// Construct a signed integer literal. The caller is responsible for the
    /// value fitting in the sort's signed range; out-of-range values produce
    /// a translation-time error.
    pub const fn signed(value: i128, sort: IntSort) -> IntLit {
        IntLit {
            value: IntLitValue::Signed(value),
            sort,
        }
    }

    /// Construct an unsigned integer literal. The caller is responsible for
    /// the value fitting in the sort's unsigned range.
    pub const fn unsigned(value: u128, sort: IntSort) -> IntLit {
        IntLit {
            value: IntLitValue::Unsigned(value),
            sort,
        }
    }

    /// The literal's tagged value.
    pub fn value(&self) -> IntLitValue {
        self.value
    }

    /// The integer sort this literal occupies.
    pub fn sort(&self) -> IntSort {
        self.sort
    }
}

/// Free variable in a predicate. Refers to a parameter, field, or local
/// binding established by the discharge-context construction.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct Variable {
    /// Source-level name. The typechecker's binding-id is collapsed to a
    /// stable name string before reaching refine; in the daemon-internal
    /// representation this is the unique name (post-renaming) so two variables
    /// with the same source spelling but different scopes do not collide.
    pub(crate) name: SmolStr,
    /// Sort of the binding.
    pub(crate) sort: Sort,
}

impl Variable {
    /// Convenience constructor.
    pub fn new(name: impl Into<SmolStr>, sort: Sort) -> Variable {
        Variable {
            name: name.into(),
            sort,
        }
    }

    /// Source-level name of this variable.
    pub fn name(&self) -> &SmolStr {
        &self.name
    }

    /// Sort of the binding.
    pub fn sort(&self) -> &Sort {
        &self.sort
    }
}

/// Linear-integer arithmetic operator.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum ArithOp {
    /// `a + b`.
    Add,
    /// `a - b`.
    Sub,
}

/// Comparison operator. Produces a [`Sort::Bool`] from operands of an
/// equality-bearing sort (or integer sort for `<`, `<=`, `>`, `>=`).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum CmpOp {
    /// `a == b`.
    Eq,
    /// `a != b`.
    Ne,
    /// `a < b`.
    Lt,
    /// `a <= b`.
    Le,
    /// `a > b`.
    Gt,
    /// `a >= b`.
    Ge,
}

/// Two-place boolean connective.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum BoolBinOp {
    /// `a && b`.
    And,
    /// `a || b`.
    Or,
}

//            a separate environment table; sort consistency between operands is the caller's
//            responsibility and is re-checked at Z3 translation time
/// Predicate term. The required-decidable fragment from
/// `docs/types/refinement-decidability.md` §2 plus the LIA literal-constant
/// rule from §4.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum Predicate {
    /// Free variable.
    Var(Variable),
    /// Integer literal.
    IntLit(IntLit),
    /// Boolean literal.
    BoolLit(bool),

    /// `a + b` or `a - b`.
    Arith {
        /// Operator.
        op: ArithOp,
        /// Left operand.
        lhs: Box<Predicate>,
        /// Right operand.
        rhs: Box<Predicate>,
    },
    /// Unary minus on an integer.
    Neg(Box<Predicate>),
    /// `c * expr` where `c` is a literal constant. The asymmetry encodes the
    /// LIA decidability rule. Construct via [`Predicate::mul_lit`] — direct
    /// variant construction bypasses the rule-enforcement contract.
    MulLit {
        /// Literal multiplier.
        c: IntLit,
        /// Expression being scaled.
        expr: Box<Predicate>,
    },
    /// `expr / c` where `c` is a literal constant. Same LIA rule as
    /// [`Predicate::MulLit`]. Construct via [`Predicate::div_lit`] — direct
    /// variant construction bypasses the rule-enforcement contract.
    DivLit {
        /// Expression being divided.
        expr: Box<Predicate>,
        /// Literal divisor. Zero is admitted at the IR level — the `b != 0`
        /// obligation is raised separately by the typechecker.
        c: IntLit,
    },
    /// `expr % c` where `c` is a literal constant — Euclidean remainder,
    /// translated to SMT-LIB `(mod expr c)` so the theory links it to
    /// [`Predicate::DivLit`] via `x == c*(x/c) + (x%c)`. Same LIA rule as
    /// [`Predicate::DivLit`]. Construct via [`Predicate::mod_lit`] — direct
    /// variant construction bypasses the rule-enforcement contract.
    ModLit {
        /// Expression being reduced.
        expr: Box<Predicate>,
        /// Literal divisor. Zero is admitted at the IR level — the `b != 0`
        /// obligation is raised separately by the typechecker.
        c: IntLit,
    },

    /// Comparison between two operands.
    Cmp {
        /// Operator.
        op: CmpOp,
        /// Left operand.
        lhs: Box<Predicate>,
        /// Right operand.
        rhs: Box<Predicate>,
    },
    /// Two-place boolean connective.
    BoolBinOp {
        /// Operator.
        op: BoolBinOp,
        /// Left operand.
        lhs: Box<Predicate>,
        /// Right operand.
        rhs: Box<Predicate>,
    },
    /// Logical negation.
    Not(Box<Predicate>),

    /// `if cond { then_br } else { else_br }`. Both branches must share a sort.
    If {
        /// Condition (must be `Bool`).
        cond: Box<Predicate>,
        /// Then branch.
        then_br: Box<Predicate>,
        /// Else branch.
        else_br: Box<Predicate>,
    },

    /// Record field projection (EUF). Models `base.field` as an
    /// uninterpreted function per record-field per
    /// `refinement-decidability.md` §2.
    FieldProj {
        /// Base record value.
        base: Box<Predicate>,
        /// Field reference; carries the projected sort.
        field: FieldRef,
    },

    /// Slice length `xs.len()`. Modeled as a per-element-sort uninterpreted
    /// function `len: [T] -> usize` in Z3.
    SliceLen {
        /// Slice value.
        slice: Box<Predicate>,
    },
    /// Slice index `xs[i]`. Modeled as `select(xs, i)` in the array theory.
    SliceIndex {
        /// Slice value.
        slice: Box<Predicate>,
        /// Index expression.
        index: Box<Predicate>,
    },
    /// Slice update `xs[i] = v` — appears in `ensures` clauses only. Modeled
    /// as `store(xs, i, v)` in the array theory.
    SliceStore {
        /// Slice value before the update.
        slice: Box<Predicate>,
        /// Index expression.
        index: Box<Predicate>,
        /// Value written.
        value: Box<Predicate>,
    },

    /// Non-narrowing integer cast `expr as <T>`. Narrowing casts carry their
    /// own obligation and are not admitted here.
    Cast {
        /// Value being cast.
        value: Box<Predicate>,
        /// Target sort.
        to: IntSort,
    },

    /// Tag equality for a payload-free sum variant — the spec admits
    /// `state == ConnectionState.closed` only when `closed` carries no
    /// payload. Payload-bearing variants are rejected at the Z3 translation
    /// step.
    TagEq {
        /// Sum value.
        value: Box<Predicate>,
        /// Variant being tested for.
        variant: VariantRef,
    },

    /// `forall <bound> in [<lower>, <upper>): <body>` — bounded universal
    /// quantifier. The range is half-open: `lower <= bound < upper`.
    /// Slice-element iteration (`forall x in xs: P(x)`) lifts to this form
    /// with a fresh integer index variable and `body` substituted to
    /// `P(SliceIndex(xs, <idx>))`. Per V1.0 refinement-fragment widening
    /// (`refinement-decidability.md` §11): bumps the solver logic from
    /// `QF_AUFLIA` to `AUFLIA`.
    Forall {
        /// Integer-sorted bound variable, visible only inside `body`.
        bound: Variable,
        /// Lower bound (inclusive).
        lower: Box<Predicate>,
        /// Upper bound (exclusive).
        upper: Box<Predicate>,
        /// Body predicate — `Bool`-sorted; may reference `bound`.
        body: Box<Predicate>,
    },
    /// `exists <bound> in [<lower>, <upper>): <body>` — bounded existential
    /// quantifier. Mirror of [`Predicate::Forall`] with existential
    /// semantics.
    Exists {
        /// Integer-sorted bound variable, visible only inside `body`.
        bound: Variable,
        /// Lower bound (inclusive).
        lower: Box<Predicate>,
        /// Upper bound (exclusive).
        upper: Box<Predicate>,
        /// Body predicate — `Bool`-sorted; may reference `bound`.
        body: Box<Predicate>,
    },
}

impl Predicate {
    //            mismatched operand sorts produce a translation-time error
    /// Compute the sort of this term. Walks only as far as necessary to
    /// determine the top-level sort.
    pub fn sort(&self) -> Sort {
        match self {
            Predicate::Var(v) => v.sort.clone(),
            Predicate::IntLit(lit) => Sort::Int(lit.sort),
            Predicate::BoolLit(_) => Sort::Bool,
            Predicate::Arith { lhs, .. } => lhs.sort(),
            Predicate::Neg(operand) => operand.sort(),
            Predicate::MulLit { expr, .. } => expr.sort(),
            Predicate::DivLit { expr, .. } => expr.sort(),
            Predicate::ModLit { expr, .. } => expr.sort(),
            Predicate::Cmp { .. } => Sort::Bool,
            Predicate::BoolBinOp { .. } => Sort::Bool,
            Predicate::Not(_) => Sort::Bool,
            Predicate::If { then_br, .. } => then_br.sort(),
            Predicate::FieldProj { field, .. } => field.sort.clone(),
            Predicate::SliceLen { .. } => Sort::usize(),
            Predicate::SliceIndex { slice, .. } => match slice.sort() {
                Sort::Slice(element) => *element,
                // Caller bug: the slice operand was not a Slice sort. The Z3
                // translator will reject; we surface the slice sort itself so
                // higher layers can render a meaningful error.
                other => other,
            },
            Predicate::SliceStore { slice, .. } => slice.sort(),
            Predicate::Cast { to, .. } => Sort::Int(*to),
            Predicate::TagEq { .. } => Sort::Bool,
            Predicate::Forall { .. } | Predicate::Exists { .. } => Sort::Bool,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sort::{IntWidth, RecordRef, SumRef};

    fn i32_lit(v: i32) -> Predicate {
        Predicate::IntLit(IntLit::signed(
            v as i128,
            IntSort::sized(IntWidth::W32, true),
        ))
    }

    fn i32_var(name: &str) -> Predicate {
        Predicate::Var(Variable::new(
            name,
            Sort::Int(IntSort::sized(IntWidth::W32, true)),
        ))
    }

    #[test]
    fn mul_lit_rejects_non_literal_multiplication_at_the_type_level() {
        // `5 * x` is representable.
        let p = Predicate::mul_lit(
            IntLit::signed(5, IntSort::sized(IntWidth::W32, true)),
            i32_var("x"),
        );
        assert!(matches!(p, Predicate::MulLit { .. }));
        // `x * y` requires constructing `MulLit` with two non-literal Predicates —
        // the type signature of `Predicate::mul_lit` (IntLit, Predicate) rejects
        // this at compile time, so the test below is a compile-time check
        // disguised as documentation: removing the `IntLit` argument type would
        // break this comment.
    }

    #[test]
    fn arith_sort_inferred_from_lhs() {
        let p = Predicate::add(i32_var("x"), i32_lit(7));
        match p.sort() {
            Sort::Int(IntSort {
                width: IntWidth::W32,
                signed: true,
            }) => {}
            other => panic!("unexpected sort: {other:?}"),
        }
    }

    #[test]
    fn comparison_always_yields_bool() {
        let p = Predicate::cmp(CmpOp::Lt, i32_var("i"), i32_lit(10));
        assert_eq!(p.sort(), Sort::Bool);
    }

    #[test]
    fn slice_len_yields_usize_and_index_yields_element_sort() {
        let xs = Predicate::Var(Variable::new(
            "xs",
            Sort::slice(Sort::Int(IntSort::sized(IntWidth::W64, true))),
        ));
        let len = Predicate::slice_len(xs.clone());
        assert_eq!(len.sort(), Sort::usize());

        let i = Predicate::Var(Variable::new("i", Sort::usize()));
        let elem = Predicate::slice_index(xs, i);
        assert_eq!(elem.sort(), Sort::Int(IntSort::sized(IntWidth::W64, true)));
    }

    #[test]
    fn field_proj_carries_its_sort() {
        let buf = Predicate::Var(Variable::new(
            "buf",
            Sort::Record(RecordRef::new("StringBuf")),
        ));
        let field = FieldRef::new(RecordRef::new("StringBuf"), "len", Sort::usize());
        let proj = Predicate::field_proj(buf, field);
        assert_eq!(proj.sort(), Sort::usize());
    }

    #[test]
    fn cast_sort_is_target_sort() {
        let usize_value = Predicate::Var(Variable::new("n", Sort::usize()));
        let casted = Predicate::cast(usize_value, IntSort::sized(IntWidth::W64, true));
        assert_eq!(casted.sort(), Sort::Int(IntSort::sized(IntWidth::W64, true)));
    }

    #[test]
    fn tag_eq_yields_bool() {
        let conn = Predicate::Var(Variable::new(
            "c",
            Sort::Sum(SumRef::new("Connection")),
        ));
        let p = Predicate::tag_eq(conn, VariantRef::new(SumRef::new("Connection"), "closed"));
        assert_eq!(p.sort(), Sort::Bool);
    }

    #[test]
    fn display_roundtrips_through_format() {
        // `(x < xs.len())` — the canonical slice-bound predicate.
        let i = Predicate::Var(Variable::new("i", Sort::usize()));
        let xs = Predicate::Var(Variable::new(
            "xs",
            Sort::slice(Sort::Int(IntSort::sized(IntWidth::W64, true))),
        ));
        let p = Predicate::cmp(CmpOp::Lt, i, Predicate::slice_len(xs));
        assert_eq!(format!("{p}"), "(i < xs.len())");
    }
}
