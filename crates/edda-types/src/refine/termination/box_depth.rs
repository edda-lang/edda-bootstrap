//! Structural termination for `decreases box_depth(<single-path>)`.
//!
//! Recognises and discharges the Box-depth structural measure form
//! introduced by bug B-008. The lifter in `edda-refine` rejects
//! `box_depth(b)` as an unadmitted user-function call (and would
//! continue to do so even after this fix — the form is unsuitable for
//! the LIA predicate fragment). Recognition and discharge therefore
//! live above the lifter, in the termination pass.
//!
//! # Why it's sound
//!
//! `Box(T)` in Edda models a heap-allocated owned pointer with no
//! cyclic-construction primitive available in safe source — every
//! `Box_T.new(value, allocator)` consumes its argument, so the inner
//! payload is owned by exactly one Box and the chain is finite. The
//! depth of the chain is therefore a well-founded measure: it is a
//! non-negative integer (zero at a leaf) that decreases by exactly one
//! at each `Box_T.borrow(child)` / field-deref step.
//!
//! V1.0 ships the minimum-precision check:
//!
//! - **Well-foundedness** — trivially discharged; `box_depth(b) >= 0`
//!   holds by construction.
//! - **Strict decrease** — enforced *syntactically* at every recursive
//!   call: the call-site argument paired with the callee's boxed
//!   parameter must be a different expression from the *caller's*
//!   boxed parameter name. A user recursing through
//!   `Box_T.borrow(boxed)`, `t.left`, or any sub-expression that
//!   descends into the box passes the check; a user re-passing the
//!   unchanged box (`f(b)`) is diagnosed.
//!
//! # Multi-member SCCs
//!
//! For mutual recursion across an SCC, every member declares its own
//! `decreases box_depth(<param>)` clause — possibly on a differently-
//! named parameter at a different positional index. At each in-SCC call
//! site the check uses the *callee's* box parameter index (from its own
//! decreases clause) to pick out the relevant argument, and compares
//! that argument against the *caller's* box parameter name. SCC members
//! that fail to declare a box_depth measure are diagnosed at the call
//! site as SCC consistency violations.
//!
//! The structural check is conservative — it accepts any expression
//! that isn't the literal caller-parameter name. A future revision can
//! tighten to "must read through a Box-deref operation" once the
//! typechecker exposes a Box-shape oracle.

use edda_diag::Diagnostics;
use edda_intern::{Interner, Symbol};
use edda_refine::{DischargeFailure, ObligationKind, RefineError};
use edda_resolve::BindingId;
use edda_span::Span;
use edda_syntax::ast::visit::Visitor;
use edda_syntax::ast::{self, Expr, ExprKind, RefinementKind};
use smol_str::SmolStr;

use crate::infer::SccMap;
use crate::lower::LowerCx;

use super::RecursiveCallCollector;
use crate::refine::fn_decl_for;

//            `<ident>` resolves to the literal text "box_depth" — anything else
//            falls through to the LIA path
/// Recognise the Box-depth measure form: a call to the
/// reserved identifier `box_depth` with exactly one argument that is a
/// single-segment path (i.e. a bare parameter name).
///
/// Returns the parameter's interned symbol on a match, `None` otherwise.
pub(super) fn match_box_depth_measure(measure: &Expr, interner: &Interner) -> Option<Symbol> {
    let ExprKind::Call { callee, args } = &measure.kind else {
        return None;
    };
    if args.len() != 1 {
        return None;
    }
    let arg = &args[0];
    if arg.mode.is_some() {
        return None;
    }
    let callee_path = match &callee.kind {
        ExprKind::Path(p) if p.segments.len() == 1 => p,
        _ => return None,
    };
    let callee_text = interner.resolve(callee_path.segments[0].name);
    if callee_text != BOX_DEPTH_MEASURE {
        return None;
    }
    let ExprKind::Path(arg_path) = &arg.expr.kind else {
        return None;
    };
    if arg_path.segments.len() != 1 {
        return None;
    }
    Some(arg_path.segments[0].name)
}

