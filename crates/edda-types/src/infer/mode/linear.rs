//! `linear`-consumption discipline — the §6 "consumed exactly once"
//! rule for the *unconsumed* half.
//!
//! The double-consume / use-after-move half is enforced by the §4 mode
//! lattice (`take` drives `Valid → Consumed`; a later read of a
//! `Consumed` binding is `use_after_move`). This module adds the
//! complementary check: a `linear` binding that reaches the end of its
//! lexical scope still `Valid` (never consumed) leaks an owned resource.
//!
//! `consume_moved_linear` realises the move semantics the §4 lattice
//! does not — a by-value move of a bare `linear` binding (the `return`
//! operand, a `let`/`=` right-hand side, a `take`-receiver method call,
//! a block's trailing value) drives `Valid → Consumed`, so a legitimate
//! hand-off is not mistaken for a leak. `check_block_exit_linear` then
//! sweeps the about-to-be-popped frame: every `Valid` `linear` binding
//! left in it is reported as `linear_unconsumed`.
//!
//! Only `linear` is swept — `affine` may be dropped silently per the
//! consumption discipline, so affine bindings are never flagged and
//! their move state is left untouched.

use edda_syntax::ast::Linearity;

use super::super::{HirExpr, InferCx, TyEnv};
use super::helpers::path_binding;
use super::BindingState;

/// The linearity modifier on the nominal type behind `ty`, if any.
/// `None` for primitives, tuples, slices, capabilities, function
/// pointers, and freely-copyable nominal types.
fn type_linearity(ty: crate::ty::TyId, ic: &InferCx<'_>) -> Option<Linearity> {
    let crate::ty::TyKind::Nominal(binding_id) = ic.lower.ty_interner.kind(ty) else {
        return None;
    };
    ic.ty_cx.type_decl(*binding_id).and_then(|decl| decl.linearity)
}

/// True when `ty` is a `linear` nominal type (`affine` and copyable
/// types return `false` — only `linear` carries the must-consume
/// obligation this module enforces).
pub(crate) fn is_linear_type(ty: crate::ty::TyId, ic: &InferCx<'_>) -> bool {
    matches!(type_linearity(ty, ic), Some(Linearity::Linear))
}

/// Consume a `linear` binding moved by value out of `expr`.
///
/// When `expr` names a single binding currently [`BindingState::Valid`]
/// whose type is `linear`, transition it to [`BindingState::Consumed`].
/// This is the move that `return e`, a `let`/`=` right-hand side, a
/// `take`-receiver method call, and a block's trailing value perform —
/// none of which carry an explicit `take` keyword, so the §4 call-mode
/// transition does not fire on them. `affine` and copyable bindings are
/// left untouched (affine may be dropped; copyable types are not moved).
pub(crate) fn consume_moved_linear(env: &mut TyEnv, expr: &HirExpr, ic: &InferCx<'_>) {
    let Some(sym) = path_binding(expr) else {
        return;
    };
    let Some(ty) = env.lookup(sym) else {
        return;
    };
    if !is_linear_type(ty, ic) {
        return;
    }
    if env.lookup_state(sym) == Some(BindingState::Valid) {
        env.transition(sym, BindingState::Consumed);
    }
}

/// Sweep the innermost (about-to-be-popped) [`TyEnv`] frame for `linear`
/// bindings that reach the end of their lexical scope still `Valid`.
///
/// Each is reported as `linear_unconsumed` (rendered through the
/// `typecheck_error` class, as the §4 consume diagnostics are) attributed
/// to `span` — the block's source range. A `linear` binding is only
/// `Valid` here when no `take` / move consumed it on the path to the
/// scope's end, which is exactly the leak the consumption discipline
/// forbids. Function parameters are exempt: they are bound in the
/// function-level frame, never an inner block frame, so this sweep never
/// reaches them (matching the native self-host, which marks `take`
/// parameters terminal and skips them).
pub(crate) fn check_block_exit_linear(
    env: &TyEnv,
    span: edda_span::Span,
    ic: &mut InferCx<'_>,
) {
    let leaks: Vec<edda_intern::Symbol> = env
        .iter_top_frame()
        .filter(|(_, state, _)| *state == BindingState::Valid)
        .filter(|(_, _, ty)| is_linear_type(*ty, ic))
        .map(|(sym, _, _)| sym)
        .collect();
    for sym in leaks {
        let name = ic.lower.interner.resolve(sym).to_string();
        ic.emit_typecheck_error(
            span,
            format!(
                "`linear` binding `{name}` is not consumed before its scope ends; \
                 a `linear` value must be passed by `take` (or otherwise consumed) \
                 on every path — only `affine` values may be dropped",
            ),
        );
    }
}
