//! §5 termination discharge for `decreases` clauses.
//!
//! Walks a recursive function's body, locates every in-SCC call,
//! substitutes the callee's measure with the call-site arguments, and
//! emits two sub-obligations per recursive call: a strict-decrease
//! `decreases_expr[args] < decreases_expr[params]` and a
//! well-foundedness `decreases_expr[params] >= 0`. Both discharge via
//! LIA for `Int` measures. Only self-recursion is admitted so far; mutual
//! recursion's shared lex-tuple measure lands in C6.
//!
//! # Structural measure: `box_depth(b)` (bug B-008)
//!
//! Recursive walks over `Box(T)`-linked structures (linked lists,
//! trees) have no LIA-expressible decreasing integer measure — the
//! canonical `box_depth(b)` is the structural depth of the Box chain,
//! not an integer the user could write as `n - 1`. The
//! [`box_depth`] sub-module owns the recognition + structural
//! discharge of `decreases box_depth(<single-segment path>)`; this
//! file's LIA path is bypassed when the recogniser matches.
//!
//! # Module layout
//!
//! - [`lets`] — caller-`let` sort scanning + call-result `ensures`
//!   instantiation into the obligation context.
//! - [`measure`] — measure-component lift, well-foundedness /
//!   lex-product builders, and the unliftable-measure diagnostic.
//! - [`collect`] — the in-SCC recursive-call collector.

use std::collections::HashMap;

use smol_str::SmolStr;

use edda_diag::Diagnostics;
use edda_intern::Symbol;
use edda_refine::{
    DischargeFailure, DischargeRoute, ObligationKind, Predicate, RefineError, Sort,
    UnverifiedAnnotation, Z3Backend, lift_clause,
};
use edda_resolve::{BindingId, ResolvedPackage};
use edda_syntax::ast::visit::Visitor;
use edda_syntax::ast::{self, Expr, RefinementKind};

use crate::attr::AttrSet;
use crate::cx::TyCx;
use crate::infer::SccMap;
use crate::lower::LowerCx;
use crate::sig::FnSig;

use super::sort::{FnRefineEnv, build_schema, sort_of_ty};
use super::{discharge_one_routed, fn_decl_for};

mod box_depth;
mod collect;
mod lets;
mod measure;

use box_depth::{discharge_box_depth_termination, match_box_depth_measure};
use collect::RecursiveCallCollector;
use lets::{collect_caller_lets, ensure_facts_for_measure};
use measure::{
    build_well_foundedness_all, call_index_to_external, diagnose_unliftable_measure,
    lex_product_lt, lift_measure_components, signed_measure_components,
};

