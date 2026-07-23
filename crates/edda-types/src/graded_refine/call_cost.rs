//! Call-site graded-bound lift for §5.4 cost accumulation.
//!
//! A call credits the callee's declared graded bound for the kind under
//! discharge, after substituting the callee's parameters with the
//! call-site argument expressions.

use std::collections::HashMap;

use edda_intern::Symbol;
use edda_refine::{Predicate, lift_predicate};
use edda_resolve::{BindingId, BindingKind, Resolved, ResolvedPackage};
use edda_syntax::ast::{CallArg, Expr, ExprKind, FnDecl};

use crate::effect::GradedBound;
use crate::refine::{fn_decl_for, substitute_paths};

use super::body_cost::cost_of_expr;
use super::lia::add_costs;
use super::GradedCx;

/// Cost contribution of a single call site for one kind.
///
/// Sums the argument-expression sub-costs (the args themselves may
/// contain calls or branches), then adds the callee's lifted graded
/// bound for this kind when present. Callees without an entry for the
/// kind contribute zero — matching Phase A's "no entry, no cost"
/// posture; the missing-kind diagnostic stays the responsibility of
/// `crate::graded::discharge_fn_graded_calls`.
pub(super) fn cost_of_call(
    callee: &Expr,
    args: &[CallArg],
    cx: &GradedCx<'_>,
) -> Option<Predicate> {
    let mut acc = cost_of_expr(callee, cx)?;
    for a in args {
        acc = add_costs(acc,cost_of_expr(&a.expr, cx)?);
    }
    let Some(callee_binding) = resolve_callee_binding(callee, cx.package) else {
        return Some(acc);
    };
    let Some(callee_sig) = cx.ty_cx.sig(callee_binding) else {
        return Some(acc);
    };
    let Some(callee_bound) = callee_sig.graded_bounds.iter().find(|gb| gb.kind == cx.kind) else {
        return Some(acc);
    };
    let Some(callee_fn_decl) = fn_decl_for(callee_binding, cx.package) else {
        return Some(acc);
    };
    let lifted = lift_callee_bound(callee_bound, callee_fn_decl, args, cx)?;
    Some(add_costs(acc,lifted))
}

/// Resolve a callee expression to its Function `BindingId`. Returns
/// `None` for non-Path callees, unresolved paths, or non-Function
/// bindings.
fn resolve_callee_binding(callee: &Expr, package: &ResolvedPackage) -> Option<BindingId> {
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

/// Lift a callee's graded-bound RHS (already extracted by sig-lowering)
/// through the caller's env, after substituting callee parameters with
/// the call-site argument expressions.
fn lift_callee_bound(
    callee_bound: &GradedBound,
    callee_fn_decl: &FnDecl,
    args: &[CallArg],
    cx: &GradedCx<'_>,
) -> Option<Predicate> {
    let mut subst: HashMap<Symbol, Expr> = HashMap::new();
    for (param_decl, arg) in callee_fn_decl.params.iter().zip(args.iter()) {
        subst.insert(param_decl.name.name, arg.expr.clone());
    }
    let substituted = substitute_paths(&callee_bound.bound, &subst, cx.package);
    lift_predicate(&substituted, cx.env).ok()
}
