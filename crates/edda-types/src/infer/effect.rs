//! Effect-discharge inference arms — `handle` and `?`.
//!
//! - `synth_handle` enters a `handle <effect>: <ty> -> recovery { body }`
//!   block, pushes the handled entry onto the discharged-effects stack
//!   while walking the body, and joins the body and recovery value
//!   types. Only `err: T` handlers are admitted so far.
//! - `synth_try` enforces the `e?` check-and-pass-through rule per
//!   `effect-tracking.md §3`: every `err: E` propagated by `inner` must
//!   appear in the enclosing function's declared row or be discharged
//!   by some open handler.

use edda_span::Span;

use crate::effect::{EffectEntry, PureEffect};
use crate::hir::{HirBlock, HirExpr};
use crate::ty::TyId;

use super::{InferCx, TyEnv, check_expr, synth_block, synth_expr};

/// Synthesise a `handle <effect>: <ty> [as <binder>] -> recovery { body }` expression.
///
/// Only `err: T` handlers are admitted so far. The body's `err: T` row
/// contribution is discharged at the handler boundary so it does not
/// propagate to the enclosing function's exit check. `recovery` is
/// checked against the body's synthesised type — both arms of the
/// handler must agree on the result type. When `binder` is `Some`,
/// a fresh scope is opened around `recovery` with `<binder>` bound
/// to `handled_ty` so the recovery expression can name the caught
/// payload.
///
/// Per `inference-rules.md §4`, *Branching merges states by GLB*:
/// `body` and `recovery` never both execute at runtime (the handler
/// runs `recovery` instead of returning from `body`, not after it), so
/// they are forked from the same entering [`TyEnv`] and their resulting
/// states are merged by [`TyEnv::merge_glb`] — mirroring
/// [`super::ctrl::synth_if`] one-for-one.
///
/// The merge is divergence-aware: an arm that always exits the
/// function ([`super::ctrl::expr_always_exits`]) can never reach the
/// join, so its states are dropped rather than merged. This is what
/// admits the standing `uninit x` / `init x`-in-body / `raise`-recovery
/// pattern — the recovery arm leaves
/// `x` uninitialised but also never falls through to the code that
/// reads `x`.
pub(super) fn synth_handle(
    env: &mut TyEnv,
    effect: edda_syntax::ast::Ident,
    handled_ty: TyId,
    binder: Option<edda_syntax::ast::Ident>,
    recovery: &mut HirExpr,
    body: &mut HirBlock,
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    let effect_name = ic.lower.interner.resolve(effect.name).to_string();
    if effect_name != "err" {
        ic.emit_typecheck_error(
            effect.span,
            format!(
                "handlers only admit the `err` effect; found `{effect_name}`",
            ),
        );
    }

    // Snapshot pre-handler state for the GLB merge — `body` and
    // `recovery` are exclusive paths, exactly like an `if`/`else` pair.
    let entering = env.clone();

    let handled_entry = EffectEntry::Pure(PureEffect::Err(handled_ty));
    let checkpoint = ic.row.checkpoint();
    ic.discharged_effects.push(handled_entry);
    let body_ty = synth_block(env, body, ic);
    ic.discharged_effects.pop();
    ic.row.discharge_since(checkpoint, &handled_entry);
    let body_state = env.clone();

    // Reset to the entering state to run `recovery` on equal terms.
    *env = entering;

    let error_ty = ic.ty_error();
    // Open a fresh scope around `recovery` when the source bound the
    // caught payload — the binder must not leak into the enclosing
    // function's environment.
    if let Some(b) = binder {
        env.enter_scope();
        env.bind(b.name, handled_ty);
    }
    let result_ty = if body_ty == error_ty {
        let _ = synth_expr(env, recovery, ic);
        error_ty
    } else {
        check_expr(env, recovery, body_ty, ic);
        body_ty
    };
    if binder.is_some() {
        env.exit_scope();
    }

    // Join the two arms' binding states. An arm that always exits the
    // function can never reach this join, so its states are dropped —
    // folding them in would poison the surviving arm's initialisations
    // with pre-arm state. When neither
    // (or both) arm exits, fall back to the plain GLB merge.
    match (
        super::ctrl::block_always_exits(body),
        super::ctrl::expr_always_exits(recovery),
    ) {
        (false, true) => *env = body_state,
        (true, false) => {
            // Join reached only through `recovery` — keep its state,
            // already in `env`.
        }
        _ => env.merge_glb(&body_state),
    }

    // Suppress the unused-binding lint on `span` — kept for symmetry
    // with neighbouring synth_* helpers that route diagnostics through it.
    let _ = span;
    result_ty
}

