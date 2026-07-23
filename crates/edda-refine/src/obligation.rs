//! Discharge obligations and their kinds.
//!
//! An [`Obligation`] pairs a goal predicate with the conjunction of in-scope
//! predicates the typechecker has gathered at the obligation's source site.
//! The [`ObligationKind`] tag tells the diagnostic layer what kind of failure
//! to render — `b != 0` failure reads differently from a `requires` failure
//! at a call site.
//!
//! Per `docs/types/refinement-decidability.md` §3, the in-scope predicates the
//! caller assembles include parameter `where` clauses, `requires`, prior
//! `ensures`, field invariants, and `if`/`match` narrowing. Building that
//! conjunction is the typechecker's job; refine consumes the finished
//! [`Obligation`].

use smol_str::SmolStr;

use edda_span::Span;

use crate::annotation::DischargeRoute;
use crate::predicate::Predicate;
use crate::sort::{FieldRef, IntSort, VariantRef};

//            uses this tag to select the canonical message template
//          narrowing-cast addition
/// Why this obligation arose. Determines the diagnostic header rendered when
/// discharge fails.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum ObligationKind {
    /// `a / b` — divisor must be non-zero. Per refinement-decidability.md §3.
    DivByZero,
    /// `a % b` — divisor must be non-zero. Per refinement-decidability.md §3.
    ModByZero,
    /// `xs[i]` — index must be less than slice length. Per
    /// refinement-decidability.md §3.
    SliceBound,
    /// `expr as <T>` where the target is narrower than the source. Per
    /// refinement-decidability.md §6 (narrowing-cast obligation).
    NarrowingCast {
        /// Source integer sort.
        from: IntSort,
        /// Target integer sort.
        to: IntSort,
    },
    /// A `requires` clause at the call site of `callee`. The index identifies
    /// which `requires` (functions may declare several).
    RequiresAtCall {
        /// Fully qualified name of the callee, as known to the typechecker.
        callee: SmolStr,
        /// Position of the failing clause within the callee's `requires` list.
        clause_index: u32,
    },
    /// An `ensures` clause at a `return` site of the current function.
    EnsuresAtReturn {
        /// Position of the failing clause within the current function's
        /// `ensures` list.
        clause_index: u32,
    },
    /// A field-level invariant on a record value being constructed or mutated.
    /// Per refinements.md *Refinements on record fields*.
    FieldInvariant {
        /// Field whose invariant failed.
        field: FieldRef,
    },
    /// A payload refinement on a sum-variant constructor. Per refinements.md
    /// *Sum-variant payload refinements*.
    VariantPayloadInvariant {
        /// Variant whose payload refinement failed.
        variant: VariantRef,
        /// Name of the offending payload field.
        payload_field: SmolStr,
    },
    /// A `decreases` termination obligation at an in-SCC recursive
    /// call site or a loop iteration boundary. Per
    /// `corpus/edda-codex/language/03-verification.md` §5, the obligation
    /// is `decreases_expr[call_site_args] < decreases_expr[caller_args]`
    /// with `decreases_expr[caller_args] >= 0` added to the context;
    /// for `loop decreases <m>`, the obligation is `m_after < m_before`
    /// with `m_before >= 0` in context. Discharged via LIA for `Int`
    /// measures. C5 emits these obligations; this variant exists so the
    /// diagnostic header can name the right failure mode.
    TerminationDecreases {
        /// Fully-qualified name of the in-SCC callee, or a synthetic
        /// `"<loop>"` for loop-iteration obligations.
        callee: SmolStr,
        /// Zero-based index of this obligation within the callee's /
        /// loop's obligation sequence — distinguishes the strict
        /// decrease obligation (index 0) from the well-foundedness
        /// obligation (index 1) and from siblings in a tuple-measure
        /// expansion (C6).
        call_index: u32,
    },
    /// A graded-effect bound obligation at a call site. Per
    /// `corpus/edda-codex/language/02-modes-effects-refinements.md` §5,
    /// the obligation is `caller_bound >= callee_bound` over the
    /// graded resource (`bytes` / `calls` / `ops`). Discharged via LIA.
    GradedBound {
        /// Graded kind name (`"alloc"` / `"io"` / `"time"`).
        kind: SmolStr,
        /// Fully-qualified name of the callee whose bound the caller
        /// must cover.
        callee: SmolStr,
    },
}

