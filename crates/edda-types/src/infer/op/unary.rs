//! Unary-operator inference.
//!
//! - **Unary** (`- ! ~`) — `Neg` requires numeric, `Not` requires
//!   `bool`, `BitNot` requires integer.
//!
//! Cascade diagnostics from an already-`Error` operand are
//! propagated silently — only the leaf failure emits.

use edda_span::Span;
use edda_syntax::ast::{Literal, UnOp};

use crate::hir::{HirExpr, HirExprKind};
use crate::prim::Primitive;
use crate::ty::{TyId, TyKind};

use super::super::lit::fits_in_primitive_negated;
use super::super::{InferCx, TyEnv, check_expr, synth_expr};

/// Check a unary expression against an expected type.
///
/// `Neg` and `BitNot` propagate `expected` into the operand: `-x` and
/// `~x` have the same type as `x`, so the expected type drives operand
/// narrowing (e.g. `-1` against `i32` checks `1 ⇐ i32`). `Not` always
/// produces `bool`, so synth and bridge.
pub(in crate::infer) fn check_unary(
    env: &mut TyEnv,
    op: UnOp,
    operand: &mut HirExpr,
    expected: TyId,
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    use super::super::path::check_synth_against_expected;

    match op {
        UnOp::Neg => {
            let is_numeric_expected = matches!(
                ic.lower.ty_interner.kind(expected),
                TyKind::Primitive(p) if p.is_numeric()
            );
            if is_numeric_expected {
                // MIN-of-signed accommodation: `-<lit>` against a
                // signed integer T admits `value <= 2^(bits-1)` so
                // `T::MIN` (whose magnitude is `T::MAX_POSITIVE + 1`)
                // resolves as a literal. The standard
                // `fits_in_primitive` range would reject this site
                // even though the negated value is in range.
                if let HirExprKind::Literal(Literal::Int { value, .. }) = &operand.kind
                    && let TyKind::Primitive(p) = ic.lower.ty_interner.kind(expected)
                    && p.is_integer()
                    && fits_in_primitive_negated(*value, *p)
                {
                    operand.ty = expected;
                    return expected;
                }
                check_expr(env, operand, expected, ic);
                return expected;
            }
            let synth = synth_unary(env, op, operand, ic, span);
            check_synth_against_expected(synth, expected, ic, span)
        }
        UnOp::BitNot => {
            let is_integer_expected = matches!(
                ic.lower.ty_interner.kind(expected),
                TyKind::Primitive(p) if p.is_integer()
            );
            if is_integer_expected {
                check_expr(env, operand, expected, ic);
                return expected;
            }
            let synth = synth_unary(env, op, operand, ic, span);
            check_synth_against_expected(synth, expected, ic, span)
        }
        UnOp::Not => {
            let synth = synth_unary(env, op, operand, ic, span);
            check_synth_against_expected(synth, expected, ic, span)
        }
    }
}

/// Synthesise a unary expression's type.
pub(in crate::infer) fn synth_unary(
    env: &mut TyEnv,
    op: UnOp,
    operand: &mut HirExpr,
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    match op {
        UnOp::Neg => {
            let t = synth_expr(env, operand, ic);
            let error = ic.ty_error();
            if t == error {
                return t;
            }
            if let TyKind::Primitive(p) = ic.lower.ty_interner.kind(t)
                && p.is_numeric()
            {
                return t;
            }
            let display = ic.display_ty(t).to_string();
            ic.emit_typecheck_error(
                span,
                format!("unary `-` requires a numeric operand, found `{display}`"),
            );
            error
        }
        UnOp::Not => {
            let bool_ty = ic.lower.ty_interner.prim(Primitive::Bool);
            check_expr(env, operand, bool_ty, ic);
            bool_ty
        }
        UnOp::BitNot => {
            let t = synth_expr(env, operand, ic);
            let error = ic.ty_error();
            if t == error {
                return t;
            }
            if let TyKind::Primitive(p) = ic.lower.ty_interner.kind(t)
                && p.is_integer()
            {
                return t;
            }
            let display = ic.display_ty(t).to_string();
            ic.emit_typecheck_error(
                span,
                format!("unary `~` requires an integer operand, found `{display}`"),
            );
            error
        }
    }
}
