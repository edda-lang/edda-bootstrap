//! §4 / §5 call-argument mode transitions + the loop re-entry check.
//!
//! These functions operate on the [`BindingState`] lattice: per-mode
//! pre-state requirements and post-states for `mutable` / `take` /
//! `init` call args (binding-level and field-level), and the §4 loop
//! body re-entry diagnostic.

use std::collections::BTreeSet;

use edda_diag::DiagnosticClass;
use edda_intern::Symbol;

use super::helpers::{field_projection_binding, path_binding, type_field_set};
use super::lattice::BindingState;

/// Reject a `mutable` / `init` borrow whose root `binding` is immutable
/// (a `let` local or a `Default`-mode parameter). Such a borrow lowers
/// to a byval copy in the backend, so the callee's write lands on the
/// copy and is silently lost. `take` is
/// a move, not a write-through, so it is exempt. Returns `true` when a
/// diagnostic was emitted so the caller can skip the state transition.
fn reject_immutable_borrow(
    env: &super::super::TyEnv,
    ic: &mut super::super::InferCx<'_>,
    span: edda_span::Span,
    mode: crate::hir::HirCallMode,
    binding: Symbol,
    field: Option<Symbol>,
) -> bool {
    if !matches!(
        mode,
        crate::hir::HirCallMode::Mutable | crate::hir::HirCallMode::Init
    ) {
        return false;
    }
    if env.lookup_mutable(binding) != Some(false) {
        return false;
    }
    let kw = mode.keyword();
    let bname = ic.lower.interner.resolve(binding).to_string();
    let place = match field {
        Some(f) => format!("{bname}.{}", ic.lower.interner.resolve(f)),
        None => bname.clone(),
    };
    ic.emit_diagnostic(
        DiagnosticClass::ModeViolation,
        span,
        format!(
            "`{kw} {place}` requires `{bname}` to be a mutable binding, but it is immutable; \
             declare it `var` (a local) or take it as a `mutable` / `init` parameter — a \
             `{kw}` borrow of an immutable binding is lowered as a copy, so the write would be lost"
        ),
    );
    true
}

/// Apply the §4 state transitions for a single call argument. The
/// argument's mode keyword determines pre-state requirements and the
/// post-state:
///
/// | Mode    | Pre        | Post      |
/// |---------|------------|-----------|
/// | `mutable` | `Valid`    | `Valid`   |
/// | `take`  | `Valid`    | `Consumed`|
/// | `init`   | `Uninit`   | `Valid`   |
/// | (none)  | `Valid`    | unchanged |
///
/// A pre-state mismatch emits a diagnostic and leaves the binding's
/// state unchanged so callers see one diagnostic per misuse rather
/// than cascading errors. The transition only fires when the argument
/// is a single-segment path naming a binding — `f(take obj.field)`
/// and `f(take xs[0])` are §5 / §8's territory and silently skip the
/// transition.
pub(crate) fn apply_call_mode_transition(
    env: &mut super::super::TyEnv,
    arg: &crate::hir::HirCallArg,
    ic: &mut super::super::InferCx<'_>,
) {
    let Some(mode) = arg.mode else {
        return;
    };
    apply_mode_transition(env, mode, &arg.expr, arg.span, ic);
}