/// Discharge the termination obligations a function's `decreases` clause
/// induces per `corpus/edda-codex/language/03-verification.md` §5.
///
/// For each in-SCC recursive call within the function body, emits two
/// sub-obligations under [`ObligationKind::TerminationDecreases`]:
/// the strict-decrease `decreases_expr[call_site_args] <
/// decreases_expr[caller_args]` (sub-index 0) and the well-foundedness
/// `decreases_expr[caller_args] >= 0` (sub-index 1, also in the
/// strict-decrease obligation's context). Both discharge via LIA for
/// `Int` measures.
///
/// Non-recursive functions (single-member SCC without a self-edge)
/// return immediately — they trivially terminate regardless of the
/// `decreases` clause's content.
///
/// # No silent termination escape
///
/// A recursive function with **no** `decreases` clause is not handled
/// here — it is caught upstream by the divergence-injection rule
/// ([`crate::infer::SccMap::is_divergent`] /
/// [`crate::check::check_function`]), which injects `Pure(Divergence)`
/// at function entry and forces the author to declare `with {
/// divergence }` or fail the function-exit row-containment check with
/// `effect_row_mismatch`. A recursive function **with** a `decreases`
/// clause reaches this pass, which suppressed that injection; if its
/// measure cannot be lifted into the required-decidable fragment (it is
/// neither an LIA integer measure nor `box_depth(<param>)`), this pass
/// emits a `RefinementUnproven` diagnostic via
/// [`diagnose_unliftable_measure`] rather than returning silently. Every
/// recursive call therefore either exhibits a proven structural /
/// LIA decrease, opts out via the `divergence` effect, or fails
/// verification — there is no path on which termination goes unproven
/// and unflagged.
///
/// Current scope: self-recursion only. Mutual recursion is C6's
/// responsibility (the shared lex-tuple measure). Loop strict-decrease
/// at the back-edge is the next sub-milestone — it needs symbolic
/// state tracking that the current refinement plumbing does not yet
/// supply; the well-foundedness obligation (`m >= 0` at loop entry) is
/// the C5 minimum for loops.
pub(crate) fn discharge_fn_termination(
    fn_decl: &ast::FnDecl,
    sig: &FnSig,
    attrs: &AttrSet,
    ty_cx: &TyCx,
    lower_cx: &LowerCx<'_>,
    scc_map: &SccMap,
    caller_binding: BindingId,
    diags: &mut Diagnostics,
) {
    // No `decreases` clause → nothing to discharge *here*. This is NOT a
    // termination escape: a recursive function without a `decreases`
    // clause has `Pure(Divergence)` injected at entry by
    // `scc::compute_divergent` + `check_function`, so the author must
    // declare `with { divergence }` or fail the function-exit
    // row-containment check (`effect_row_mismatch`). Only functions that
    // DO carry a `decreases` clause (which suppressed that injection)
    // proceed past this point and owe a proven measure below.
    let Some(measure_clause) = fn_decl
        .refinements
        .iter()
        .find(|c| c.kind == RefinementKind::Decreases)
    else {
        return;
    };
    // Non-recursive function → trivially terminates.
    if !scc_map.is_recursive(caller_binding) {
        return;
    }
    let Some(package) = lower_cx.package else {
        return;
    };

    // B-008 structural measure: `decreases box_depth(<single-segment path>)`.
    // Detected and discharged structurally before falling through to the
    // LIA path so the lifter (which would reject `box_depth` as a
    // user-function call) is bypassed entirely.
    if let Some(box_param) =
        match_box_depth_measure(&measure_clause.pred, lower_cx.interner)
    {
        discharge_box_depth_termination(
            fn_decl,
            box_param,
            caller_binding,
            scc_map,
            measure_clause.span,
            lower_cx,
            diags,
        );
        return;
    }

    // Build the per-function param-sort map (same shape as
    // `discharge_fn_clauses`) so the refinement lifter knows the sort
    // of each variable it encounters.
    let param_sorts = build_param_sorts(fn_decl, sig, ty_cx, lower_cx);

    // Scan the body's `let` bindings so a measure that references a
    // call-result local (the natural `decreases hi - lo` across
    // `let p = partition(arr, lo, hi)`) can lift, and so that callee's
    // `ensures` can be threaded into the strict-decrease context below.
    let (local_sorts, let_calls) =
        collect_caller_lets(fn_decl, &param_sorts, ty_cx, lower_cx, package);
    let result_sym = lower_cx.interner.intern("result");

    // Parser-recovery DUMMY fn name → bail; a termination discharge keyed on
    // an unnamed function would produce a degenerate diagnostic.
    let Some(fn_name_text) = lower_cx.interner.try_resolve(fn_decl.name.name) else {
        return;
    };
    let fn_name = SmolStr::new(fn_name_text);
    let env = FnRefineEnv::new(
        package,
        &param_sorts,
        &local_sorts,
        lower_cx.interner,
        lower_cx.ty_interner,
        ty_cx,
    );

    // Build the function-level @unverified route once. Per refinement-
    // decidability.md §9 the trust hatch admits every obligation in the
    // function (including termination sub-obligations) — see C14. Computed
    // before the measure lift so an `@unverified` function — which has
    // explicitly opted out of discharge — does not trip the
    // unliftable-measure diagnostic below.
    let unverified_route = build_unverified_route(attrs, lower_cx);

    // Lift the caller-side measure per component — `m[caller_params]`
    // in the codex notation. A measure outside the predicate fragment is a
    // verification escape: fail it with a
    // `RefinementUnproven` diagnostic unless the function is `@unverified`.
    let m_caller_components = match lift_measure_components(&measure_clause.pred, &env) {
        Ok(components) => components,
        Err(err) => {
            if unverified_route.is_none() {
                diagnose_unliftable_measure(&fn_name, &err, diags);
            }
            return;
        }
    };

    // Well-foundedness: every signed component must be `>= 0`. Unsigned
    // components are structurally non-negative and skipped (bug C14).
    let signed_components = signed_measure_components(&m_caller_components);
    let well_founded_opt = if signed_components.is_empty() {
        None
    } else {
        Some(build_well_foundedness_all(&signed_components))
    };

    // The function's own `requires` clauses hold at entry, where the
    // measure is evaluated — thread them into BOTH the well-foundedness
    // obligation and every strict-decrease obligation. The well-founded
    // predicate itself joins only the strict-decrease context.
    let fn_requires: Vec<Predicate> = fn_decl
        .refinements
        .iter()
        .filter(|c| c.kind == RefinementKind::Requires)
        .filter_map(|c| lift_clause(c, &env).ok())
        .collect();
    let mut requires_context = fn_requires.clone();
    if let Some(ref wf) = well_founded_opt {
        requires_context.push(wf.clone());
    }

    let schema = std::sync::Arc::new(build_schema(ty_cx, lower_cx.ty_interner, lower_cx.interner));
    let solver = Z3Backend::with_schema(schema);
    let body_block = match &fn_decl.body {
        ast::FnBody::Block(b) => b,
        ast::FnBody::Extern { .. } => return,
    };

    // Discharge the well-foundedness obligation on its own (sub-index 1)
    // so a measure like `decreases -x` that can go negative diagnoses
    // independently of any call-site evidence. Skip entirely when every
    // measure component is unsigned (structurally non-negative).
    if let Some(well_founded) = well_founded_opt.clone() {
        discharge_one_routed(
            &solver,
            well_founded,
            fn_requires.clone(),
            measure_clause.span,
            ObligationKind::TerminationDecreases {
                callee: fn_name.clone(),
                call_index: 1,
            },
            unverified_route.clone(),
            diags,
        );
    }

    // Walk the body collecting every Call whose callee resolves to a
    // Function BindingId in the caller's SCC, then emit one strict-
    // decrease obligation per call site.
    let mut collector = RecursiveCallCollector {
        package,
        scc_map,
        caller: caller_binding,
        out: Vec::new(),
    };
    collector.visit_block(body_block);
    let ctx = CallDischargeCtx {
        fn_name: &fn_name,
        param_sorts: &param_sorts,
        local_sorts: &local_sorts,
        let_calls: &let_calls,
        result_sym,
        m_caller_components: &m_caller_components,
        requires_context: &requires_context,
        unverified_route: &unverified_route,
        package,
        lower_cx,
        ty_cx,
        solver: &solver,
    };
    discharge_recursive_calls(collector.out, &ctx, diags);
}