impl ObligationKind {
    /// Project this obligation kind to its routing
    /// [`DiagnosticClass`](edda_diag::DiagnosticClass).
    ///
    /// Per `02-modes-effects-refinements.md` §5.8, graded-bound
    /// failures route through the dedicated
    /// `effect_graded_bound_exceeded` class rather than the generic
    /// `refinement_unproven`.
    pub fn diagnostic_class(&self) -> edda_diag::DiagnosticClass {
        match self {
            ObligationKind::GradedBound { .. } => {
                edda_diag::DiagnosticClass::EffectGradedBoundExceeded
            }
            _ => edda_diag::DiagnosticClass::RefinementUnproven,
        }
    }

    /// Short human-readable header for diagnostics. The full message includes
    /// the predicate text and (when discharge returns `sat`) a counter-example
    /// tail; this header names only the operator or clause type.
    pub fn header(&self) -> String {
        match self {
            ObligationKind::DivByZero => "division by zero".to_string(),
            ObligationKind::ModByZero => "modulus by zero".to_string(),
            ObligationKind::SliceBound => "slice index out of bounds".to_string(),
            ObligationKind::NarrowingCast { from, to } => {
                format!("narrowing cast from {} to {}", from.type_name(), to.type_name())
            }
            ObligationKind::RequiresAtCall {
                callee,
                clause_index,
            } => format!("requires clause {clause_index} of `{callee}`"),
            ObligationKind::EnsuresAtReturn { clause_index } => {
                format!("ensures clause {clause_index}")
            }
            ObligationKind::FieldInvariant { field } => {
                format!("field invariant on `{}.{}`", field.record.name(), field.field)
            }
            ObligationKind::VariantPayloadInvariant {
                variant,
                payload_field,
            } => format!(
                "payload invariant on `{}.{}.{}`",
                variant.sum.name(),
                variant.variant,
                payload_field
            ),
            ObligationKind::TerminationDecreases {
                callee,
                call_index,
            } => format!("termination measure (`{callee}`, sub-obligation {call_index})"),
            ObligationKind::GradedBound { kind, callee } => {
                format!("graded {kind} bound exceeded by `{callee}`")
            }
        }
    }
}

//            the typechecker assembles it from where / requires / ensures / field
//            invariants / narrowing per refinement-decidability.md §3
/// A single discharge obligation.
///
/// The typechecker produces an [`Obligation`] for every required-decidable
/// operator use site and every `requires` / `ensures` clause crossing a
/// function boundary. The [`Solver`](crate::Solver) consumes the
/// [`Obligation`] and returns a
/// [`DischargeOutcome`](crate::DischargeOutcome).
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct Obligation {
    /// The predicate that must hold (the goal). Discharge succeeds iff Z3
    /// reports `unsat` on `context ∧ ¬goal`.
    pub(crate) goal: Predicate,
    /// Conjunction of predicates currently in scope. Assembled by the
    /// typechecker per refinement-decidability.md §3.
    pub(crate) context: Vec<Predicate>,
    /// Source position of the operator or clause that produced the
    /// obligation.
    pub(crate) site: Span,
    /// Kind tag — drives diagnostic header rendering.
    pub(crate) kind: ObligationKind,
    /// Pretty-rendered goal, kept alongside `goal` so diagnostics can echo the
    /// user's source-level text without re-pretty-printing the IR. May be
    /// empty if the caller has no better representation than
    /// [`std::fmt::Display`] on `goal`.
    pub(crate) predicate_text: String,
    /// Discharge method per refinement-decidability.md §9. Defaults to
    /// [`DischargeRoute::Smt`]; the typechecker overrides via
    /// [`Obligation::with_route`] when an `@unverified` / `@trust` annotation
    /// is in scope.
    pub(crate) route: DischargeRoute,
}

