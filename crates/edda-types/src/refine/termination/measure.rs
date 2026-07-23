//! Measure-predicate construction for §5 termination discharge.
//!
//! Lifts a `decreases` clause into its per-component predicate list,
//! builds the well-foundedness (`m >= 0`) and lex-product strict-decrease
//! (`m[args] < m[params]`) obligations, and emits the diagnostic for a
//! measure that falls outside the required-decidable predicate fragment.

use smol_str::SmolStr;

use edda_diag::Diagnostics;
use edda_refine::{
    CmpOp, DischargeFailure, IntSort, IntWidth, ObligationKind, Predicate, RefineError, Sort,
    lift_predicate,
};
use edda_syntax::ast::{Expr, ExprKind};

use super::super::sort::FnRefineEnv;

/// Lift the decreases-clause expression into its per-component
/// predicate list. A tuple expression `(a, b, ...)` decomposes into
/// `[lift(a), lift(b), ...]`; anything else lifts as a singleton.
/// Returns the first component's [`LiftError`] when any component falls
/// outside the predicate fragment, so the caller can diagnose the exact
/// sub-expression rather than discarding the measure.
pub(super) fn lift_measure_components(
    measure: &Expr,
    env: &FnRefineEnv<'_>,
) -> Result<Vec<Predicate>, edda_refine::LiftError> {
    if let ExprKind::Tuple(elems) = &measure.kind {
        let mut out = Vec::with_capacity(elems.len());
        for e in elems {
            out.push(lift_predicate(e, env)?);
        }
        Ok(out)
    } else {
        Ok(vec![lift_predicate(measure, env)?])
    }
}

/// Emit a `RefinementUnproven` diagnostic for a `decreases` measure
/// expression that falls outside the required-decidable predicate
/// fragment. Routes through the same [`RefineError::discharge`] surface
/// the box_depth and arity-mismatch paths use, attributing the failure
/// to the offending sub-expression's span.
pub(super) fn diagnose_unliftable_measure(
    fn_name: &SmolStr,
    err: &edda_refine::LiftError,
    diags: &mut Diagnostics,
) {
    diags.push(
        RefineError::discharge(
            err.span(),
            ObligationKind::TerminationDecreases {
                callee: fn_name.clone(),
                call_index: 1,
            },
            format!(
                "`decreases` measure is outside the required-decidable fragment \
                 ({err}); restate it as an integer (LIA) measure or \
                 `box_depth(<param>)`, or declare `with {{ divergence }}` to opt out \
                 of the termination guarantee",
            ),
            DischargeFailure::Unknown {
                reason: Some(format!("decreases measure could not be lifted: {err}")),
            },
        )
        .to_diagnostic(),
    );
}

/// Filter the measure components to those whose well-foundedness
/// (`component >= 0`) is not structurally true.
///
/// Per refinement-decidability.md §5 the well-foundedness obligation is
/// `m >= 0`. For an unsigned integer sort the constraint is structurally
/// satisfied — the SMT solver cannot bridge type-level non-negativity
/// into its context without it being asserted explicitly (bug C14).
/// Rather than asserting it as an additional context predicate (which
/// would change the obligation hash and invalidate cached certificates
/// for every consumer that touches an unsigned measure), we skip the
/// sub-obligation outright. The strict-decrease check still discharges
/// against the SMT solver and benefits from u-type bounds inferred by
/// Z3's native bitvector/integer reasoning when the components are
/// asserted in the goal itself.
pub(super) fn signed_measure_components(components: &[Predicate]) -> Vec<Predicate> {
    components
        .iter()
        .filter(|c| !is_structurally_nonneg(c))
        .cloned()
        .collect()
}

/// `true` when this predicate's top-level sort is an unsigned integer.
/// The well-foundedness obligation `predicate >= 0` is structurally
/// satisfied at unsigned sorts.
fn is_structurally_nonneg(p: &Predicate) -> bool {
    matches!(p.sort(), Sort::Int(IntSort { signed: false, .. }))
}

/// Conjunction `a >= 0 AND b >= 0 AND ...` for a per-component
/// well-foundedness obligation. The conjunction handles tuple
/// measures uniformly with the scalar case (singleton list).
pub(super) fn build_well_foundedness_all(components: &[Predicate]) -> Predicate {
    let mut iter = components.iter();
    let first = iter.next().expect("non-empty by caller invariant");
    let mut out = build_well_foundedness(first);
    for c in iter {
        out = Predicate::and(out, build_well_foundedness(c));
    }
    out
}

/// Lex-product strict-less-than across paired component lists.
///
/// For arity 1: `a0 < c0`.
/// For arity 2: `(a0 < c0) OR ((a0 == c0) AND (a1 < c1))`.
/// For arity N: the same shape, nested. The predicate is built
/// right-associatively so the bottom case stays minimal.
pub(super) fn lex_product_lt(args: &[Predicate], caller: &[Predicate]) -> Predicate {
    debug_assert_eq!(args.len(), caller.len());
    debug_assert!(!args.is_empty());
    let head_lt = Predicate::cmp(CmpOp::Lt, args[0].clone(), caller[0].clone());
    if args.len() == 1 {
        return head_lt;
    }
    let head_eq = Predicate::cmp(CmpOp::Eq, args[0].clone(), caller[0].clone());
    let tail_lt = lex_product_lt(&args[1..], &caller[1..]);
    Predicate::or(head_lt, Predicate::and(head_eq, tail_lt))
}

/// Pack the per-call obligation index into the locked
/// `(callee, call_index)` shape used by [`ObligationKind::TerminationDecreases`].
/// Indices 0/1 are reserved for the well-foundedness and the per-tuple
/// component sub-obligations C6 will mint; recursive-call obligations
/// start at index 2 so the namespaces stay disjoint.
pub(super) fn call_index_to_external(zero_based: u32) -> u32 {
    zero_based.saturating_add(2)
}

/// `m >= 0` as the standard well-foundedness obligation on an `Int`-
/// sorted measure. For tuple measures (C6) this becomes a conjunction
/// over each component.
fn build_well_foundedness(measure: &Predicate) -> Predicate {
    let zero = match measure.sort() {
        Sort::Int(int_sort) => Predicate::IntLit(edda_refine::IntLit::signed(0, int_sort)),
        _ => Predicate::IntLit(edda_refine::IntLit::signed(
            0,
            IntSort::sized(IntWidth::W64, true),
        )),
    };
    Predicate::cmp(CmpOp::Ge, measure.clone(), zero)
}
