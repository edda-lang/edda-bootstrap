//! Call-site typing for the locked primitive-headed static
//! methods catalogue (currently empty — see
//! [`crate::PrimitiveStaticMethod`]). The resolver returns
//! `Resolved::Error` for catalogue-headed paths (`String`, `i32`, …)
//! and stays silent, so [`super::call::synth_call`] routes here before
//! the package-level resolver lookup. Each catalogue entry has a fixed
//! arity and locked parameter shape; the typechecker resolves the
//! signature directly without a `BindingId` and records the dispatch
//! on [`crate::infer::cx::InferCx::primitive_static_calls`] so MIR
//! lowering can emit a `Call` against the variant's `__edda_*` runtime
//! extern.

use edda_span::Span;

use crate::PrimitiveStaticMethod;
use crate::hir::{HirCallArg, HirPath};
use crate::ty::TyId;

use super::call::{check_arg_expr, synth_arg_expr};
use super::{InferCx, TyEnv};

/// Type-check a `<Primitive>.<method>(args)` call where the
/// `(head, leaf)` pair matched
/// [`crate::resolve_primitive_static_method`].
pub(super) fn synth_primitive_static_method_call(
    env: &mut TyEnv,
    path: &HirPath,
    method: PrimitiveStaticMethod,
    args: &mut [HirCallArg],
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    let (param_tys, return_ty) = signature_for(method, ic);
    let arity = param_tys.len();

    // Reject named-payload arguments — primitive static methods are
    // positional (`expressions.md` §521).
    for arg in args.iter() {
        if let Some(name) = &arg.name {
            let field = ic.lower.interner.resolve(name.name).to_string();
            let display = display_method_name(method, ic);
            ic.emit_typecheck_error(
                arg.span,
                format!(
                    "named argument `{field}:` is not admitted at primitive \
                     static-method call site `{display}`"
                ),
            );
        }
    }

    if args.len() != arity {
        for arg in args.iter_mut() {
            synth_arg_expr(env, arg, ic);
        }
        let display = display_method_name(method, ic);
        ic.emit_typecheck_error(
            span,
            format!(
                "primitive static method `{display}` takes {arity} argument(s) \
                 but {found} were supplied",
                found = args.len()
            ),
        );
        return ic.ty_error();
    }

    for (arg, param_ty) in args.iter_mut().zip(param_tys.iter()) {
        check_arg_expr(env, arg, *param_ty, ic);
        super::mode::apply_call_mode_transition(env, arg, ic);
    }

    ic.primitive_static_calls.insert(span, method);
    // Path span carries no resolver-side `Resolved` entry; recording the
    // dispatch keyed by call span (above) is the canonical handoff.
    let _ = path.span;
    return_ty
}

/// Synthesised signature of a primitive static method: the parameter
/// types (positional) and the return type. These methods carry no effect
/// row on these methods — the runtime stubs do not raise.
fn signature_for(method: PrimitiveStaticMethod, _ic: &mut InferCx<'_>) -> (Vec<TyId>, TyId) {
    match method {}
}

/// Source-level display form for diagnostics.
fn display_method_name(method: PrimitiveStaticMethod, _ic: &InferCx<'_>) -> &'static str {
    match method {}
}
