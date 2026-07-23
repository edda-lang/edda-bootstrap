//! Call-argument typing helpers, including the `init`-mode read-bypass.
//!
//! `check_arg_expr` / `synth_arg_expr` type a call argument's inner
//! expression with awareness of the argument's mode keyword;
//! `try_init_bypass` skips the read-side state gate for `init`-mode
//! args so a freshly-uninit binding/field is admissible at the use
//! site.

use crate::cx::TypeDeclShape;
use crate::hir::{HirCallArg, HirCallMode, HirExprKind};
use crate::ty::{TyId, TyKind};

use super::super::path::check_synth_against_expected;
use super::super::{InferCx, TyEnv, check_expr, synth_expr};

/// Synthesise every argument for its side-effect (sub-expression
/// type-error surfacing) without imposing an expected type — used by
/// the error paths above so a malformed call still emits diagnostics
/// for malformed arguments.
pub(super) fn synth_args_for_diagnostics(env: &mut TyEnv, args: &mut [HirCallArg], ic: &mut InferCx<'_>) {
    for arg in args.iter_mut() {
        synth_arg_expr(env, arg, ic);
    }
}

/// Type-check a call argument's inner expression against `expected`,
/// with awareness of the argument's mode keyword.
///
/// The `?`-propagator flag is zeroed for the duration of the argument
/// walk: `?` binds only to the call it
/// is syntactically attached to, so a fallible call in argument
/// position must carry its own `?` (or sit under an enclosing
/// `handle`). MIR's `lower_try` wires the on-error successor for
/// exactly one call — admitting a nested unwrapped call here left its
/// err-edge unwired and miscompiled (crash / runaway-alloc OOM).
///
/// For [`HirCallMode::Init`] args whose inner expression is a
/// single-segment `Path` (`init slots`) or a single-segment-rooted
/// `Field` projection (`init obj.field`), the read-side state gates
/// in [`super::super::path::synth_path`] / [`super::super::field::synth_field`] are
/// bypassed: `init` mode REQUIRES the binding/field to be `Uninit`
/// (or `Consumed`) at the call site, so applying the read-gate would
/// inverting the diagnostic. The mode pre-state check
/// ([`super::super::mode::apply_call_mode_transition`]) runs *after* this
/// helper and validates the actual pre-state against the mode's
/// requirements.
///
/// For every other shape — `mutable` / `take` modes, no mode, or
/// `init` with a non-bypass-eligible inner expression — this routes
/// through the standard [`super::super::check_expr`].
pub(crate) fn check_arg_expr(
    env: &mut TyEnv,
    arg: &mut HirCallArg,
    expected: TyId,
    ic: &mut InferCx<'_>,
) {
    let saved_try_depth = ic.try_position_depth;
    ic.try_position_depth = 0;
    if !try_init_bypass(env, arg, Some(expected), ic) {
        check_expr(env, &mut arg.expr, expected, ic);
    }
    ic.try_position_depth = saved_try_depth;
}

/// Synthesise a call argument's inner expression without an expected
/// type, applying the same `init`-mode read-bypass as
/// [`check_arg_expr`]. Used by diagnostic-cascade paths and by
/// indirect-call sites that resolved their callee type before the
/// arg loop.
///
/// Zeroes the `?`-propagator flag for the argument walk, same as
/// [`check_arg_expr`].
pub(crate) fn synth_arg_expr(
    env: &mut TyEnv,
    arg: &mut HirCallArg,
    ic: &mut InferCx<'_>,
) {
    let saved_try_depth = ic.try_position_depth;
    ic.try_position_depth = 0;
    if !try_init_bypass(env, arg, None, ic) {
        let _ = synth_expr(env, &mut arg.expr, ic);
    }
    ic.try_position_depth = saved_try_depth;
}

/// Eligibility test + execution for the `init`-mode read-bypass.
///
/// Eligibility: `arg.mode == Some(Init)` AND the inner expression is
/// either a single-segment `Path` (whole-binding init: `init x`) or a
/// `Field` projection whose receiver is a single-segment path
/// (per-field init: `init x.f`).
///
/// When eligible, types the inner expression directly from the env /
/// `TyCx` without routing through `synth_path` / `synth_field`. This
/// keeps the read-side state gate from firing — which is the whole
/// point: `init` mode requires the binding (or the projected field)
/// to be `Uninit` / `Consumed` at the call site, and the read-gate
/// would invert that as an error.
fn try_init_bypass(
    env: &mut TyEnv,
    arg: &mut HirCallArg,
    expected: Option<TyId>,
    ic: &mut InferCx<'_>,
) -> bool {
    if arg.mode != Some(HirCallMode::Init) {
        return false;
    }
    let span = arg.expr.span;
    // Whole-binding init: `init x`.
    if let HirExprKind::Path(p) = &arg.expr.kind
        && p.segments.len() == 1
        && let Some(ty) = env.lookup(p.segments[0].name)
    {
        arg.expr.ty = ty;
        if let Some(expected) = expected {
            let result = check_synth_against_expected(ty, expected, ic, span);
            arg.expr.ty = result;
        }
        return true;
    }
    // Field-projection init: `init x.f`. The receiver must be a
    // single-segment path so we can look the binding's type up in
    // `env` and pull the field's declared type from `TyCx` without
    // walking `synth_field` (whose §5 per-field gate would fire on a
    // freshly-uninit field).
    if let HirExprKind::Field { receiver, name } = &mut arg.expr.kind
        && let HirExprKind::Path(p) = &receiver.kind
        && p.segments.len() == 1
        && let Some(recv_ty) = env.lookup(p.segments[0].name)
    {
        // Stamp the receiver's `ty` directly from the env — routing
        // through `synth_expr` would re-trigger the whole-binding
        // read-gate on an `Uninit` / `PartialInit` binding.
        receiver.ty = recv_ty;
        let field_name = name.name;
        let field_ty = lookup_field_ty(recv_ty, field_name, ic).unwrap_or_else(|| ic.ty_error());
        arg.expr.ty = field_ty;
        if let Some(expected) = expected {
            let result = check_synth_against_expected(field_ty, expected, ic, span);
            arg.expr.ty = result;
        }
        return true;
    }
    false
}

/// Look up a field's declared `TyId` on a nominal product type.
/// Returns `None` when `recv_ty` is not a `Nominal` of a product type
/// or the field is not declared.
fn lookup_field_ty(
    recv_ty: TyId,
    field: edda_intern::Symbol,
    ic: &InferCx<'_>,
) -> Option<TyId> {
    let TyKind::Nominal(binding_id) = ic.lower.ty_interner.kind(recv_ty) else {
        return None;
    };
    let info = ic.ty_cx.type_decl(*binding_id)?;
    match &info.kind {
        TypeDeclShape::Product { fields } => fields
            .iter()
            .find(|f| f.name == field)
            .map(|f| f.ty),
        TypeDeclShape::Sum { .. } => None,
    }
}
