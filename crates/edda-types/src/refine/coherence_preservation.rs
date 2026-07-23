//! SMT-precise upgrade path for coherence-region preservation.
//!
//! Called from [`crate::coherence`] when its structural classifier
//! reports a `Mutated` parameter inside a `scope(coherence)` region.
//! This hook tries to prove `P[expr/x]` under the function's
//! `requires` context — if Z3 says SAT-unsat (i.e., the goal is
//! valid), the conservative diagnostic is suppressed.
//!
//! Current scope: handles the *single direct assignment* case only.
//! Multiple assignments, field/index assignments, mutable-mode
//! call-site arguments, or any non-fragment expression cause the
//! discharge to fall back to the conservative diagnostic.

use std::collections::HashMap;
use std::time::Duration;

use smol_str::SmolStr;

use edda_intern::Symbol;
use edda_refine::{
    DischargeFailure, Obligation, ObligationKind, Predicate, Solver, Sort, Z3Backend, lift_clause,
    lift_predicate,
};
use edda_syntax::ast::visit::{self as ast_visit, Visitor};
use edda_syntax::ast::{self, Block, Expr, ExprKind, FnDecl, RefinementKind, Stmt, StmtKind, Type, TypeKind};

use crate::cx::TyCx;
use crate::lower::LowerCx;
use crate::sig::FnSig;

use super::sort::{FnRefineEnv, build_schema, field_refinement_facts, sort_of_ty};
use super::substitute_paths;

