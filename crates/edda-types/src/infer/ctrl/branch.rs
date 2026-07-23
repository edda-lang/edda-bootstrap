//! Branching control-flow inference — `if` / `match` and the branch-type join.
//!
//! Implements T-If-Synth / T-Match-Synth and their check-mode mirrors.
//! Each branch runs against a clone of the entering [`TyEnv`]; per-branch
//! states are folded by GLB at the join, and branch value types unify
//! via [`join_types`] (with `never` absorption).

use edda_diag::DiagnosticClass;
use edda_intern::Symbol;
use edda_span::Span;
use edda_syntax::ast::Literal;

use crate::hir::{HirBlock, HirExpr, HirMatchArm, HirPatKind};
use crate::prim::Primitive;
use crate::ty::{TyId, TyKind};

use super::super::pat::bind_pat;
use super::super::{InferCx, TyEnv, check_expr, synth_block, synth_expr};
use super::{block_always_exits, expr_always_exits};

/// Synthesise an `if cond { ... } else { ... }` expression.
///
/// Per `inference-rules.md §4`, *Branching merges states by GLB*: the
/// two branches each run against a clone of the entering [`TyEnv`];
/// after both arms execute, the surviving env merges them
/// element-wise with [`TyEnv::merge_glb`]. A binding consumed in one
/// branch and not the other ends up `Consumed` at the join — the
/// only path that can reuse it is the impossible one.
///
/// A branch that always exits the function (`return` / `raise` /
/// `panic`) can never reach the join, so its states are dropped rather
/// than GLB-merged — folding them in would drag a binding initialised
/// only in the surviving arm back to `Uninit`, a false `binding
/// uninitialised`.
pub(crate) fn synth_if(
    env: &mut TyEnv,
    cond: &mut HirExpr,
    then_block: &mut HirBlock,
    else_branch: Option<&mut HirExpr>,
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    let bool_ty = ic.lower.ty_interner.prim(Primitive::Bool);
    check_expr(env, cond, bool_ty, ic);

    // Snapshot pre-branch state for the GLB merge.
    let entering = env.clone();
    let then_ty = synth_block(env, then_block, ic);
    let then_state = env.clone();

    // Reset to the entering state to run the else-branch on equal terms.
    *env = entering;
    let (else_ty, else_exits) = match else_branch {
        Some(e) => {
            let ty = synth_expr(env, e, ic);
            (ty, expr_always_exits(e))
        }
        None => (ic.lower.ty_interner.prim(Primitive::Unit), false),
    };
    // Join the two arms' binding states, dropping an arm that always
    // exits the function: its state can never reach the join, and
    // folding it in by GLB would poison the surviving arm's
    // initialisations with pre-branch state — a false `binding
    // uninitialised` on the fall-through arm's writes (mirrors
    // ctrl::synth_handle). Both-fall-through and both-diverge keep the plain GLB.
    match (block_always_exits(then_block), else_exits) {
        (false, true) => *env = then_state,
        (true, false) => {}
        _ => env.merge_glb(&then_state),
    }

    join_types(then_ty, else_ty, ic, span)
}

/// Check an `if cond { ... } else { ... }` expression against `expected`.
///
/// Symmetric to [`synth_if`] but pushes `expected` into both branches
/// so integer-literal narrowing reaches branch trailing expressions
/// (`if c { 7 } else { 0 } ⇐ i32` narrows both `7` and `0`). A missing
/// else clause forces the if-as-statement form (trailing must be `()`);
/// that's enforced via the trailing-expr check on the synth path.
pub(crate) fn check_if(
    env: &mut TyEnv,
    cond: &mut HirExpr,
    then_block: &mut HirBlock,
    else_branch: Option<&mut HirExpr>,
    expected: TyId,
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    let bool_ty = ic.lower.ty_interner.prim(Primitive::Bool);
    check_expr(env, cond, bool_ty, ic);

    let entering = env.clone();
    let then_ty = super::super::check_block(env, then_block, expected, ic);
    let then_state = env.clone();
    *env = entering;
    let (else_ty, else_exits) = match else_branch {
        Some(e) => {
            check_expr(env, e, expected, ic);
            (e.ty, expr_always_exits(e))
        }
        None => (ic.lower.ty_interner.prim(Primitive::Unit), false),
    };
    // Drop an always-exit arm from the binding-state join (see
    // [`synth_if`]). Divergence is read
    // structurally from the HIR shape, not the stamped `ty`: check-mode
    // `never`-absorption coerces a diverging arm's `ty` to `expected`,
    // so `ty` alone cannot tell us the arm never reaches the join.
    match (block_always_exits(then_block), else_exits) {
        (false, true) => *env = then_state,
        (true, false) => {}
        _ => env.merge_glb(&then_state),
    }

    // Either branch may diverge (`return …`) and contribute `never`;
    // the result type is the join, which collapses to `expected` when
    // any branch matched.
    join_types(then_ty, else_ty, ic, span)
}

