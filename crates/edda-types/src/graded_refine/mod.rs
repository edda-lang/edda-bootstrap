//! Graded-effect bound discharge per `02-modes-effects-refinements.md` §5.4.
//!
//! Implements the §5.4 structural composition rules over the body of a
//! function that declares a graded `with { kind(<var> <= EXPR) }` entry:
//!
//! - **Straight-line code** — costs add. Sequential statements / trailing
//!   expressions sum into one [`Predicate`] per kind via [`Predicate::add`].
//! - **Branches** (`if`/`else`, `match`) — costs take the max. Encoded as
//!   `if then_cost >= else_cost then then_cost else else_cost` via
//!   [`Predicate::if_then_else`].
//! - **Bounded loops** — `for x in <iter>` lifts to
//!   `iter_count * body_cost`. Range literals (`lo..hi`, `lo..=hi`)
//!   contribute `hi - lo` (`+1` when closed); slice-typed iter
//!   expressions contribute [`Predicate::slice_len`]. Either lift
//!   requires one factor be an integer literal so the result stays in
//!   LIA per `refinement-decidability.md` §4.
//! - **Unbounded loops** (`loop { ... }`) — graded bounds are
//!   satisfiable only when the loop body's cost is zero for the kind;
//!   a non-zero body cost causes the obligation to fail at Z3.
//!
//! Per kind on the caller's signature, lifts the declared bound through
//! [`edda_refine::lift_predicate`], builds the accumulated body cost as
//! a [`Predicate`], and discharges
//! `caller_bound >= body_cost` under [`ObligationKind::GradedBound`].
//! Unliftable / out-of-fragment forms (closures, spawn, user method
//! calls, non-LIA arithmetic) collapse the whole kind's discharge to a
//! silent admit — matching the "never under-rejects" posture used in
//! [`crate::refine::discharge_fn_clauses`] for unsupported clauses.
//!
//! # Module layout
//!
//! - [`body_cost`] — the per-block / per-statement / per-expression cost
//!   accumulator ([`body_cost::cost_of_block`] and friends).
//! - [`call_cost`] — call-site graded-bound lift.
//! - [`lia`] — integer-sorted predicate helpers and the `for`-loop
//!   iteration-count derivation, all kept inside the LIA fragment.

mod body_cost;
mod call_cost;
mod lia;

use std::collections::HashMap;

use smol_str::SmolStr;

use edda_diag::Diagnostics;
use edda_intern::Symbol;
use edda_refine::{
    CmpOp, ObligationKind, Predicate, Sort, Z3Backend, lift_clause, lift_predicate,
};
use edda_resolve::ResolvedPackage;
use edda_syntax::ast::{Block, FnBody, FnDecl, RefinementKind};

use crate::cx::TyCx;
use crate::effect::{GradedBound, GradedKind};
use crate::lower::LowerCx;
use crate::refine::{FnRefineEnv, build_schema, discharge_one, sort_of_ty};
use crate::sig::FnSig;

use body_cost::cost_of_block;
use lia::slice_placeholder_sort;

/// Per-discharge context bundled so `cost_of_*` helpers don't drag a
/// six-argument tuple through every recursion site. Constructed once per
/// `(fn_decl, kind)` pair inside [`discharge_fn_graded_bounds`].
pub(super) struct GradedCx<'a> {
    /// `PredicateEnv` impl backed by the caller's parameter sorts.
    pub(super) env: &'a FnRefineEnv<'a>,
    /// Resolved package — used to resolve callee paths to bindings and
    /// to thread through [`crate::refine::substitute_paths`].
    pub(super) package: &'a ResolvedPackage,
    /// Typechecker context — used to fetch callee `FnSig`s so the
    /// already-extracted graded-bound RHS is recovered without re-walking
    /// the AST.
    pub(super) ty_cx: &'a TyCx,
    /// Graded kind under discharge for this pass.
    pub(super) kind: GradedKind,
    /// Cost sort — pinned to the caller's lifted bound sort so every
    /// `Predicate::add` / `Predicate::cmp` arm sees consistent operands.
    pub(super) sort: edda_refine::IntSort,
}

