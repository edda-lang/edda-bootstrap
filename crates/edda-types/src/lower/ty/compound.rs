//! Structural composite type lowering — function-pointer and tuple types.
//!
//! Holds [`lower_function_type`] (`function(P, ...) -> R uses {row}` →
//! [`crate::TyKind::FnPtr`]) and [`lower_tuple`] (`(T1, …, Tn)` →
//! [`crate::TyInterner::tuple`]). Both recurse through
//! [`super::lower_type`] for their element / parameter / return types.

use edda_diag::{Diagnostics, LintConfig};
use edda_span::Span;
use edda_syntax::ast;

use crate::effect::EffectRow;
use crate::lower::LowerCx;
use crate::lower::emit_typecheck_error;
use crate::lower::lower_effect_row;
use crate::sig::{FnPtrParam, FnPtrSig, ParamMode};
use crate::ty::TyId;

use super::lower_type;

/// Lower a `function(P, ...) -> R uses {row}` type expression to its
/// interned [`TyId`].
///
/// Walks each parameter through [`lower_type`], converts the mode via
/// [`ParamMode::from_ast`], lowers the return type, lowers the effect
/// row (defaulting to empty when absent), builds an [`FnPtrSig`], and
/// hands it to [`crate::TyInterner::fn_ptr`].
///
/// Source-level parameter names (`function(x: i32)`) are intentionally
/// dropped — names are documentation, not part of the type. The two
/// surface forms `function(i32)` and `function(x: i32)` produce the
/// same [`TyId`].
pub(super) fn lower_function_type(
    params: &[ast::FnTypeParam],
    ret: &ast::Type,
    effects: Option<&ast::EffectRow>,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> TyId {
    let lowered_params: Vec<FnPtrParam> = params
        .iter()
        .map(|p| FnPtrParam {
            mode: ParamMode::from_ast(p.mode),
            ty: lower_type(&p.ty, cx, diags, lint_cfg),
        })
        .collect();
    let return_ty = lower_type(ret, cx, diags, lint_cfg);
    let row = match effects {
        Some(r) => lower_effect_row(r, cx, diags, lint_cfg),
        None => EffectRow::empty(),
    };
    cx.ty_interner.fn_ptr(FnPtrSig {
        params: lowered_params.into_boxed_slice(),
        return_ty,
        effects: row,
    })
}

/// Lower a tuple-type expression. Surface tuples are `n >= 2` (the
/// parser enforces this — `(x)` is grouping); a one-element tuple
/// reaches this only from the D-22 mono substitution splicing a
/// single-payload variant's composite `(u32)` into a specialized
/// signature, and lowers to the distinct one-element
/// [`TyKind::Tuple`]. Only the degenerate empty form is rejected
/// (`()` is the unit type, never a zero-tuple).
pub(super) fn lower_tuple(
    elems: &[ast::Type],
    span: Span,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> TyId {
    if elems.is_empty() {
        // Neither the parser nor the mono substitution produces this,
        // but fail-soft so a bug upstream cannot crash the lowering pass.
        emit_typecheck_error(
            diags,
            lint_cfg,
            span,
            "tuple type requires at least 1 element (`()` is the unit type, not a zero-tuple)",
        );
        return cx.ty_interner.error();
    }
    let lowered: Vec<TyId> = elems
        .iter()
        .map(|t| lower_type(t, cx, diags, lint_cfg))
        .collect();
    cx.ty_interner.tuple(lowered)
}
