//! Array / slice literal inference.

use edda_span::Span;

use crate::hir::HirExpr;
use crate::ty::TyId;

use super::super::{InferCx, TyEnv, check_expr, synth_expr};

/// Synthesise an array / slice literal `[e1, ..., en]`. The first
/// element synthesises the element type `T`; each remaining element
/// checks against `T`. The result type is `[T]`. The empty form `[]`
/// has no element to infer from, so synth-mode emits a diagnostic and
/// yields the error sentinel — the check-mode path (`check_array`)
/// supplies the element type from context for `[]`.
pub(in crate::infer) fn synth_array(
    env: &mut TyEnv,
    elems: &mut [HirExpr],
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    let Some((first, rest)) = elems.split_first_mut() else {
        ic.emit_typecheck_error(
            span,
            "cannot infer the element type of an empty array literal `[]`; add a type annotation (e.g. `let xs: [T] = []`)",
        );
        return ic.ty_error();
    };
    let elem_ty = synth_expr(env, first, ic);
    let error = ic.ty_error();
    for e in rest {
        check_expr(env, e, elem_ty, ic);
    }
    if elem_ty == error {
        return error;
    }
    ic.lower.ty_interner.slice(elem_ty)
}
