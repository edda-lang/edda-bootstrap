//! Indirect (fn-pointer) call inference.
//!
//! `synth_indirect_call` handles a callee that evaluates to a
//! fn-pointer value rather than a direct named-function reference.

use edda_span::Span;

use crate::effect::EffectEntry;
use crate::hir::{HirCallArg, HirExpr};
use crate::ty::{TyId, TyKind};

use super::super::{InferCx, TyEnv, synth_expr};
use super::args::{check_arg_expr, synth_args_for_diagnostics};
use super::err_propagation::check_err_propagation;

/// Synthesise an indirect-call expression — the callee is *not* a
/// direct named-function reference but instead evaluates to a
/// fn-pointer value (`h(x)` where `let h = some_fn`, or
/// `(make_fn())(x)`).
///
/// Walks the callee through [`synth_expr`] to materialise its type,
/// then dispatches on the result:
/// - `TyKind::FnPtr(sig)` → arity + per-arg checks against `sig.params`,
///   row union against `sig.effects`, returns `sig.return_ty`.
/// - `TyKind::Error` → cascade silently (the inner failure already
///   pushed a diagnostic).
/// - anything else → "cannot call value of type X" diagnostic.
///
/// Capability substitution at the call site is **not** applied
/// (deferred): fn-ptr-typed callees that carry capability rows would
/// require chasing the operand's capability source through a value
/// chain `synth_call`'s `translate_callee_entry` does not yet handle.
/// Phase A's empty-effect-row probes don't exercise this path.
pub(super) fn synth_indirect_call(
    env: &mut TyEnv,
    callee: &mut HirExpr,
    args: &mut [HirCallArg],
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    // The callee expression is evaluated before the call — an enclosing
    // `?` covers only the indirect call itself, so a fallible call
    // producing the fn-pointer must carry its own `?`
    // (same rule as `check_arg_expr`).
    let saved_try_depth = ic.try_position_depth;
    ic.try_position_depth = 0;
    let callee_ty = synth_expr(env, callee, ic);
    ic.try_position_depth = saved_try_depth;
    if callee_ty == ic.ty_error() {
        synth_args_for_diagnostics(env, args, ic);
        return ic.ty_error();
    }
    // Snapshot the FnPtr signature so we can release the interner borrow
    // before walking args. `FnPtrSig` is `Clone`.
    let fn_ptr_sig = match ic.lower.ty_interner.kind(callee_ty) {
        TyKind::FnPtr(sig) => (**sig).clone(),
        _ => {
            synth_args_for_diagnostics(env, args, ic);
            ic.emit_typecheck_error(
                span,
                format!(
                    "cannot call value of type `{}`",
                    ic.display_ty(callee_ty),
                ),
            );
            return ic.ty_error();
        }
    };

    if args.len() != fn_ptr_sig.params.len() {
        let arity = fn_ptr_sig.params.len();
        let supplied = args.len();
        synth_args_for_diagnostics(env, args, ic);
        ic.emit_typecheck_error(
            span,
            format!(
                "fn-pointer expects {arity} argument(s) but {supplied} were supplied",
            ),
        );
        return ic.ty_error();
    }

    // Positional-only — same rule as direct calls.
    for arg in args.iter() {
        if let Some(name) = &arg.name {
            let field_name = ic.lower.interner.resolve(name.name).to_string();
            ic.emit_typecheck_error(
                arg.span,
                format!(
                    "named argument `{field_name}:` is not admitted at \
                     a fn-pointer call site (positional arguments only)",
                ),
            );
        }
    }

    for (arg, param) in args.iter_mut().zip(fn_ptr_sig.params.iter()) {
        check_arg_expr(env, arg, param.ty, ic);
        super::super::mode::apply_call_mode_transition(env, arg, ic);
    }

    // Effect-row union. Capability entries pass through unchanged
    // (call-site substitution for fn-ptr callees is deferred — see the
    // doc-comment above); pure entries pass through unconditionally.
    let translated_entries: Vec<EffectEntry> = fn_ptr_sig.effects.entries().to_vec();
    for entry in &translated_entries {
        ic.row.push(*entry);
    }
    // B22 err-propagation check at the indirect-call site. fn-ptr
    // callees are anonymous from the row's perspective — render the
    // diagnostic against a synthetic `<fn-pointer>` name so the message
    // still points the user at the missing `?`/`handle`.
    let synthetic = ic.lower.interner.intern("<fn-pointer>");
    check_err_propagation(&translated_entries, synthetic, span, ic);

    fn_ptr_sig.return_ty
}