/// Attempt to prove that a coherence region body preserves a
/// refined-`mutable` parameter's refinement. Returns `true` to
/// suppress the conservative diagnostic.
///
/// Scope: handles the *single direct assignment* case — the region
/// body contains exactly one assignment of the form `param = expr`
/// where `expr` lifts into the predicate fragment. Multiple
/// assignments, field/index assignments, mutable-mode call-site
/// arguments, or any non-fragment expression cause the discharge to
/// fall back to the conservative diagnostic.
///
/// The obligation is `P[expr/param]` under the function's `requires`
/// clauses (so callers can rely on entry-state invariants when
/// proving preservation). Wave-C ships this minimum precision win;
/// follow-ups can extend to straight-line assignment chains and
/// post-condition reuse from method calls.
pub(crate) fn try_coherence_preservation_smt(
    param_sym: Symbol,
    region_body: &Block,
    fn_decl: &FnDecl,
    sig: &FnSig,
    ty_cx: &TyCx,
    lower_cx: &LowerCx<'_>,
) -> bool {
    let Some(package) = lower_cx.package else {
        return false;
    };
    // Find the parameter's refinement predicate. Bail when the type
    // does not nest a `where` clause (the caller already filters on
    // refined-mutable params, so this is a defensive check rather
    // than the primary gate).
    let Some(param_decl) = fn_decl.params.iter().find(|p| p.name.name == param_sym) else {
        return false;
    };
    let Some(pred_expr) = first_refinement_pred(&param_decl.ty) else {
        return false;
    };
    // Find the single direct `param = expr` assignment in the region.
    // Bail on zero (no mutation but structural classifier said
    // otherwise → indirect mutation we don't model), multiple, or
    // assignments with a non-Path LHS.
    let mut collector = DirectAssignCollector {
        sym: param_sym,
        out: Vec::new(),
    };
    collector.visit_block(region_body);
    if collector.out.len() != 1 {
        return false;
    }
    let new_value = collector.out[0];

    // Build the predicate environment for the function (shared shape
    // with `discharge_fn_clauses`).
    let mut param_sorts: HashMap<Symbol, (SmolStr, Sort)> = HashMap::new();
    for (param_decl, param_sig) in fn_decl.params.iter().zip(sig.params.iter()) {
        // Parser-recovery DUMMY name → skip; a coherence-preservation
        // discharge keyed on an unnamed parameter would produce a degenerate
        // predicate.
        let Some(name_text) = lower_cx.interner.try_resolve(param_decl.name.name) else {
            continue;
        };
        if let Some(sort) = sort_of_ty(param_sig.ty, lower_cx.ty_interner, ty_cx) {
            let name = SmolStr::new(name_text);
            param_sorts.insert(param_decl.name.name, (name, sort));
        }
    }
    // Coherence-preservation discharge ranges over parameters only; no
    // caller `let`-bound locals participate, so pass an empty local-sort map.
    let no_locals: HashMap<Symbol, (SmolStr, Sort)> = HashMap::new();
    let env = FnRefineEnv::new(
        package,
        &param_sorts,
        &no_locals,
        lower_cx.interner,
        lower_cx.ty_interner,
        ty_cx,
    );

    // Substitute the parameter symbol with the new-value expression
    // and lift the result. The substitution rewrites every
    // single-segment Path(param_sym) → new_value clone.
    let mut subst: HashMap<Symbol, Expr> = HashMap::new();
    subst.insert(param_sym, new_value.clone());
    let substituted = substitute_paths(pred_expr, &subst, package);
    let Ok(goal) = lift_predicate(&substituted, &env) else {
        return false;
    };

    // Carry the function's `requires` clauses as context so the
    // discharge can rely on entry-state invariants when proving
    // preservation. `where`-style refinements on the parameter type
    // do not enter the context — they are what we're trying to
    // re-prove.
    let mut context: Vec<Predicate> = fn_decl
        .refinements
        .iter()
        .filter(|c| c.kind == RefinementKind::Requires)
        .filter_map(|c| lift_clause(c, &env).ok())
        .collect();
    // The substituted goal may project into the new value's refined
    // fields — those facts hold unconditionally, same as
    // the `requires`-derived context above. `new_value` is a same-
    // function-frame expression (the region's own assignment RHS), so
    // this fold carries none of `discharge_call_site`'s cross-frame
    // span hazard.
    context.extend(field_refinement_facts(&substituted, &env));

    // The discharge result is consumed as a bool below; the kind tag is
    // internal-only here (no diagnostic is emitted from this helper).
    // `EnsuresAtReturn { clause_index: 0 }` is a structural placeholder
    // until a dedicated `CoherencePreservation` variant lands.
    let obligation = Obligation::new(
        goal,
        context,
        new_value.span,
        ObligationKind::EnsuresAtReturn { clause_index: 0 },
        "",
    );
    let schema = std::sync::Arc::new(build_schema(ty_cx, lower_cx.ty_interner, lower_cx.interner));
    let solver = Z3Backend::with_schema(schema);
    let outcome = solver.discharge(&obligation, Duration::from_secs(5));
    DischargeFailure::from_outcome(outcome).is_none()
}

/// Return the first `where`-clause predicate inside `ty`. Mirrors the
/// `first_refinement_span` helper in `coherence.rs` but returns the
/// AST expression node so the lifter can consume it.
fn first_refinement_pred(ty: &Type) -> Option<&Expr> {
    match &ty.kind {
        TypeKind::Refined { pred, .. } => Some(pred),
        TypeKind::Slice(inner) => first_refinement_pred(inner),
        _ => None,
    }
}

/// Collects every direct `param = expr` assignment whose LHS is a
/// single-segment Path to `sym`. Compound assignments (`+=`, `-=`, …)
/// are *not* collected — they require modeling the LHS's prior value,
/// which exceeds the wave-C scope.
struct DirectAssignCollector<'ast> {
    sym: Symbol,
    out: Vec<&'ast Expr>,
}

impl<'ast> Visitor<'ast> for DirectAssignCollector<'ast> {
    fn visit_stmt(&mut self, stmt: &'ast Stmt) {
        if let StmtKind::Assign { target, rhs, op, .. } = &stmt.kind
            && let ExprKind::Path(p) = &target.kind
            && p.segments.len() == 1
            && p.segments[0].name == self.sym
            && matches!(op, ast::AssignOp::Plain)
        {
            self.out.push(rhs);
        }
        ast_visit::walk_stmt(self, stmt);
    }
}