/// Synthesise `e?` per `effect-tracking.md §3`.
///
/// `?` is a *check-and-pass-through* operator on the row side. The
/// rule has two parts:
///
/// 1. **At least one `err: E` to propagate.** If `inner`'s contributed
///    row has no `err: E` entries, the `?` is ill-formed — there is
///    nothing to propagate. Diagnostic: `no error effect to propagate`.
/// 2. **Every `err: E` is declared at the enclosing function.** The
///    [`super::check_fn_body`] row-containment check fires
///    `effect_row_mismatch` for any propagated entry the function did
///    not declare; this site does not duplicate the diagnostic.
///
/// The value type is `inner`'s synthesised type (there is no
/// `Result<T, E>` yet, so the "success case" is the inner's stated type
/// per §3, *Formal rule*).
pub(super) fn synth_try(
    env: &mut TyEnv,
    inner: &mut HirExpr,
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    let checkpoint = ic.row.checkpoint();
    // B22: mark the operand walk as being in propagator position so
    // call typers can recognise `call(...)?` as an authorised
    // statement-position call. The counter is restored after the inner
    // walk so sibling expressions (and parent-level calls) are not
    // falsely admitted. The flag authorises ONLY the call the `?` is
    // attached to: `check_arg_expr` / `synth_arg_expr`, the method
    // receiver walk, and the indirect-callee walk re-zero it, so a
    // nested fallible call needs its own `?` — MIR's `lower_try` wires
    // the on-error successor for exactly one call, and admitting a
    // nested unwrapped call left its err-edge unwired and miscompiled.
    ic.try_position_depth = ic.try_position_depth.saturating_add(1);
    let inner_ty = synth_expr(env, inner, ic);
    ic.try_position_depth = ic.try_position_depth.saturating_sub(1);
    if inner_ty == ic.ty_error() {
        // Inner already failed to synth; cascade silently. Any partial
        // row contribution stays in the accumulator — that matches the
        // §3 pass-through semantics and the function-exit check will
        // surface row issues coherently.
        return ic.ty_error();
    }
    let propagated: Vec<EffectEntry> = ic
        .row
        .entries_since(checkpoint)
        .iter()
        .copied()
        .filter(|entry| matches!(entry, EffectEntry::Pure(PureEffect::Err(_))))
        .collect();
    if propagated.is_empty() {
        ic.emit_typecheck_error(span, "no error effect to propagate");
        return ic.ty_error();
    }
    // Localized declared-row check: every propagated `err: T` must
    // either appear in the enclosing function's declared row or match
    // an effect discharged by some enclosing `handle err: T -> …`.
    // The function-exit row-containment check covers the function-row
    // case globally; emitting at the `?` site gives the user a
    // pointer to the exact propagation that fails when neither path
    // admits the entry.
    for entry in propagated {
        if ic.declared_row.contains(&entry) {
            continue;
        }
        if ic.discharged_effects.contains(&entry) {
            continue;
        }
        let rendered = match entry {
            EffectEntry::Pure(PureEffect::Err(t)) => {
                format!("err: {}", ic.display_ty(t))
            }
            _ => continue,
        };
        let declared = ic
            .declared_row
            .display(ic.lower.interner, ic.lower.ty_interner)
            .to_string();
        ic.emit_typecheck_error(
            span,
            format!(
                "`?` propagates `{rendered}` but neither the enclosing function nor an open handler declares it; declared row is `{declared}`",
            ),
        );
    }
    inner_ty
}
