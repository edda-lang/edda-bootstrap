//! `for pat in iter { ... }` inference.
//!
//! Per `corpus/edda-codex/language/01-syntax.md` §7 (Loops), `for` is a
//! statement form: it produces no value, iterates over a spec-provided
//! iterator, and binds each yielded element to `pat`. The body's value
//! type is irrelevant — the surrounding expression's value type is
//! `()`.
//!
//! Two iter shapes are admitted that the surface backend can lower without
//! the full iterator-protocol machinery:
//!
//! - **Range** — `0..<n` / `0..=n` with both endpoints present. The
//!   element type is the synth_range-recorded endpoint type.
//! - **Slice** — `xs: [T]`. The element type is `T`.
//!
//! Open-ended range forms (`..hi`, `lo..`, `..`) and other iter shapes
//! emit a typecheck diagnostic — they will be admitted when the
//! iterator-protocol seam lands.

use edda_span::Span;

use crate::hir::{HirBlock, HirExpr, HirExprKind, HirPat};
use crate::prim::Primitive;
use crate::ty::{TyId, TyKind};

use super::pat::bind_pat;
use super::{InferCx, TyEnv, mode, synth_block, synth_expr};

/// Synthesise `for pat in iter { ... }`. Always yields `()` — `for` is
/// the statement form per `corpus/edda-codex/language/01-syntax.md` §7.
pub(super) fn synth_for(
    env: &mut TyEnv,
    pat: &mut HirPat,
    iter: &mut HirExpr,
    body: &mut HirBlock,
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    let element_ty = synth_for_element_ty(env, iter, ic);
    let pre = env.clone();
    env.enter_scope();
    bind_pat(env, pat, element_ty, ic);
    let _ = synth_block(env, body, ic);
    env.exit_scope();
    mode::check_loop_reentry(env, &pre, ic, body.span);
    // Re-establish pre-body state so downstream sees the same env it
    // would have without the loop, matching `synth_loop`.
    *env = pre;
    let _ = span;
    ic.lower.ty_interner.prim(Primitive::Unit)
}

/// Derive the element type from the `for`'s iter expression.
///
/// Two admitted shapes:
///
/// - `HirExprKind::Range { lo: Some, hi: Some, .. }` — synth_range
///   leaves `iter.ty` as the error sentinel but populates both lo / hi
///   with the inferred endpoint type, which is the element type.
/// - any other iter — walk via `synth_expr`; admitted only when the
///   result type is `TyKind::Slice(elem)`.
fn synth_for_element_ty(
    env: &mut TyEnv,
    iter: &mut HirExpr,
    ic: &mut InferCx<'_>,
) -> TyId {
    if matches!(&iter.kind, HirExprKind::Range { lo: Some(_), hi: Some(_), .. }) {
        return range_iter_element_ty(env, iter, ic);
    }
    let iter_ty = synth_expr(env, iter, ic);
    match ic.lower.ty_interner.kind(iter_ty) {
        TyKind::Slice(elem) => *elem,
        _ if iter_ty == ic.ty_error() => ic.ty_error(),
        _ => {
            let display = ic.display_ty(iter_ty).to_string();
            ic.emit_typecheck_error(
                iter.span,
                format!(
                    "`for` iter must be a slice or range, found `{display}`",
                ),
            );
            ic.ty_error()
        }
    }
}

/// Element-type derivation for the closed-endpoint Range case. Walks
/// both endpoints with the same literal-narrowing rule `synth_range`
/// uses (the binary-operand-style mutual narrowing) so an integer
/// literal endpoint paired with a typed sibling adopts the sibling's
/// width. Returns the agreed endpoint type, which is also the binder's
/// type in `for i in lo..<hi`.
fn range_iter_element_ty(
    env: &mut TyEnv,
    iter: &mut HirExpr,
    ic: &mut InferCx<'_>,
) -> TyId {
    let HirExprKind::Range { lo: Some(lo), hi: Some(hi), .. } = &mut iter.kind else {
        return ic.ty_error();
    };
    let lo_is_int_lit = matches!(
        lo.kind,
        HirExprKind::Literal(edda_syntax::ast::Literal::Int { .. })
    );
    let hi_is_int_lit = matches!(
        hi.kind,
        HirExprKind::Literal(edda_syntax::ast::Literal::Int { .. })
    );
    let element_ty = if lo_is_int_lit && hi_is_int_lit {
        let _ = synth_expr(env, lo, ic);
        let _ = synth_expr(env, hi, ic);
        ic.lower.ty_interner.prim(Primitive::I64)
    } else if hi_is_int_lit {
        let lo_ty = synth_expr(env, lo, ic);
        super::check_expr(env, hi, lo_ty, ic);
        lo_ty
    } else if lo_is_int_lit {
        let hi_ty = synth_expr(env, hi, ic);
        super::check_expr(env, lo, hi_ty, ic);
        hi_ty
    } else {
        let lo_ty = synth_expr(env, lo, ic);
        let hi_ty = synth_expr(env, hi, ic);
        if lo_ty == ic.ty_error() || hi_ty == ic.ty_error() {
            return ic.ty_error();
        }
        if lo_ty != hi_ty {
            let lo_display = ic.display_ty(lo_ty).to_string();
            let hi_display = ic.display_ty(hi_ty).to_string();
            ic.emit_typecheck_error(
                iter.span,
                format!(
                    "for-range endpoints have incompatible types: `{lo_display}` and `{hi_display}`",
                ),
            );
            return ic.ty_error();
        }
        lo_ty
    };
    element_ty
}