/// Apply the §4 / §5 mode transition for an explicit `mode`-prefixed
/// place expression `expr` at `span`. Shared by call arguments
/// ([`apply_call_mode_transition`]) and struct-literal field
/// initialisers ([`apply_struct_field_mode_transition`]).
/// Also reused by spawn-arg
/// initialisers, which carry a `take`
/// mode implicitly (the AST has no separate mode tag — every spawn
/// arg is `take` at the source level). A pre-state mismatch emits one
/// diagnostic and leaves the binding state unchanged.
pub(crate) fn apply_mode_transition(
    env: &mut super::super::TyEnv,
    mode: crate::hir::HirCallMode,
    expr: &crate::hir::HirExpr,
    span: edda_span::Span,
    ic: &mut super::super::InferCx<'_>,
) {
    // Field projections take the §5 per-field path; whole-binding
    // references take the §4 per-binding path.
    if let Some((binding, field)) = field_projection_binding(expr) {
        apply_field_mode_transition(env, span, binding, field, mode, ic);
        return;
    }
    let Some(sym) = path_binding(expr) else {
        return;
    };
    // A `mutable`/`init` borrow of an immutable binding (a `let` local /
    // `Default`-mode param) is unsound: the backend passes the binding
    // byval, so the callee mutates a throwaway copy and the write is
    // silently lost. Reject it.
    if reject_immutable_borrow(env, ic, span, mode, sym, None) {
        return;
    }
    let Some(pre) = env.lookup_state(sym) else {
        return;
    };
    let (required, post) = match mode {
        crate::hir::HirCallMode::Mutable => (BindingState::Valid, BindingState::Valid),
        crate::hir::HirCallMode::Take => (BindingState::Valid, BindingState::Consumed),
        crate::hir::HirCallMode::Init => (BindingState::Uninit, BindingState::Valid),
    };
    if pre != required {
        let name = ic.lower.interner.resolve(sym).to_string();
        let kw = mode.keyword();
        ic.emit_typecheck_error(
            span,
            format!(
                "`{kw} {name}` requires the binding to be {} here, but it is {}",
                required.describe(),
                pre.describe(),
            ),
        );
        return;
    }
    env.transition(sym, post);
}

/// Apply the mode transition for a struct-literal field initialiser
/// (`Point { x: take p }`).
pub(crate) fn apply_struct_field_mode_transition(
    env: &mut super::super::TyEnv,
    field: &crate::hir::HirStructLitField,
    ic: &mut super::super::InferCx<'_>,
) {
    let Some(mode) = field.mode else {
        return;
    };
    apply_mode_transition(env, mode, &field.value, field.span, ic);
}

/// Apply the §5 state transitions for a field-projection call argument
/// (`f(mutable|take|set x.field, ...)`).
///
/// Operates on the per-field state derived from the binding's current
/// `BindingState`. Pre-state requirements per mode:
///
/// | Mode    | Pre `field_state` | Post `field_state` |
/// |---------|--------------------|---------------------|
/// | `mutable` | `Valid`            | `Valid`             |
/// | `take`  | `Valid`            | (consumed → not-in-F)|
/// | `init`   | `Uninit`/`Consumed`| `Valid`             |
///
/// The binding's post-state is summarised: if the resulting valid-set
/// equals the full field set, the binding promotes to `Valid`; if
/// empty, falls back to `Uninit`; otherwise stays `PartialInit`.
fn apply_field_mode_transition(
    env: &mut super::super::TyEnv,
    span: edda_span::Span,
    binding: Symbol,
    field: Symbol,
    mode: crate::hir::HirCallMode,
    ic: &mut super::super::InferCx<'_>,
) {
    let Some(binding_state) = env.lookup_state(binding) else {
        return;
    };
    // `mutable`/`init` of a field whose root binding is immutable is the
    // §300 hole: the byval copy makes the field write vanish. Reject
    // before any per-field state transition.
    if reject_immutable_borrow(env, ic, span, mode, binding, Some(field)) {
        return;
    }
    let Some(binding_ty) = env.lookup(binding) else {
        return;
    };
    let Some(full_set) = type_field_set(binding_ty, ic) else {
        // Non-product type — field projection on this is already a
        // structural error caught elsewhere. Skip transitions.
        return;
    };
    if !full_set.contains(&field) {
        // Field not declared on the type — `synth_field` already
        // reports this. Skip.
        return;
    }
    let pre_field = binding_state.field_state(field);
    let (required_label, post_valid) = match mode {
        crate::hir::HirCallMode::Mutable => ("valid", true),
        crate::hir::HirCallMode::Take => ("valid", false),
        crate::hir::HirCallMode::Init => {
            // Either Uninit or Consumed is admissible for `init`.
            let ok = matches!(pre_field, BindingState::Uninit | BindingState::Consumed);
            if !ok {
                let bname = ic.lower.interner.resolve(binding).to_string();
                let fname = ic.lower.interner.resolve(field).to_string();
                ic.emit_typecheck_error(
                    span,
                    format!(
                        "`set {bname}.{fname}` requires the field to be \
                         uninitialised or consumed here, but it is {}",
                        pre_field.describe(),
                    ),
                );
                return;
            }
            // Set: transition this field to Valid.
            apply_field_state_transition(env, binding, field, true, &full_set);
            return;
        }
    };
    if !matches!(pre_field, BindingState::Valid) {
        let bname = ic.lower.interner.resolve(binding).to_string();
        let fname = ic.lower.interner.resolve(field).to_string();
        let kw = mode.keyword();
        ic.emit_typecheck_error(
            span,
            format!(
                "`{kw} {bname}.{fname}` requires the field to be {required_label} here, \
                 but it is {}",
                pre_field.describe(),
            ),
        );
        return;
    }
    apply_field_state_transition(env, binding, field, post_valid, &full_set);
}

