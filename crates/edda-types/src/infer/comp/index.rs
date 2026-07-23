//! Index- and range-expression inference.
//!
//! - **Index** (`e[i]`) — `e` must be a slice `[T]`, `i` must check
//!   against `usize`; result is `T`. The refinement side-condition
//!   `i < e.len()` from `T-IndexAccess` is the refinement layer's
//!   business and isn't enforced here.
//! - **Range** (`lo..<hi` / `lo..=hi`) — element-type inference per
//!   `inference-rules.md §3`, registering an implicit `Range` spec
//!   request.

use edda_span::Span;

use crate::hir::{HirExpr, HirExprKind};
use crate::implicit_spec::ImplicitSpec;
use crate::prim::Primitive;
use crate::ty::{TyId, TyKind};

use super::super::{InferCx, TyEnv, check_expr, synth_expr};

/// Synthesise an index expression `e[i]`. Requires `e: [T]` and
/// `i ⇐ usize`; result is `T`.
pub(in crate::infer) fn synth_index(
    env: &mut TyEnv,
    receiver: &mut HirExpr,
    index: &mut HirExpr,
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    let recv_ty = synth_expr(env, receiver, ic);
    let error = ic.ty_error();
    let usize_ty = ic.lower.ty_interner.prim(Primitive::Usize);

    // Slice subrange: `b[lo..hi]`, `b[..hi]`, `b[lo..]`, `b[..]` —
    // each endpoint, when present, must be `usize`. The result type
    // is the same slice type as the receiver.
    if let HirExprKind::Range { lo, hi, kind: _ } = &mut index.kind {
        if let Some(lo) = lo.as_deref_mut() {
            check_expr(env, lo, usize_ty, ic);
        }
        if let Some(hi) = hi.as_deref_mut() {
            check_expr(env, hi, usize_ty, ic);
        }
        index.ty = error;
        if recv_ty == error {
            return error;
        }
        if matches!(ic.lower.ty_interner.kind(recv_ty), TyKind::Slice(_)) {
            return recv_ty;
        }
        let display = ic.display_ty(recv_ty).to_string();
        ic.emit_typecheck_error(
            span,
            format!(
                "cannot take a sub-range of `{display}` — only slice types `[T]` support range indexing",
            ),
        );
        return error;
    }

    check_expr(env, index, usize_ty, ic);
    if recv_ty == error {
        return error;
    }
    if let TyKind::Slice(elem) = ic.lower.ty_interner.kind(recv_ty) {
        return *elem;
    }
    let display = ic.display_ty(recv_ty).to_string();
    ic.emit_typecheck_error(
        span,
        format!("cannot index into `{display}` — only slice types `[T]` support indexing"),
    );
    error
}

/// Synthesise a range expression `lo..<hi` / `lo..=hi`.
///
/// Per `inference-rules.md §3`:
///
/// - When both endpoints are bare integer literals, the element type
///   defaults to `i64` (the integer-literal default).
/// - When one endpoint is a typed expression and the other is a bare
///   integer literal, the literal is *checked* against the typed
///   endpoint's type and the element type is that typed-endpoint
///   type.
/// - When both endpoints are typed expressions with the same type,
///   the element type is that type.
/// - Mismatched non-literal types emit a typecheck diagnostic and
///   yield `Error`.
///
/// The expression's value type is the error sentinel for now — the
/// generated `Range_<T>` nominal lands when `edda-codegen`'s spec-
/// instantiation pass consumes the recorded
/// [`crate::ImplicitSpecRequest`]. Surface use is currently restricted
/// to `for i in <range>` (whose `yield: T` consumption needs the
/// element type, recorded via the request) and to pattern-style use
/// that doesn't yet exist; both flow through later waves.
pub(in crate::infer) fn synth_range(
    env: &mut TyEnv,
    lo: &mut Option<Box<HirExpr>>,
    hi: &mut Option<Box<HirExpr>>,
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    // Open-endpoint forms (`..`, `..hi`, `lo..`) carry no usable element
    // type by themselves; they only acquire meaning in a slice-index
    // context where `synth_index` re-routes through the receiver's
    // element type. Walk any present endpoint so its `ty` field is
    // populated, then return the Error sentinel.
    let lo_present = lo.is_some();
    let hi_present = hi.is_some();
    if !lo_present || !hi_present {
        if let Some(lo) = lo.as_deref_mut() {
            let _ = synth_expr(env, lo, ic);
        }
        if let Some(hi) = hi.as_deref_mut() {
            let _ = synth_expr(env, hi, ic);
        }
        return ic.ty_error();
    }
    let lo_box = lo.as_mut().unwrap();
    let hi_box = hi.as_mut().unwrap();
    let lo_is_int_lit = matches!(
        lo_box.kind,
        HirExprKind::Literal(edda_syntax::ast::Literal::Int { .. })
    );
    let hi_is_int_lit = matches!(
        hi_box.kind,
        HirExprKind::Literal(edda_syntax::ast::Literal::Int { .. })
    );

    let element_ty = if lo_is_int_lit && hi_is_int_lit {
        // Both bare literals: synth both at i64, register an i64 spec.
        let _ = synth_expr(env, lo_box, ic);
        let _ = synth_expr(env, hi_box, ic);
        ic.lower.ty_interner.prim(Primitive::I64)
    } else if hi_is_int_lit {
        // lo is typed; check hi against lo's type.
        let lo_ty = synth_expr(env, lo_box, ic);
        check_expr(env, hi_box, lo_ty, ic);
        lo_ty
    } else if lo_is_int_lit {
        // hi is typed; check lo against hi's type.
        let hi_ty = synth_expr(env, hi_box, ic);
        check_expr(env, lo_box, hi_ty, ic);
        hi_ty
    } else {
        // Both typed — must agree.
        let lo_ty = synth_expr(env, lo_box, ic);
        let hi_ty = synth_expr(env, hi_box, ic);
        let error = ic.ty_error();
        if lo_ty == error || hi_ty == error {
            return error;
        }
        if lo_ty != hi_ty {
            let lo_display = ic.display_ty(lo_ty).to_string();
            let hi_display = ic.display_ty(hi_ty).to_string();
            ic.emit_typecheck_error(
                span,
                format!(
                    "range endpoints have incompatible types: `{lo_display}` and `{hi_display}`",
                ),
            );
            return error;
        }
        lo_ty
    };

    if element_ty == ic.ty_error() {
        return element_ty;
    }
    // Only integer / float endpoint types are admitted for ranges —
    // diagnose other primitives so users see the constraint
    // at the range site rather than at downstream codegen.
    if let TyKind::Primitive(p) = ic.lower.ty_interner.kind(element_ty)
        && !p.is_numeric()
    {
        let display = ic.display_ty(element_ty).to_string();
        ic.emit_typecheck_error(
            span,
            format!("range endpoints must be numeric, found `{display}`"),
        );
        return ic.ty_error();
    }

    // Record the implicit-spec request for codegen to materialise.
    // The synthesised value type stays Error until the generated
    // nominal is wired in.
    ic.register_implicit_spec(ImplicitSpec::Range, element_ty, span);
    ic.ty_error()
}