/// Per-function param-sort map (same shape as `discharge_fn_clauses`) so
/// the refinement lifter knows the sort of each variable it encounters.
/// Parameters whose name is a parser-recovery DUMMY or whose type has no
/// predicate-fragment sort are skipped.
fn build_param_sorts(
    fn_decl: &ast::FnDecl,
    sig: &FnSig,
    ty_cx: &TyCx,
    lower_cx: &LowerCx<'_>,
) -> HashMap<Symbol, (SmolStr, Sort)> {
    let mut param_sorts: HashMap<Symbol, (SmolStr, Sort)> = HashMap::new();
    for (param_decl, param_sig) in fn_decl.params.iter().zip(sig.params.iter()) {
        let Some(name_text) = lower_cx.interner.try_resolve(param_decl.name.name) else {
            continue;
        };
        if let Some(sort) = sort_of_ty(param_sig.ty, lower_cx.ty_interner, ty_cx) {
            let name = SmolStr::new(name_text);
            param_sorts.insert(param_decl.name.name, (name, sort));
        }
    }
    param_sorts
}

/// Build the function-level `@unverified` [`DischargeRoute`] when the
/// attribute is in scope. The annotation's reason string is non-empty per
/// the attr validator's contract, so `UnverifiedAnnotation::new` cannot
/// fail. `None` means default-SMT discharge.
fn build_unverified_route(attrs: &AttrSet, lower_cx: &LowerCx<'_>) -> Option<DischargeRoute> {
    attrs.unverified.and_then(|ann| {
        let reason = lower_cx.interner.resolve(ann.reason).to_string();
        UnverifiedAnnotation::new(reason, ann.attr_span)
            .ok()
            .map(DischargeRoute::Unverified)
    })
}

/// Read-only bundle of the per-function discharge inputs shared across
/// every recursive call site, so [`discharge_recursive_calls`] keeps a
/// flat signature.
struct CallDischargeCtx<'a> {
    fn_name: &'a SmolStr,
    param_sorts: &'a HashMap<Symbol, (SmolStr, Sort)>,
    local_sorts: &'a HashMap<Symbol, (SmolStr, Sort)>,
    let_calls: &'a HashMap<Symbol, (BindingId, Vec<Expr>)>,
    result_sym: Symbol,
    m_caller_components: &'a [Predicate],
    requires_context: &'a [Predicate],
    unverified_route: &'a Option<DischargeRoute>,
    package: &'a ResolvedPackage,
    lower_cx: &'a LowerCx<'a>,
    ty_cx: &'a TyCx,
    solver: &'a Z3Backend,
}