//            argument is structurally identical to the caller's Box parameter
//            (i.e. the call passes `b` unchanged); accepts every other
//            argument shape as a structural decrease
//            at the *callee's* box parameter index — looked up via the
//            callee's own `decreases box_depth(<param>)` clause — not the
//            caller's index, so SCCs whose members put their box parameter
//            at different positions still discharge correctly
//            clause; a call to an in-SCC member missing one (or carrying a
//            non-box_depth measure) is diagnosed as an SCC-consistency
//            violation at the call site
//          Z3 entirely (well-foundedness is structural; strict-decrease is
//          enforced by AST-shape diff)
/// Discharge the `decreases box_depth(<box_param>)` termination measure
/// for `fn_decl`. Walks every in-SCC recursive call; for each, locates
/// the callee's own box-parameter slot via its `decreases` clause and
/// checks that the call-site argument at that slot is *not* the literal
/// caller `<box_param>` path — i.e. the user is passing some expression
/// that descends into the Box (a borrow, a field projection, a method
/// call, …). When the check fails, emits a `RefinementUnproven`
/// diagnostic with a hint pointing at the structural-decrease rule.
/// When it succeeds, emits nothing — the well-foundedness sub-
/// obligation is structural for `box_depth` and the strict-decrease is
/// the syntactic check itself.
pub(super) fn discharge_box_depth_termination(
    fn_decl: &ast::FnDecl,
    box_param: Symbol,
    caller_binding: BindingId,
    scc_map: &SccMap,
    measure_span: Span,
    lower_cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
) {
    let Some(package) = lower_cx.package else {
        return;
    };
    let body_block = match &fn_decl.body {
        ast::FnBody::Block(b) => b,
        ast::FnBody::Extern { .. } => return,
    };
    // Verify the boxed parameter is actually a parameter of this
    // function — protects against typos that would otherwise silently
    // skip discharge (lifter would also flag the unresolved path, but
    // the user's mental model is "I named a real parameter").
    if !fn_decl.params.iter().any(|p| p.name.name == box_param) {
        let fn_name = SmolStr::new(lower_cx.interner.resolve(fn_decl.name.name));
        diags.push(
            RefineError::discharge(
                measure_span,
                ObligationKind::TerminationDecreases {
                    callee: fn_name,
                    call_index: 1,
                },
                "`box_depth(<param>)` argument must be a parameter of this function",
                DischargeFailure::Unknown {
                    reason: Some(
                        "`box_depth` decreases-measure argument did not match any parameter name"
                            .to_string(),
                    ),
                },
            )
            .to_diagnostic(),
        );
        return;
    }

    let mut collector = RecursiveCallCollector {
        package,
        scc_map,
        caller: caller_binding,
        out: Vec::new(),
    };
    collector.visit_block(body_block);

    let fn_name = SmolStr::new(lower_cx.interner.resolve(fn_decl.name.name));
    let mut call_index: u32 = 0;
    for site in collector.out {
        let Some(callee_fn_decl) = fn_decl_for(site.callee, package) else {
            // Callee binding has no AST FnDecl (extern, missing) — skip
            // silently; this can only happen for non-Function bindings
            // the collector should have filtered out, but be defensive.
            continue;
        };

        // Locate the callee's own `decreases box_depth(<callee_box_param>)`
        // clause. Cross-function calls within a multi-member SCC index
        // into the call-site arg list using the *callee's* box_param
        // position, not the caller's — these may differ when SCC members
        // declare their box parameter at different positional indices.
        let Some(callee_box_param) = callee_box_param_for(callee_fn_decl, lower_cx.interner)
        else {
            // SCC-consistency violation: every member of an SCC under a
            // box_depth measure must declare its own box_depth clause.
            // Diagnose at the call site so the user sees the offending
            // edge, not the unrelated callee's signature.
            diags.push(
                RefineError::discharge(
                    site.call_span,
                    ObligationKind::TerminationDecreases {
                        callee: fn_name.clone(),
                        call_index: 2u32.saturating_add(call_index),
                    },
                    format!(
                        "in-SCC callee `{}` must declare `decreases box_depth(<param>)` — \
                         every member of an SCC under a box_depth measure shares the \
                         structural-decrease form",
                        lower_cx.interner.resolve(callee_fn_decl.name.name),
                    ),
                    DischargeFailure::Unknown {
                        reason: Some(
                            "multi-member SCC requires every member to declare \
                             `decreases box_depth(<param>)`"
                                .to_string(),
                        ),
                    },
                )
                .to_diagnostic(),
            );
            call_index = call_index.saturating_add(1);
            continue;
        };

        let callee_param_index = callee_fn_decl
            .params
            .iter()
            .position(|p| p.name.name == callee_box_param);
        let Some(callee_param_index) = callee_param_index else {
            // Callee's box_depth references a non-parameter; its own
            // discharge call will diagnose. Skip here.
            call_index = call_index.saturating_add(1);
            continue;
        };

        // Skip calls that don't reach the box parameter slot — short
        // arg lists arise on partial-arity callers (shouldn't happen
        // for well-typed in-SCC recursive calls, but defensive).
        let Some(arg) = site.args.get(callee_param_index) else {
            continue;
        };

        // Strict-decrease: the argument occupying the callee's box slot
        // must not be the literal *caller's* box parameter. Passing the
        // caller's box unchanged across any in-SCC edge would let the
        // recursion loop without descending.
        if expr_is_path_to(arg, box_param) {
            diags.push(
                RefineError::discharge(
                    site.call_span,
                    ObligationKind::TerminationDecreases {
                        callee: fn_name.clone(),
                        call_index: 2u32.saturating_add(call_index),
                    },
                    format!(
                        "`box_depth({param})` requires the recursive call to pass a strict sub-box \
                         (e.g. `Box_T.borrow(child)` or a Box-typed field) — got the unchanged \
                         parameter `{param}`",
                        param = lower_cx.interner.resolve(box_param),
                    ),
                    DischargeFailure::Unknown {
                        reason: Some(
                            "structural Box-depth decrease requires a different argument expression"
                                .to_string(),
                        ),
                    },
                )
                .to_diagnostic(),
            );
        }
        call_index = call_index.saturating_add(1);
    }
}