//            has populated `sig.graded_bounds`; matches `discharge_fn_clauses`'
//            precondition so the per-function environment is consistent
/// Per-kind graded-bound discharge for one function declaration.
///
/// For every graded entry on `sig`, builds the body's cumulative cost
/// for that kind per §5.4 (sum / branch-max / loop-lift), lifts the
/// caller's declared bound, and discharges
/// `caller_bound >= body_cost` through the Z3 backend under
/// [`ObligationKind::GradedBound`]. The obligation's `requires_context`
/// is the conjunction of the caller's `requires` clauses so callers
/// like `requires paths.len() >= 3` can prove parametric bounds hold.
///
/// Functions without graded entries, extern-bodied functions, and
/// unit-test lowering scenarios with no resolved package are
/// short-circuited.
pub(crate) fn discharge_fn_graded_bounds(
    fn_decl: &FnDecl,
    sig: &FnSig,
    ty_cx: &TyCx,
    lower_cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
) {
    if sig.graded_bounds.is_empty() {
        return;
    }
    let Some(package) = lower_cx.package else {
        return;
    };
    let body_block = match &fn_decl.body {
        FnBody::Block(b) => b,
        FnBody::Extern { .. } => return,
    };

    let mut param_sorts: HashMap<Symbol, (SmolStr, Sort)> = HashMap::new();
    for (param_decl, param_sig) in fn_decl.params.iter().zip(sig.params.iter()) {
        // Parser-recovery DUMMY name → skip; a graded-bound discharge over an
        // unnamed parameter would produce a degenerate predicate.
        let Some(name_text) = lower_cx.interner.try_resolve(param_decl.name.name) else {
            continue;
        };
        let sort = sort_of_ty(param_sig.ty, lower_cx.ty_interner, ty_cx)
            .or_else(|| slice_placeholder_sort(param_sig.ty, lower_cx.ty_interner));
        if let Some(sort) = sort {
            let name = SmolStr::new(name_text);
            param_sorts.insert(param_decl.name.name, (name, sort));
        }
    }
    // Graded-bound discharge ranges over parameters only; no caller
    // `let`-bound locals participate, so pass an empty local-sort map.
    let no_locals: HashMap<Symbol, (SmolStr, Sort)> = HashMap::new();
    let env = FnRefineEnv::new(
        package,
        &param_sorts,
        &no_locals,
        lower_cx.interner,
        lower_cx.ty_interner,
        ty_cx,
    );
    // Parser-recovery DUMMY fn name → bail; a graded discharge keyed on an
    // unnamed function would produce a degenerate diagnostic.
    let Some(fn_name_text) = lower_cx.interner.try_resolve(fn_decl.name.name) else {
        return;
    };
    let fn_name = SmolStr::new(fn_name_text);
    let schema = std::sync::Arc::new(build_schema(ty_cx, lower_cx.ty_interner, lower_cx.interner));
    let solver = Z3Backend::with_schema(schema);

    let requires_context: Vec<Predicate> = fn_decl
        .refinements
        .iter()
        .filter(|c| c.kind == RefinementKind::Requires)
        .filter_map(|c| lift_clause(c, &env).ok())
        .collect();

    for caller_bound in sig.graded_bounds.iter() {
        discharge_one_kind(
            caller_bound,
            body_block,
            &fn_name,
            &env,
            package,
            ty_cx,
            &solver,
            &requires_context,
            diags,
        );
    }
}

/// Discharge one (kind, caller bound) pair against the body's accumulated cost.
#[allow(clippy::too_many_arguments)]
fn discharge_one_kind(
    caller_bound: &GradedBound,
    body_block: &Block,
    fn_name: &SmolStr,
    env: &FnRefineEnv<'_>,
    package: &ResolvedPackage,
    ty_cx: &TyCx,
    solver: &Z3Backend,
    requires_context: &[Predicate],
    diags: &mut Diagnostics,
) {
    let Ok(caller_pred) = lift_predicate(&caller_bound.bound, env) else {
        return;
    };
    let cost_sort = match caller_pred.sort() {
        Sort::Int(s) => s,
        _ => return,
    };
    let cx = GradedCx {
        env,
        package,
        ty_cx,
        kind: caller_bound.kind,
        sort: cost_sort,
    };
    let Some(body_cost) = cost_of_block(body_block, &cx) else {
        return;
    };
    let goal = Predicate::cmp(CmpOp::Ge, caller_pred, body_cost);
    discharge_one(
        solver,
        goal,
        requires_context.to_vec(),
        caller_bound.span,
        ObligationKind::GradedBound {
            kind: SmolStr::new(caller_bound.kind.as_str()),
            callee: fn_name.clone(),
        },
        diags,
    );
}