/// Emit one strict-decrease obligation per collected in-SCC recursive
/// call. Each site substitutes the callee's measure with the call-site
/// args, checks SCC-shared-measure consistency (callee declares a
/// `decreases`, arities agree), and discharges the lex-product decrease
/// under [`ObligationKind::TerminationDecreases`].
fn discharge_recursive_calls(
    sites: Vec<collect::RecursiveCall>,
    ctx: &CallDischargeCtx<'_>,
    diags: &mut Diagnostics,
) {
    let mut call_index: u32 = 0;
    for site in sites {
        let Some(callee_fn_decl) = fn_decl_for(site.callee, ctx.package) else {
            continue;
        };
        let Some(callee_measure_clause) = callee_fn_decl
            .refinements
            .iter()
            .find(|c| c.kind == RefinementKind::Decreases)
        else {
            // Mutual-recursion SCC consistency: every member must
            // carry a decreases clause. Missing one means the SCC's
            // shared lex-product is undefined; report on the *call
            // site* (not the callee's signature) so the diagnostic
            // points at the offending code path.
            let callee_name =
                SmolStr::new(ctx.lower_cx.interner.resolve(callee_fn_decl.name.name));
            diags.push(
                RefineError::discharge(
                    site.call_span,
                    ObligationKind::TerminationDecreases {
                        callee: callee_name,
                        call_index: 1,
                    },
                    "callee in same SCC must declare a `decreases` measure",
                    DischargeFailure::Unknown {
                        reason: Some(
                            "mutual-recursion SCC member missing `decreases`".to_string(),
                        ),
                    },
                )
                .to_diagnostic(),
            );
            continue;
        };
        // Build (callee-param → arg-expression) substitution.
        let mut subst: HashMap<Symbol, Expr> = HashMap::new();
        for (param_decl, arg) in callee_fn_decl.params.iter().zip(site.args.iter()) {
            subst.insert(param_decl.name.name, arg.clone());
        }
        let measure_substituted =
            super::substitute_paths(&callee_measure_clause.pred, &subst, ctx.package);

        // For mutual recursion the substituted measure references the
        // callee's parameter names; the lifter's env is keyed by the
        // CALLER's parameter sorts. Build a per-call env that includes
        // both sets so paths resolve. (C6 minimum: the callee-sort
        // scaffolding trusts the SCC-shared measure invariant.)
        let combined_sorts = ctx.param_sorts.clone();
        let call_env = FnRefineEnv::new(
            ctx.package,
            &combined_sorts,
            ctx.local_sorts,
            ctx.lower_cx.interner,
            ctx.lower_cx.ty_interner,
            ctx.ty_cx,
        );
        // The call-site measure `m[args]` must also lie in the
        // predicate fragment for the strict-decrease obligation to be
        // dischargeable.
        let m_args_components = match lift_measure_components(&measure_substituted, &call_env) {
            Ok(components) => components,
            Err(err) => {
                if ctx.unverified_route.is_none() {
                    diagnose_unliftable_measure(ctx.fn_name, &err, diags);
                }
                call_index += 1;
                continue;
            }
        };
        // Component-arity must agree across caller and callee per the
        // SCC-shared-measure rule. Mismatch surfaces a diagnostic.
        if m_args_components.len() != ctx.m_caller_components.len() {
            let callee_name =
                SmolStr::new(ctx.lower_cx.interner.resolve(callee_fn_decl.name.name));
            diags.push(
                RefineError::discharge(
                    site.call_span,
                    ObligationKind::TerminationDecreases {
                        callee: callee_name,
                        call_index: 1,
                    },
                    format!(
                        "decreases tuple arity mismatch: caller has {}, callee `{}` has {}",
                        ctx.m_caller_components.len(),
                        ctx.lower_cx.interner.resolve(callee_fn_decl.name.name),
                        m_args_components.len(),
                    ),
                    DischargeFailure::Unknown {
                        reason: Some(
                            "SCC-shared decreases tuple arity must match".to_string(),
                        ),
                    },
                )
                .to_diagnostic(),
            );
            continue;
        }
        // Lex-product strict-decrease across all components.
        let goal = lex_product_lt(&m_args_components, ctx.m_caller_components);
        // Thread the postconditions of any call-result local this measure
        // references into the obligation context (sound).
        let mut call_context = ctx.requires_context.to_vec();
        ensure_facts_for_measure(
            &measure_substituted,
            ctx.let_calls,
            ctx.local_sorts,
            ctx.result_sym,
            ctx.package,
            &call_env,
            &mut call_context,
        );
        discharge_one_routed(
            ctx.solver,
            goal,
            call_context,
            site.call_span,
            ObligationKind::TerminationDecreases {
                callee: ctx.fn_name.clone(),
                call_index: call_index_to_external(call_index),
            },
            ctx.unverified_route.clone(),
            diags,
        );
        call_index += 1;
    }
}