/// Check a `match` expression against `expected`. See [`synth_match`]
/// for the scrutinee / pattern handling; this variant simply drives
/// each arm body through [`check_expr`] with `expected`.
pub(crate) fn check_match(
    env: &mut TyEnv,
    scrutinee: &mut HirExpr,
    arms: &mut [HirMatchArm],
    expected: TyId,
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    let scrut_ty = synth_expr(env, scrutinee, ic);
    check_match_exhaustiveness(scrutinee, arms, ic);
    if arms.is_empty() {
        return ic.lower.ty_interner.prim(Primitive::Never);
    }
    let bool_ty = ic.lower.ty_interner.prim(Primitive::Bool);
    let entering = env.clone();
    let mut accumulated: Option<TyEnv> = None;
    let mut joined: Option<TyId> = None;
    for arm in arms.iter_mut() {
        *env = entering.clone();
        env.enter_scope();
        bind_pat(env, &mut arm.pat, scrut_ty, ic);
        check_pat_against_scrutinee(&arm.pat.kind, scrut_ty, ic);
        if let Some(guard) = &mut arm.guard {
            check_expr(env, guard, bool_ty, ic);
        }
        check_expr(env, &mut arm.body, expected, ic);
        let body_ty = arm.body.ty;
        env.exit_scope();

        // Skip an always-exit arm from the binding-state fold (see
        // [`synth_match`]). Divergence is
        // read structurally, not from the stamped `ty`: check-mode
        // `never`-absorption coerces a diverging arm's `body.ty` to
        // `expected`.
        if !expr_always_exits(&arm.body) {
            match accumulated.as_mut() {
                None => accumulated = Some(env.clone()),
                Some(acc) => acc.merge_glb(env),
            }
        }
        joined = Some(match joined {
            None => body_ty,
            Some(prev) => join_types(prev, body_ty, ic, span),
        });
    }
    // Every arm always exits ⇒ no arm reaches the join; restore the
    // entering state so the (unreachable) continuation sees a coherent
    // env rather than the last diverging arm's post-state.
    match accumulated {
        Some(post) => *env = post,
        None => *env = entering,
    }
    joined.unwrap_or_else(|| ic.lower.ty_interner.prim(Primitive::Never))
}

/// Synthesise a `match scrutinee { arms... }` expression.
///
/// Each arm runs against a clone of the entering [`TyEnv`]; the
/// per-arm post-states are folded by GLB into the surviving env so
/// any binding moved by one arm becomes `Consumed` at the join.
pub(crate) fn synth_match(
    env: &mut TyEnv,
    scrutinee: &mut HirExpr,
    arms: &mut [HirMatchArm],
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    let scrut_ty = synth_expr(env, scrutinee, ic);
    check_match_exhaustiveness(scrutinee, arms, ic);
    if arms.is_empty() {
        // Empty match — the spec doesn't admit this; treat as
        // the never-yielding form (matches an uninhabited scrutinee).
        return ic.lower.ty_interner.prim(Primitive::Never);
    }
    let bool_ty = ic.lower.ty_interner.prim(Primitive::Bool);
    let entering = env.clone();
    let mut accumulated: Option<TyEnv> = None;
    let mut joined: Option<TyId> = None;
    for arm in arms.iter_mut() {
        // Run each arm on a fresh copy of the entering state.
        *env = entering.clone();
        env.enter_scope();
        bind_pat(env, &mut arm.pat, scrut_ty, ic);
        check_pat_against_scrutinee(&arm.pat.kind, scrut_ty, ic);
        if let Some(guard) = &mut arm.guard {
            check_expr(env, guard, bool_ty, ic);
        }
        let body_ty = synth_expr(env, &mut arm.body, ic);
        env.exit_scope();

        // Fold this arm's post-state into the accumulator — unless the
        // arm always exits the function, in which case its state can
        // never reach the join and folding it in by GLB would poison the
        // surviving arms' initialisations with pre-arm state (mirrors
        // ctrl::synth_handle).
        if !expr_always_exits(&arm.body) {
            match accumulated.as_mut() {
                None => accumulated = Some(env.clone()),
                Some(acc) => acc.merge_glb(env),
            }
        }
        joined = Some(match joined {
            None => body_ty,
            Some(prev) => join_types(prev, body_ty, ic, span),
        });
    }
    // Every arm always exits ⇒ no arm reaches the join; restore the
    // entering state so the (unreachable) continuation sees a coherent
    // env rather than the last diverging arm's post-state.
    match accumulated {
        Some(post) => *env = post,
        None => *env = entering,
    }
    joined.unwrap_or_else(|| ic.lower.ty_interner.prim(Primitive::Never))
}

