//! Closure-literal inference (PR-B2).
//!
//! Synthesises the [`crate::TyKind::FnPtr`] type of a closure literal and
//! type-checks its body re-entrantly through [`check_fn_body`] under a
//! fresh environment holding only the closure's parameters and captured
//! bindings — the mandatory-capture discipline from CLAUDE.md
//! §"Function types and closures".

use super::{InferCx, TyEnv, check_fn_body};
use crate::hir::HirClosure;
use crate::sig::{FnPtrParam, FnPtrSig, ParamMode};
use crate::ty::TyId;

/// Synthesise a closure literal's [`crate::TyKind::FnPtr`] type and
/// type-check its body. Returns the closure's value type.
pub(super) fn synth_closure(
    env: &mut TyEnv,
    closure: &mut HirClosure,
    ic: &mut InferCx<'_>,
) -> TyId {
    // The closure's value type — same shape a `function(...)` type
    // annotation lowers to.
    let params: Vec<FnPtrParam> = closure
        .params
        .iter()
        .map(|p| FnPtrParam {
            mode: p.mode,
            ty: p.ty,
        })
        .collect();
    let fn_ty = ic.lower.ty_interner.fn_ptr(FnPtrSig {
        params: params.into_boxed_slice(),
        return_ty: closure.ret_ty,
        effects: closure.effects.clone(),
    });

    // Capture types come from the enclosing environment — the capture
    // names resolve to outer bindings. Read them before the fresh body
    // env shadows the enclosing frame.
    let error = ic.ty_error();
    let capture_tys: Vec<(edda_intern::Symbol, TyId)> = closure
        .captures
        .iter()
        .map(|cap| (cap.name.name, env.lookup(cap.name.name).unwrap_or(error)))
        .collect();

    // Fresh env: only the parameters and captures are visible inside the
    // body (mandatory-capture discipline). Top-level functions / consts
    // still resolve via the resolution map, not the env.
    let mut body_env = TyEnv::new();
    for p in closure.params.iter() {
        body_env.bind(p.name.name, p.ty);
    }
    for (name, ty) in capture_tys {
        body_env.bind(name, ty);
    }
    let take_params: Vec<edda_intern::Symbol> = closure
        .params
        .iter()
        .filter(|p| p.mode == ParamMode::Take)
        .map(|p| p.name.name)
        .collect();

    // The function-body machinery is re-entrant (see `check_fn_body`) —
    // it swaps the declared row / accumulator / return type and restores
    // them, so a closure body nested inside an enclosing function body
    // does not disturb the outer typing context.
    check_fn_body(
        &mut body_env,
        &mut closure.body,
        ic,
        &closure.effects,
        closure.ret_ty,
        closure.span,
        false,
        &take_params,
    );

    fn_ty
}