//            clause when one is present, or `None` when `decl` declares no decreases
//            clause / a non-box_depth measure / a malformed box_depth argument
/// Extract the box-parameter symbol from a function declaration's
/// `decreases box_depth(<param>)` clause, if present. Returns `None`
/// when the function declares no `decreases` clause, declares a
/// non-box_depth measure (LIA tuple, scalar, etc.), or the box_depth
/// argument doesn't match the recogniser's single-segment-path shape.
fn callee_box_param_for(decl: &ast::FnDecl, interner: &Interner) -> Option<Symbol> {
    let clause = decl
        .refinements
        .iter()
        .find(|c| c.kind == RefinementKind::Decreases)?;
    match_box_depth_measure(&clause.pred, interner)
}

//            single segment whose interned name equals `target`; any wrapping
//            (parentheses, casts, field accesses, method calls) fails the test
/// `true` when `expr` is the literal single-segment path `target`.
/// Used by [`discharge_box_depth_termination`] to detect the
/// "recursive call passes the box unchanged" anti-pattern.
fn expr_is_path_to(expr: &Expr, target: Symbol) -> bool {
    match &expr.kind {
        ExprKind::Path(p) => p.segments.len() == 1 && p.segments[0].name == target,
        _ => false,
    }
}

/// Reserved measure name recognised by [`match_box_depth_measure`].
/// There is no `box_depth` function in scope in user code — the
/// recogniser matches purely on identifier text, so any user-defined
/// `box_depth(...)` would shadow this form. That shadowing is a
/// follow-up concern; for now the name is reserved by convention.
const BOX_DEPTH_MEASURE: &str = "box_depth";