impl Obligation {
    /// Construct an obligation routed through the SMT solver (the default
    /// route). The `predicate_text` defaults to the
    /// [`Display`](std::fmt::Display) form of `goal` if the caller passes an
    /// empty string.
    pub fn new(
        goal: Predicate,
        context: Vec<Predicate>,
        site: Span,
        kind: ObligationKind,
        predicate_text: impl Into<String>,
    ) -> Obligation {
        let mut text: String = predicate_text.into();
        if text.is_empty() {
            text = format!("{goal}");
        }
        Obligation {
            goal,
            context,
            site,
            kind,
            predicate_text: text,
            route: DischargeRoute::Smt,
        }
    }

    /// Builder-style: route this obligation through `@unverified` or
    /// `@trust` instead of the SMT solver. The discharge layer emits the
    /// corresponding certificate and returns
    /// [`DischargeOutcome::Unsat`](crate::DischargeOutcome::Unsat) without
    /// calling Z3.
    pub fn with_route(mut self, route: DischargeRoute) -> Obligation {
        self.route = route;
        self
    }

    /// The predicate that must hold (the goal).
    pub fn goal(&self) -> &Predicate {
        &self.goal
    }

    /// In-scope predicate conjunction assembled by the typechecker.
    pub fn context(&self) -> &[Predicate] {
        &self.context
    }

    /// Source position of the operator or clause that produced the obligation.
    pub fn site(&self) -> &Span {
        &self.site
    }

    /// Kind tag — drives diagnostic header rendering.
    pub fn kind(&self) -> &ObligationKind {
        &self.kind
    }

    /// Pretty-rendered goal text for diagnostics.
    pub fn predicate_text(&self) -> &str {
        &self.predicate_text
    }

    /// Discharge method per refinement-decidability.md §9.
    pub fn route(&self) -> &DischargeRoute {
        &self.route
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::predicate::{CmpOp, IntLit, Variable};
    use crate::sort::{IntWidth, Sort};

    #[test]
    fn predicate_text_defaults_to_display_when_empty() {
        let i32_sort = IntSort::sized(IntWidth::W32, true);
        let goal = Predicate::cmp(
            CmpOp::Ne,
            Predicate::Var(Variable::new("den", Sort::Int(i32_sort))),
            Predicate::IntLit(IntLit::signed(0, i32_sort)),
        );
        let o = Obligation::new(
            goal,
            Vec::new(),
            Span::DUMMY,
            ObligationKind::DivByZero,
            "",
        );
        assert_eq!(o.predicate_text(), "(den != 0)");
    }

    #[test]
    fn predicate_text_passes_through_when_provided() {
        let goal = Predicate::BoolLit(false);
        let o = Obligation::new(
            goal,
            Vec::new(),
            Span::DUMMY,
            ObligationKind::DivByZero,
            "den != 0",
        );
        assert_eq!(o.predicate_text(), "den != 0");
    }

    #[test]
    fn obligation_kind_headers_are_distinct() {
        let headers = [
            ObligationKind::DivByZero.header(),
            ObligationKind::ModByZero.header(),
            ObligationKind::SliceBound.header(),
            ObligationKind::NarrowingCast {
                from: IntSort::sized(IntWidth::W32, false),
                to: IntSort::sized(IntWidth::W8, false),
            }
            .header(),
            ObligationKind::RequiresAtCall {
                callee: "std.option.unwrap".into(),
                clause_index: 0,
            }
            .header(),
            ObligationKind::TerminationDecreases {
                callee: "factorial".into(),
                call_index: 0,
            }
            .header(),
        ];
        // No two headers collide — diagnostics need disambiguatable surfaces.
        for (i, a) in headers.iter().enumerate() {
            for b in &headers[i + 1..] {
                assert_ne!(a, b, "collision: {a:?}");
            }
        }
    }
}
