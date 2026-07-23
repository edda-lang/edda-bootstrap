//! Pattern lowering for the AST → HIR pass.

use edda_diag::{Diagnostics, LintConfig};
use edda_syntax::ast;

use crate::hir::{
    HirPat, HirPatKind, HirStructPatField, HirVariantPatPayload,
};

use super::super::LowerCx;
use super::expr::lower_expr;
use super::path::lower_hir_path;

/// Lower an AST pattern to its HIR counterpart.
pub(crate) fn lower_pat(
    pat: &ast::Pat,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> HirPat {
    let kind = match &pat.kind {
        ast::PatKind::Wildcard => HirPatKind::Wildcard,
        ast::PatKind::Binding(ident) => HirPatKind::Binding(*ident),
        ast::PatKind::Literal(lit) => HirPatKind::Literal(*lit),
        ast::PatKind::Tuple(elems) => HirPatKind::Tuple(
            elems
                .iter()
                .map(|p| lower_pat(p, cx, diags, lint_cfg))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        ),
        ast::PatKind::Variant { path, payload } => HirPatKind::Variant {
            path: lower_hir_path(path),
            payload: lower_variant_pat_payload(payload, cx, diags, lint_cfg),
        },
        ast::PatKind::Struct {
            path,
            fields,
            rest,
        } => HirPatKind::Struct {
            path: lower_hir_path(path),
            fields: fields
                .iter()
                .map(|f| lower_struct_pat_field(f, cx, diags, lint_cfg))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            rest: *rest,
        },
        ast::PatKind::Guard { pat, cond } => HirPatKind::Guard {
            pat: Box::new(lower_pat(pat, cx, diags, lint_cfg)),
            cond: lower_expr(cond, cx, diags, lint_cfg),
        },
        ast::PatKind::Range { lo, hi, kind } => HirPatKind::Range {
            lo: *lo,
            hi: *hi,
            kind: *kind,
        },
        ast::PatKind::AtBinding { name, inner } => HirPatKind::AtBinding {
            name: *name,
            inner: Box::new(lower_pat(inner, cx, diags, lint_cfg)),
        },
        ast::PatKind::Slice {
            prefix,
            rest,
            suffix,
        } => HirPatKind::Slice {
            prefix: prefix
                .iter()
                .map(|p| lower_pat(p, cx, diags, lint_cfg))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            rest: *rest,
            suffix: suffix
                .iter()
                .map(|p| lower_pat(p, cx, diags, lint_cfg))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        },
        ast::PatKind::Error => HirPatKind::Error,
    };
    HirPat {
        span: pat.span,
        ty: cx.ty_interner.error(),
        kind,
    }
}

fn lower_struct_pat_field(
    field: &ast::StructPatField,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> HirStructPatField {
    HirStructPatField {
        span: field.span,
        name: field.name,
        pat: lower_pat(&field.pat, cx, diags, lint_cfg),
    }
}

fn lower_variant_pat_payload(
    payload: &ast::VariantPatPayload,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> HirVariantPatPayload {
    match payload {
        ast::VariantPatPayload::None => HirVariantPatPayload::None,
        ast::VariantPatPayload::Tuple(pats) => HirVariantPatPayload::Tuple(
            pats.iter()
                .map(|p| lower_pat(p, cx, diags, lint_cfg))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        ),
        ast::VariantPatPayload::Struct(fields) => HirVariantPatPayload::Struct(
            fields
                .iter()
                .map(|f| lower_struct_pat_field(f, cx, diags, lint_cfg))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        ),
    }
}
