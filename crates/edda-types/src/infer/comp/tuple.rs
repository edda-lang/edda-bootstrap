//! Tuple construction and positional-field-access inference;
//! sum-receiver variant-payload access added with the D-22 READ seam.

use edda_span::Span;

use crate::cx::{TypeDeclShape, VariantPayloadInfo};
use crate::hir::HirExpr;
use crate::prim::Primitive;
use crate::ty::{TyId, TyKind};

use super::super::{InferCx, TyEnv, synth_expr};

/// Synthesise a tuple expression. Elements synth left-to-right; the
/// resulting type is the structural tuple of element types.
pub(in crate::infer) fn synth_tuple(
    env: &mut TyEnv,
    elems: &mut [HirExpr],
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    if elems.len() < 2 {
        ic.emit_typecheck_error(span, "tuple expression requires at least 2 elements");
        return ic.ty_error();
    }
    let element_tys: Vec<TyId> = elems
        .iter_mut()
        .map(|e| synth_expr(env, e, ic))
        .collect();
    ic.lower.ty_interner.tuple(element_tys)
}

/// Synthesise a `receiver.N` positional-field access. The receiver
/// must type-synthesise to `TyKind::Tuple(_)` (element `N`) or to a
/// sum-typed nominal (variant-`N` payload — the D-22 READ surface).
/// The variant payload
/// reads as its *composite* type under the native `field_type_at`
/// convention: `()` for a unit variant, the one-element tuple `(T)` for
/// a single payload, and the positional tuple `(T1, …, Tn)` for a
/// multi-element / struct payload. Out-of-range indices and other
/// receivers emit a diagnostic and yield the error sentinel.
pub(in crate::infer) fn synth_tuple_index(
    env: &mut TyEnv,
    receiver: &mut HirExpr,
    index: u32,
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    let recv_ty = synth_expr(env, receiver, ic);
    let error = ic.ty_error();
    if recv_ty == error {
        return error;
    }
    match ic.lower.ty_interner.kind(recv_ty) {
        TyKind::Tuple(elements) => {
            let idx = index as usize;
            if idx < elements.len() {
                return elements[idx];
            }
            let display = ic.display_ty(recv_ty).to_string();
            let len = elements.len();
            ic.emit_typecheck_error(
                span,
                format!(
                    "tuple index `.{index}` is out of bounds for `{display}` (length {len})",
                ),
            );
            error
        }
        TyKind::Nominal(binding_id) => {
            match sum_variant_payload_ty(ic, *binding_id, index as usize) {
                SumPayloadLookup::Payload(t) => t,
                SumPayloadLookup::NotASum => {
                    let display = ic.display_ty(recv_ty).to_string();
                    ic.emit_typecheck_error(
                        span,
                        format!(
                            "cannot apply positional index `.{index}` to `{display}` — only tuples and sum-typed values support positional access",
                        ),
                    );
                    error
                }
                SumPayloadLookup::VariantOutOfRange(count) => {
                    let display = ic.display_ty(recv_ty).to_string();
                    ic.emit_typecheck_error(
                        span,
                        format!(
                            "variant index `.{index}` is out of bounds for `{display}` ({count} variants)",
                        ),
                    );
                    error
                }
            }
        }
        _ => {
            let display = ic.display_ty(recv_ty).to_string();
            ic.emit_typecheck_error(
                span,
                format!(
                    "cannot apply tuple index `.{index}` to `{display}` — only tuple types support positional field access",
                ),
            );
            error
        }
    }
}

/// Outcome of the sum-receiver variant-payload lookup.
enum SumPayloadLookup {
    /// The variant's payload composite type (`()`, a one-element tuple,
    /// or a positional tuple).
    Payload(TyId),
    /// The nominal is not a sum declaration (or has no decl info).
    NotASum,
    /// Variant index past the declared variant count (carries the count).
    VariantOutOfRange(usize),
}

/// Look up the variant-`index` payload *composite* type on a sum-typed
/// nominal — the typing half of the `v.(i)` variant-payload READ (D-22).
/// Mirrors the native
/// `field_type_at(T, i)` convention: a payload-less variant reads as the
/// unit type `()`; a payload-bearing variant reads as the positional
/// payload tuple — the one-element tuple `(T)` for a single payload
/// (`case data(u32)` → `(u32)`, distinct from bare `u32`), the tuple
/// `(T1, …, Tn)` for a multi-element tuple payload or a struct payload
/// (fields in declaration order). Every declared variant therefore
/// yields a payload type; the old single-payload-only rule is retired.
fn sum_variant_payload_ty(
    ic: &InferCx<'_>,
    binding_id: edda_resolve::BindingId,
    index: usize,
) -> SumPayloadLookup {
    let Some(info) = ic.ty_cx.type_decl(binding_id) else {
        return SumPayloadLookup::NotASum;
    };
    let TypeDeclShape::Sum { variants } = &info.kind else {
        return SumPayloadLookup::NotASum;
    };
    let Some(variant) = variants.get(index) else {
        return SumPayloadLookup::VariantOutOfRange(variants.len());
    };
    let composite = match &variant.payload {
        VariantPayloadInfo::Unit => ic.lower.ty_interner.prim(Primitive::Unit),
        VariantPayloadInfo::Tuple { elems } => payload_composite(ic, elems.iter().copied()),
        VariantPayloadInfo::Struct { fields } => {
            payload_composite(ic, fields.iter().map(|f| f.ty))
        }
    };
    SumPayloadLookup::Payload(composite)
}

/// Intern the positional payload composite for a payload-bearing
/// variant: the one-element tuple `(T)` for a single payload (the D-22
/// convention — a real tuple, distinct from the bare `T`),
/// the ordinary tuple `(T1, …, Tn)` otherwise. A payload-less variant
/// never reaches here (it reads as `()`); the empty guard is defensive.
fn payload_composite(ic: &InferCx<'_>, elems: impl IntoIterator<Item = TyId>) -> TyId {
    let tys: Vec<TyId> = elems.into_iter().collect();
    if tys.is_empty() {
        return ic.lower.ty_interner.prim(Primitive::Unit);
    }
    ic.lower.ty_interner.tuple(tys)
}
