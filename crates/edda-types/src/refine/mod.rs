//! Feature-gated refinement discharge: lifts requires/ensures clauses through Z3.
//!
//! This module is compiled only when the `refine` Cargo feature is active.
//! It owns four discharge passes — each in its own submodule:
//!
//! - [`sort`] — sort projection and the per-function [`sort::FnRefineEnv`]
//!   [`edda_refine::PredicateEnv`] backing every lift.
//! - [`clauses`] — `requires` / `ensures` clause discharge at function
//!   definition.
//! - [`call_precondition`] — callee `requires` discharge at call sites,
//!   substituting call-site arguments for the callee's parameters.
//! - [`termination`] — §5 `decreases` discharge at recursive call sites,
//!   with its in-SCC call walker.
//! - [`coherence_preservation`] — SMT-precise hook the
//!   [`crate::coherence`] structural classifier calls when a region
//!   shows a `Mutated` parameter; tries to prove preservation under
//!   the function's `requires` context.
//!
//! `Where` clauses (type-level invariants) apply at struct-construction
//! sites and are deferred to a later wave.

use std::collections::HashMap;
use std::time::Duration;

use edda_diag::Diagnostics;
use edda_intern::Symbol;
use edda_refine::{
    DischargeFailure, DischargeRoute, Obligation, ObligationKind, Predicate, RefineError, Solver,
    Z3Backend,
};
use edda_resolve::{BindingId, BindingKind, Resolved, ResolvedPackage};
use edda_span::Span;
use edda_syntax::ast::{self, Expr, ExprKind, FnDecl};

mod call_precondition;
mod clauses;
mod coherence_preservation;
mod sort;
mod termination;

#[cfg(test)]
mod tests;

pub(crate) use call_precondition::discharge_fn_call_preconditions;
pub(crate) use clauses::discharge_fn_clauses;
pub(crate) use coherence_preservation::try_coherence_preservation_smt;
pub(crate) use sort::{FnRefineEnv, build_schema, sort_of_ty};
pub(crate) use termination::discharge_fn_termination;

/// Single-obligation discharge helper — mirrors the per-clause loop in
/// `discharge_fn_clauses` so the termination path emits diagnostics
/// through the same surface format.
#[allow(dead_code)]
pub(crate) fn discharge_one(
    solver: &Z3Backend,
    goal: Predicate,
    context: Vec<Predicate>,
    site: Span,
    kind: ObligationKind,
    diags: &mut Diagnostics,
) {
    discharge_one_routed(solver, goal, context, site, kind, None, diags);
}

/// Discharge helper that lets the caller pin a non-SMT
/// [`DischargeRoute`]. The termination pass uses this to honour a
/// function-level `@unverified` annotation across every
/// `TerminationDecreases` sub-obligation (bug C14: previously
/// `@unverified` only suppressed body verification, not the termination
/// measure check). `None` means default-SMT — equivalent to
/// [`discharge_one`].
pub(crate) fn discharge_one_routed(
    solver: &Z3Backend,
    goal: Predicate,
    context: Vec<Predicate>,
    site: Span,
    kind: ObligationKind,
    route: Option<DischargeRoute>,
    diags: &mut Diagnostics,
) {
    let mut obligation = Obligation::new(goal, context, site, kind, "");
    if let Some(r) = route {
        obligation = obligation.with_route(r);
    }
    let outcome = solver.discharge(&obligation, Duration::from_secs(5));
    if let Some(failure) = DischargeFailure::from_outcome(outcome) {
        let err = RefineError::discharge(
            site,
            obligation.kind().clone(),
            obligation.predicate_text(),
            failure,
        );
        diags.push(err.to_diagnostic());
    }
}

/// Look up a binding's `FnDecl` in its owning module's AST. Mirrors the
/// helper in `infer::scc` but returns the declaration node so callers
/// can read the `decreases` clause / parameter list directly.
pub(crate) fn fn_decl_for<'p>(
    binding: BindingId,
    package: &'p ResolvedPackage,
) -> Option<&'p FnDecl> {
    let entry = package.binding(binding);
    let module_entry = package.module_entry(binding.module);
    for item in &module_entry.ast.items {
        if let ast::ItemKind::Function(fd) = &item.kind
            && fd.name.name == entry.name
        {
            return Some(fd);
        }
    }
    None
}