/// Transition a single field's state to either Valid (`valid = true`)
/// or "not valid" (`valid = false`) and recompute the binding's
/// summarised [`BindingState`].
fn apply_field_state_transition(
    env: &mut super::super::TyEnv,
    binding: Symbol,
    field: Symbol,
    valid: bool,
    full_set: &BTreeSet<Symbol>,
) {
    let current = match env.lookup_state(binding) {
        Some(s) => s,
        None => return,
    };
    let mut valid_set: BTreeSet<Symbol> = match &current {
        BindingState::Valid => full_set.clone(),
        BindingState::Uninit | BindingState::Consumed => BTreeSet::new(),
        BindingState::PartialInit(f) => f.clone(),
    };
    if valid {
        valid_set.insert(field);
    } else {
        valid_set.remove(&field);
    }
    let new_state = if valid_set == *full_set {
        BindingState::Valid
    } else if valid_set.is_empty() {
        BindingState::Uninit
    } else {
        BindingState::PartialInit(valid_set)
    };
    env.transition(binding, new_state);
}

/// Apply M-Field-Assign for `x.field = e`. After the assignment the
/// field is Valid; if the resulting valid-set equals the full field
/// set, the binding promotes to `Valid` per the §5 promotion rule.
pub(crate) fn apply_field_assign_transition(
    env: &mut super::super::TyEnv,
    binding: Symbol,
    field: Symbol,
    ic: &mut super::super::InferCx<'_>,
) -> bool {
    let Some(binding_ty) = env.lookup(binding) else {
        return false;
    };
    let Some(full_set) = type_field_set(binding_ty, ic) else {
        return false;
    };
    if !full_set.contains(&field) {
        return false;
    }
    apply_field_state_transition(env, binding, field, true, &full_set);
    true
}

/// Diagnose any binding whose post-loop state differs from its
/// pre-loop state. Per `inference-rules.md §4`, *`loop` and `for`
/// body re-entry checks*, the body must not change the state of
/// outer-scope bindings between iterations — otherwise the second
/// iteration observes a different state than the first.
pub(crate) fn check_loop_reentry(
    post: &super::super::TyEnv,
    pre: &super::super::TyEnv,
    ic: &mut super::super::InferCx<'_>,
    span: edda_span::Span,
) {
    use std::collections::HashSet;
    let mut reported: HashSet<edda_intern::Symbol> = HashSet::new();
    for (sym, post_state) in post.iter_states() {
        if reported.contains(&sym) {
            continue;
        }
        let Some(pre_state) = pre.lookup_state(sym) else {
            continue;
        };
        if pre_state != post_state {
            let name = ic.lower.interner.resolve(sym).to_string();
            ic.emit_typecheck_error(
                span,
                format!(
                    "loop body changes the state of outer binding `{name}` \
                     (was {}, now {}); state must agree across iterations",
                    pre_state.describe(),
                    post_state.describe(),
                ),
            );
            reported.insert(sym);
        }
    }
}
