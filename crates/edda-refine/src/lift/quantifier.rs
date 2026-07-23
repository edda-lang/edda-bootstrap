//! `forall` / `exists` bounded-quantifier lifts.
//!
//! Both forms have shape `<quant> <bound> in <iter>: <body>` where:
//! - `<iter>` is a range expression (`lo..<hi`, `lo..=hi`, or `lo..`); the
//!   range form is mandatory in V1.0 — slice-element iteration
//!   (`forall x in xs: P`) is reserved for v1.x.
//! - `<body>` is a `Bool`-sorted predicate over the bound variable.
//! - `<bound>` is a fresh local visible only inside `<body>`; the resolver
//!   declares it as `BindingKind::Local`; this module wires the bound's
//!   sort into the [`PredicateEnv`] for the duration of the body lift via
//!   `push_quantifier_bound` / `pop_quantifier_bound`.
//!
//! The lifter normalises the half-open `lo..<hi` form to canonical bounds;
//! `lo..=hi` lowers to `lo..<(hi + 1)`. The bound's sort is taken from the
//! range endpoints' sort (typically `usize` for slice-length bounds).

use edda_span::Span;
use edda_syntax::ast::{Expr, ExprKind, Ident, RangeKind};

use crate::error::LiftError;
use crate::predicate::{IntLit, Predicate, Variable};
use crate::sort::{IntSort, Sort};

use super::PredicateEnv;
use super::literal::int_lit_value_from_u128;

pub(super) fn lift_forall(
    bound: &Ident,
    iter: &Expr,
    body: &Expr,
    site: Span,
    env: &dyn PredicateEnv,
) -> Result<Predicate, LiftError> {
    let (bound_var, lower, upper, body_pred) = lift_quantifier_pieces(bound, iter, body, site, env)?;
    Ok(Predicate::forall(bound_var, lower, upper, body_pred))
}

pub(super) fn lift_exists(
    bound: &Ident,
    iter: &Expr,
    body: &Expr,
    site: Span,
    env: &dyn PredicateEnv,
) -> Result<Predicate, LiftError> {
    let (bound_var, lower, upper, body_pred) = lift_quantifier_pieces(bound, iter, body, site, env)?;
    Ok(Predicate::exists(bound_var, lower, upper, body_pred))
}

// Shared piece-builder for both quantifier kinds. Returns the bound
// variable, the (canonicalised) lower/upper bounds, and the lifted body.
fn lift_quantifier_pieces(
    bound: &Ident,
    iter: &Expr,
    body: &Expr,
    site: Span,
    env: &dyn PredicateEnv,
) -> Result<(Variable, Predicate, Predicate, Predicate), LiftError> {
    // Iter must be a Range — V1.0 admits only the bounded-range form.
    let (lo, hi, kind) = match &iter.kind {
        ExprKind::Range {
            lo: Some(lo),
            hi: Some(hi),
            kind,
        } => (lo.as_ref(), hi.as_ref(), *kind),
        ExprKind::Range { .. } => {
            return Err(LiftError::Unsupported {
                what: "open-ended range in quantifier iterable — bounded \
                       `lo..<hi` or `lo..=hi` form required"
                    .to_string(),
                span: iter.span,
            });
        }
        _ => {
            return Err(LiftError::Unsupported {
                what: "non-range quantifier iterable — V1.0 admits only \
                       `forall <bound> in <range>: <body>` (slice-element \
                       iteration is reserved for v1.x)"
                    .to_string(),
                span: iter.span,
            });
        }
    };

    // Determine the bound's sort from the range endpoint's inferred sort.
    let lo_sort = env.expr_sort(lo).ok_or(LiftError::UnresolvedPath { span: lo.span })?;
    let bound_sort = match lo_sort {
        Sort::Int(s) => Sort::Int(s),
        other => {
            return Err(LiftError::SortMismatch {
                span: lo.span,
                expected: format!("integer (got {other:?})"),
            });
        }
    };

    // Lift the lower and upper bounds.
    let lower = super::lift_predicate(lo, env)?;
    let upper_raw = super::lift_predicate(hi, env)?;
    // Canonicalise: `lo..=hi` becomes `lo..<(hi + 1)`.
    let upper = match kind {
        RangeKind::HalfOpen => upper_raw,
        RangeKind::Closed => add_one(upper_raw, bound_sort.clone(), site)?,
    };

    // Build the bound variable. The name is the bound ident's interned text.
    let bound_name = env.ident_name(bound);
    let bound_var = Variable::new(bound_name, bound_sort.clone());

    // Lift body with the bound visible. The env's push/pop is stack-balanced
    // so even if the body lift fails the pop still runs.
    env.push_quantifier_bound(bound, bound_sort);
    let body_result = super::lift_predicate(body, env);
    env.pop_quantifier_bound(bound);
    let body_pred = body_result?;

    Ok((bound_var, lower, upper, body_pred))
}

// Build `expr + 1` at the supplied integer sort. Used to canonicalise the
// closed-range upper bound into a half-open form.
fn add_one(expr: Predicate, sort: Sort, site: Span) -> Result<Predicate, LiftError> {
    let int_sort = match sort {
        Sort::Int(s) => s,
        other => {
            return Err(LiftError::SortMismatch {
                span: site,
                expected: format!("integer (got {other:?})"),
            });
        }
    };
    let one = make_int_one(int_sort, site)?;
    Ok(Predicate::add(expr, one))
}

fn make_int_one(sort: IntSort, site: Span) -> Result<Predicate, LiftError> {
    let value = int_lit_value_from_u128(1, sort, site)?;
    Ok(Predicate::IntLit(IntLit { value, sort }))
}
