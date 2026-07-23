//! Block / tuple checking and synthesis dispatchers.
//!
//! - [`synth_block`] is the synthesise-mode block walker: infer each
//!   statement, then the trailing expression (or default to
//!   [`Primitive::Unit`] when no trailing exists). The block's
//!   synthesised type is the trailing expression's type.
//! - [`check_block`] is the check-mode mirror that propagates the
//!   expected type into the trailing expression — needed for narrowing
//!   integer / literal leaves when the surrounding context already
//!   pins the type.
//! - [`check_tuple`] destructures a tuple `expected` so per-element
//!   checking narrows leaves; falls back to T-Synth-Check on arity /
//!   shape mismatch so the structural-mismatch diagnostic fires.

use edda_span::Span;

use crate::hir::{HirBlock, HirExpr};
use crate::prim::Primitive;
use crate::ty::TyId;

use super::path::check_synth_against_expected;
use super::{InferCx, TyEnv, check_expr, comp, infer_stmt, synth_expr};

/// Check a tuple expression against `expected`. When `expected` is a
/// tuple type of matching arity, each element checks against its
/// declared component type. Otherwise route through T-Synth-Check.
pub(super) fn check_tuple(
    env: &mut TyEnv,
    elems: &mut [HirExpr],
    expected: TyId,
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    if let crate::ty::TyKind::Tuple(declared) = ic.lower.ty_interner.kind(expected) {
        if declared.len() == elems.len() {
            // Clone declared types out of the interner-borrowed slice
            // so we can drop the immutable borrow before recursing into
            // check_expr (which takes &mut InferCx).
            let declared: Vec<TyId> = declared.to_vec();
            for (elem, want) in elems.iter_mut().zip(declared.into_iter()) {
                check_expr(env, elem, want, ic);
            }
            return expected;
        }
    }
    let synth = comp::synth_tuple(env, elems, ic, span);
    check_synth_against_expected(synth, expected, ic, span)
}

/// Check an array / slice literal against `expected`. When `expected`
/// is a slice type `[T]`, each element checks against `T` (and `[]`
/// simply adopts `expected`, taking its element type from context).
/// Otherwise route through T-Synth-Check.
pub(super) fn check_array(
    env: &mut TyEnv,
    elems: &mut [HirExpr],
    expected: TyId,
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    if let crate::ty::TyKind::Slice(elem) = ic.lower.ty_interner.kind(expected) {
        let elem = *elem;
        for e in elems.iter_mut() {
            check_expr(env, e, elem, ic);
        }
        return expected;
    }
    let synth = comp::synth_array(env, elems, ic, span);
    check_synth_against_expected(synth, expected, ic, span)
}

/// Check a [`HirBlock`] against `expected`.
///
/// Pushes a fresh `TyEnv` frame, infers each statement in source order,
/// then either checks the trailing expression against `expected` (if
/// present) or compares `Unit` to `expected` (block-as-statement).
pub(super) fn check_block(
    env: &mut TyEnv,
    block: &mut HirBlock,
    expected: TyId,
    ic: &mut InferCx<'_>,
) -> TyId {
    env.enter_scope();
    for stmt in block.stmts.iter_mut() {
        infer_stmt(env, stmt, ic);
    }
    let ty = match block.trailing.as_deref_mut() {
        Some(trailing) => {
            check_expr(env, trailing, expected, ic);
            trailing.ty
        }
        None => {
            let unit = ic.lower.ty_interner.prim(Primitive::Unit);
            check_synth_against_expected(unit, expected, ic, block.span)
        }
    };
    // A `linear` binding handed out as the block's trailing value is
    // moved, not dropped — consume it before the scope-exit sweep.
    if let Some(trailing) = block.trailing.as_deref() {
        super::mode::consume_moved_linear(env, trailing, ic);
    }
    super::mode::check_block_exit_linear(env, block.span, ic);
    env.exit_scope();
    block.ty = ty;
    ty
}

/// Type-check a [`HirBlock`].
///
/// Pushes a fresh `TyEnv` frame, infers each statement in source
/// order, infers the trailing expression (if any), and pops the
/// frame. The block's value type is the trailing expression's
/// synthesised type, or [`Primitive::Unit`] when no trailing
/// expression is present.
pub(crate) fn synth_block(env: &mut TyEnv, block: &mut HirBlock, ic: &mut InferCx<'_>) -> TyId {
    env.enter_scope();
    for stmt in block.stmts.iter_mut() {
        infer_stmt(env, stmt, ic);
    }
    let ty = match block.trailing.as_deref_mut() {
        Some(trailing) => synth_expr(env, trailing, ic),
        None => ic.lower.ty_interner.prim(Primitive::Unit),
    };
    // A `linear` binding handed out as the block's trailing value is
    // moved, not dropped — consume it before the scope-exit sweep.
    if let Some(trailing) = block.trailing.as_deref() {
        super::mode::consume_moved_linear(env, trailing, ic);
    }
    super::mode::check_block_exit_linear(env, block.span, ic);
    env.exit_scope();
    block.ty = ty;
    ty
}