/// Substitute parameter references in `expr` with their call-site
/// argument expressions. Used to build `decreases_expr[call_site_args]`
/// from `decreases_expr` (which references the callee's parameter
/// names) and the call's positional arg list.
pub(crate) fn substitute_paths(
    expr: &Expr,
    subst: &HashMap<Symbol, Expr>,
    package: &ResolvedPackage,
) -> Expr {
    fn go(expr: &Expr, subst: &HashMap<Symbol, Expr>, package: &ResolvedPackage) -> Expr {
        if let ExprKind::Path(path) = &expr.kind
            && path.segments.len() == 1
            && let Some(replacement) = subst.get(&path.segments[0].name)
        {
            return replacement.clone();
        }
        // `base.method(args)` parses as `Call { callee: Path([base, method]),
        // args }` — multi-segment paths absorb the dot (see the lifter's
        // `lift_method_call_shape`). When `base` is a substituted parameter
        // the two-segment path slips past the single-segment check above, so
        // a callee clause like `offset + 8 <= bytes.len()` kept the callee's
        // `bytes` name after substitution; its callee-side span then resolved
        // in the caller's frame and NAME-CAPTURED any same-named caller
        // binding (`u64_from_le_bytes` /
        // `from_bytes` obligations checked against the CALLER's unrelated
        // `bytes` param). Rewrite to a `MethodCall` on the replacement so the
        // clause references the actual argument; when the replacement doesn't
        // lift (e.g. a caller local outside the sort env) the clause skips
        // conservatively instead of mis-resolving.
        if let ExprKind::Call { callee, args } = &expr.kind
            && let ExprKind::Path(path) = &callee.kind
            && path.segments.len() == 2
            && let Some(replacement) = subst.get(&path.segments[0].name)
        {
            return Expr {
                span: expr.span,
                kind: ExprKind::MethodCall {
                    receiver: Box::new(replacement.clone()),
                    name: path.segments[1],
                    args: args
                        .iter()
                        .map(|a| ast::CallArg {
                            span: a.span,
                            mode: a.mode,
                            name: a.name,
                            expr: go(&a.expr, subst, package),
                        })
                        .collect(),
                },
            };
        }
        // Same name-capture hazard, plain-field-access shape: `d.nanos`
        // (no trailing call) parses as a bare multi-segment `Path`
        // (`parse_path` greedily eats `ident(.ident)*` at the primary
        // level — `ExprKind::Field` only arises when the receiver isn't
        // itself a path, e.g. `f().field`). When `d` is a substituted
        // parameter the head-segment-only check above never fires for
        // `segments.len() > 1`, so a callee clause like `requires
        // d.nanos >= 0` kept the callee's own `nanos` segment (and
        // span) after substitution; left as-is, a caller-side resolver
        // pass over the "substituted" clause would still see the
        // callee's `d` binding at that span and could silently
        // NAME-CAPTURE an unrelated same-named caller binding — the
        // same hazard fixed for the method-call-callee shape,
        // now closed for ordinary field projection too.
        // Rewrite to a `Field` chain rooted at the replacement so every
        // downstream consumer (the lifter, `field_refinement_facts`)
        // sees the actual argument's own identity, never the callee's.
        if let ExprKind::Path(path) = &expr.kind
            && path.segments.len() > 1
            && let Some(replacement) = subst.get(&path.segments[0].name)
        {
            let mut built = replacement.clone();
            for seg in &path.segments[1..] {
                built = Expr {
                    span: expr.span,
                    kind: ExprKind::Field {
                        receiver: Box::new(built),
                        name: *seg,
                    },
                };
            }
            return built;
        }
        let kind = match &expr.kind {
            ExprKind::Binary { op, lhs, rhs } => ExprKind::Binary {
                op: *op,
                lhs: Box::new(go(lhs, subst, package)),
                rhs: Box::new(go(rhs, subst, package)),
            },
            ExprKind::Unary { op, expr: inner } => ExprKind::Unary {
                op: *op,
                expr: Box::new(go(inner, subst, package)),
            },
            ExprKind::Call { callee, args } => ExprKind::Call {
                callee: Box::new(go(callee, subst, package)),
                args: args
                    .iter()
                    .map(|a| ast::CallArg {
                        span: a.span,
                        mode: a.mode,
                        name: a.name,
                        expr: go(&a.expr, subst, package),
                    })
                    .collect(),
            },
            ExprKind::Field { receiver, name } => ExprKind::Field {
                receiver: Box::new(go(receiver, subst, package)),
                name: *name,
            },
            ExprKind::TupleIndex { receiver, index } => ExprKind::TupleIndex {
                receiver: Box::new(go(receiver, subst, package)),
                index: *index,
            },
            ExprKind::Index { receiver, index } => ExprKind::Index {
                receiver: Box::new(go(receiver, subst, package)),
                index: Box::new(go(index, subst, package)),
            },
            // Cast / method-call / tuple measures must substitute through to
            // their operands too, else a recursive call's `m[args]` keeps the
            // caller's parameter names inside the cast/receiver/element and
            // the strict-decrease obligation degenerates to `m < m`.
            ExprKind::Cast { expr: inner, ty, mode } => ExprKind::Cast {
                expr: Box::new(go(inner, subst, package)),
                ty: ty.clone(),
                mode: *mode,
            },
            ExprKind::MethodCall { receiver, name, args } => ExprKind::MethodCall {
                receiver: Box::new(go(receiver, subst, package)),
                name: *name,
                args: args
                    .iter()
                    .map(|a| ast::CallArg {
                        span: a.span,
                        mode: a.mode,
                        name: a.name,
                        expr: go(&a.expr, subst, package),
                    })
                    .collect(),
            },
            ExprKind::Tuple(elems) => {
                ExprKind::Tuple(elems.iter().map(|e| go(e, subst, package)).collect())
            }
            ExprKind::Array(elems) => {
                ExprKind::Array(elems.iter().map(|e| go(e, subst, package)).collect())
            }
            _ => expr.kind.clone(),
        };
        Expr {
            span: expr.span,
            kind,
        }
    }
    go(expr, subst, package)
}

/// Resolve `callee` to its in-package Function [`BindingId`]. Mirrors
/// the helper in `infer/scc.rs` but skips the `Resolutions` borrow
/// dance since this caller already has the package handle in scope.
pub(crate) fn resolve_function_callee(
    callee: &Expr,
    package: &ResolvedPackage,
) -> Option<BindingId> {
    let ExprKind::Path(path) = &callee.kind else {
        return None;
    };
    let resolved = package.resolutions().lookup_path(path.span)?;
    let Resolved::Binding(binding) = resolved else {
        return None;
    };
    let entry = package.binding(binding);
    if !matches!(entry.kind, BindingKind::Function) {
        return None;
    }
    Some(binding)
}
