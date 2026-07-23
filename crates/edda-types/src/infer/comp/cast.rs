//! Cast-expression inference.
//!
//! Numeric → numeric casts are admitted (integer to integer,
//! integer ↔ float, float ↔ float). Other cast forms (bool → integer,
//! pointer / user-type casts) are deferred.

use edda_span::Span;
use edda_syntax::ast::CastMode;

use crate::ty::{TyId, TyKind};

use crate::hir::HirExpr;

use super::super::{InferCx, TyEnv, synth_expr};

/// Synthesise a cast expression. Numeric → numeric
/// casts are admitted; other source / target combinations emit a diagnostic and
/// yield `Error`.
///
/// When `mode` is [`CastMode::Checked`], the function's declared
/// effect row must carry `err: Overflow` — the checked-cast originates
/// the overflow effect identically to the checked arithmetic operators
/// `+?` / `-?` / `*?` / `%?`. The effect entry is pushed onto the row
/// accumulator via [`crate::infer::op::attach_overflow_row_for_cast`]; absence
/// from the declared row is diagnosed at this site.
pub(in crate::infer) fn synth_cast(
    env: &mut TyEnv,
    operand: &mut HirExpr,
    target_ty: TyId,
    mode: CastMode,
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    if matches!(
        operand.kind,
        crate::hir::HirExprKind::Literal(edda_syntax::ast::Literal::Int { .. })
    ) {
        if let TyKind::Primitive(p) = ic.lower.ty_interner.kind(target_ty) {
            if p.is_integer() {
                super::super::check_expr(env, operand, target_ty, ic);
                if mode == CastMode::Checked {
                    crate::infer::op::attach_overflow_row_for_cast(ic, span);
                }
                return target_ty;
            }
        }
    }
    let src_ty = synth_expr(env, operand, ic);
    let error = ic.ty_error();
    if src_ty == error || target_ty == error {
        return error;
    }
    let src_kind = ic.lower.ty_interner.kind(src_ty);
    let tgt_kind = ic.lower.ty_interner.kind(target_ty);
    let (src_num, tgt_num) = match (src_kind, tgt_kind) {
        (TyKind::Primitive(a), TyKind::Primitive(b)) => (a.is_numeric(), b.is_numeric()),
        _ => (false, false),
    };
    if src_num && tgt_num {
        if mode == CastMode::Checked {
            crate::infer::op::attach_overflow_row_for_cast(ic, span);
        }
        return target_ty;
    }
    let src_display = ic.display_ty(src_ty).to_string();
    let tgt_display = ic.display_ty(target_ty).to_string();
    ic.emit_typecheck_error(
        span,
        format!(
            "cannot cast `{src_display}` to `{tgt_display}` — only numeric-to-numeric casts are admitted",
        ),
    );
    error
}