/// Light pattern-check against the scrutinee type. Only
/// literal-pattern types are validated; structural mismatch (variant /
/// struct on a non-matching scrutinee) is deferred territory.
fn check_pat_against_scrutinee(kind: &HirPatKind, scrut_ty: TyId, ic: &mut InferCx<'_>) {
    if scrut_ty == ic.ty_error() {
        return;
    }
    if let HirPatKind::Literal(lit) = kind {
        let lit_ty = match lit {
            Literal::Int { .. } => ic.lower.ty_interner.prim(Primitive::I64),
            Literal::Float(_) => ic.lower.ty_interner.prim(Primitive::F64),
            Literal::Bool(_) => ic.lower.ty_interner.prim(Primitive::Bool),
            Literal::Str(_) => ic.lower.ty_interner.prim(Primitive::String),
            Literal::Unit => ic.lower.ty_interner.prim(Primitive::Unit),
        };
        // Integer literals match any integer primitive scrutinee — same
        // narrowing rule as expressions. Other literals must agree
        // structurally.
        if !matches!(lit, Literal::Int { .. }) && lit_ty != scrut_ty {
            let lit_display = ic.display_ty(lit_ty).to_string();
            let scrut_display = ic.display_ty(scrut_ty).to_string();
            ic.emit_typecheck_error(
                Span::DUMMY,
                format!(
                    "literal pattern of type `{lit_display}` does not match scrutinee of type `{scrut_display}`",
                ),
            );
        }
    }
}

/// Emit [`DiagnosticClass::NonExhaustiveMatch`] when `scrutinee`'s sum
/// type has a declared variant that no unguarded arm covers and the
/// match has no unguarded irrefutable arm (`_` / a bare binding).
///
/// Mirrors the native pass
/// (`compiler/lib/types/src/check/pass/exhaustive`) one-for-one — same
/// coverage rules, same diagnostic class, same message — so the
/// bootstrap stops false-negatively accepting non-exhaustive matches.
fn check_match_exhaustiveness(scrutinee: &HirExpr, arms: &[HirMatchArm], ic: &mut InferCx<'_>) {
    let binding = match ic.lower.ty_interner.kind(scrutinee.ty) {
        TyKind::Nominal(b) => *b,
        _ => return,
    };
    // `variants()` is empty for product types and zero-variant sums; both
    // are vacuously exhaustive, so an empty list means nothing to check.
    let variant_names: Vec<Symbol> = match ic.ty_cx.type_decl(binding) {
        Some(info) => info.variants().iter().map(|v| v.name).collect(),
        None => return,
    };
    if variant_names.is_empty() {
        return;
    }
    let has_catch_all = arms
        .iter()
        .any(|arm| arm.guard.is_none() && pat_is_irrefutable(&arm.pat.kind));
    if has_catch_all {
        return;
    }
    let all_covered = variant_names.iter().all(|&v| {
        arms.iter()
            .any(|arm| arm.guard.is_none() && pat_covers_variant(&arm.pat.kind, v))
    });
    if !all_covered {
        ic.emit_diagnostic(
            DiagnosticClass::NonExhaustiveMatch,
            scrutinee.span,
            "non-exhaustive match: the scrutinee sum type has variants not covered by any arm; add the missing case arms or a `case _` wildcard",
        );
    }
}

/// `true` for a pattern that matches every value of its type with no
/// guard — a wildcard, a bare binding, or (defensively) the
/// lowering-error sentinel (a diagnostic was already reported there, so
/// suppress a cascading exhaustiveness error).
fn pat_is_irrefutable(kind: &HirPatKind) -> bool {
    matches!(
        kind,
        HirPatKind::Wildcard | HirPatKind::Binding(_) | HirPatKind::Error
    )
}

/// `true` when `kind` covers the sum variant named `v` — any irrefutable
/// pattern, or a variant pattern whose final path segment names `v`
/// (`.north` and a qualified `Direction.north` both match by leaf name).
fn pat_covers_variant(kind: &HirPatKind, v: Symbol) -> bool {
    match kind {
        HirPatKind::Wildcard | HirPatKind::Binding(_) | HirPatKind::Error => true,
        HirPatKind::Variant { path, .. } => {
            path.segments.last().map_or(false, |seg| seg.name == v)
        }
        _ => false,
    }
}

/// Join two branch types per `inference-rules.md §1a.4` T-If-Synth:
/// either they agree exactly, or one diverges (`never`) and the other
/// wins. `Error` is the absorbing element — propagates silently.
pub(super) fn join_types(a: TyId, b: TyId, ic: &mut InferCx<'_>, span: Span) -> TyId {
    let err = ic.ty_error();
    let never = ic.lower.ty_interner.prim(Primitive::Never);
    if a == err || b == err {
        return err;
    }
    if a == b {
        return a;
    }
    if a == never {
        return b;
    }
    if b == never {
        return a;
    }
    let a_display = ic.display_ty(a).to_string();
    let b_display = ic.display_ty(b).to_string();
    ic.emit_typecheck_error(
        span,
        format!("branches have incompatible types: `{a_display}` vs `{b_display}`"),
    );
    err
}
