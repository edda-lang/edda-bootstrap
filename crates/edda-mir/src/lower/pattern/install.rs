//! Irrefutable pattern destructuring (LHS of `let` / `for`).
//!
//! Supports `Wildcard`, `Binding`, tuple destructuring, and `Struct`
//! (product-type field-by-field destructuring). `Variant` is inherently
//! refutable and remains rejected here.

use edda_span::Span;
use edda_types::{HirPat, HirPatKind, HirStructPatField};

use crate::ids::LocalId;

use super::super::ctx::LoweringContext;
use super::super::ty::lower_ty;
use super::{
    emit_extract_field, emit_extract_field_variant, find_product_field_idx, is_product_adt,
    push_unsupported, resolve_adt,
};

/// Install irrefutable bindings from a `let pat = expr;` (or `for pat in ...`)
/// pattern. `source` is the local that holds the value being destructured.
pub(in super::super) fn install_bindings(
    ctx: &mut LoweringContext<'_>,
    pat: &HirPat,
    source: LocalId,
) {
    match &pat.kind {
        HirPatKind::Wildcard => {}
        HirPatKind::Binding(ident) => {
            ctx.bindings.insert(ident.name, source);
        }
        HirPatKind::Tuple(elements) => {
            install_tuple_bindings(ctx, pat.span, elements, source);
        }
        HirPatKind::Struct { fields, .. } => {
            install_struct_bindings(ctx, pat, fields, source);
        }
        HirPatKind::Error => {}
        HirPatKind::Literal(_) => {
            // A literal in an irrefutable position is meaningless; the
            // typechecker should have rejected it. Report once and move on.
            push_unsupported(ctx, pat.span, "Literal in irrefutable position");
        }
        // Variant patterns are inherently refutable — there's nothing the
        // installer can do with `Some(x)` on the LHS of `let`.
        HirPatKind::Variant { .. } => push_unsupported(ctx, pat.span, "Variant"),
        HirPatKind::Guard { .. } => push_unsupported(ctx, pat.span, "Guard"),
        // Range and slice patterns are inherently refutable (a literal
        // bound / a length test), so they never reach an irrefutable
        // `let` / `for` position — rejected here like `Variant`.
        HirPatKind::Range { .. } => push_unsupported(ctx, pat.span, "Range"),
        HirPatKind::Slice { .. } => push_unsupported(ctx, pat.span, "Slice"),
        // `name @ inner` binds the whole value to `name` and recurses into
        // the (irrefutable) sub-pattern.
        HirPatKind::AtBinding { name, inner } => {
            ctx.bindings.insert(name.name, source);
            install_bindings(ctx, inner, source);
        }
    }
}

/// Tuple destructuring: emit `ExtractField` rvalues into fresh temps and
/// recurse for each sub-pattern.
fn install_tuple_bindings(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    elements: &[HirPat],
    source: LocalId,
) {
    for (i, sub_pat) in elements.iter().enumerate() {
        let elem_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, sub_pat.ty);
        let temp = match ctx.body.as_mut() {
            Some(body) => body.temp(elem_ty.clone(), span),
            None => return,
        };
        emit_extract_field(ctx, span, source, i as u32, temp, elem_ty);
        install_bindings(ctx, sub_pat, temp);
    }
}

/// Irrefutable struct destructuring for `let pat = e;` on a product type.
/// For each field, look up its `FieldIdx`, emit an `ExtractField` into a
/// fresh temp, and recurse into [`install_bindings`] against the
/// sub-pattern.
fn install_struct_bindings(
    ctx: &mut LoweringContext<'_>,
    pat: &HirPat,
    fields: &[HirStructPatField],
    source: LocalId,
) {
    let Some(adt_id) = resolve_adt(ctx, pat) else {
        push_unsupported(ctx, pat.span, "Struct");
        return;
    };
    if !is_product_adt(ctx, adt_id) {
        push_unsupported(ctx, pat.span, "Struct on sum ADT");
        return;
    }
    for field_pat in fields {
        let Some(field_idx) = find_product_field_idx(ctx, adt_id, field_pat.name.name) else {
            push_unsupported(ctx, field_pat.span, "Struct field name not in ADT");
            continue;
        };
        let elem_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, field_pat.pat.ty);
        let Some(temp) = ctx
            .body
            .as_mut()
            .map(|body| body.temp(elem_ty.clone(), field_pat.span))
        else {
            return;
        };
        emit_extract_field_variant(
            ctx,
            field_pat.span,
            source,
            None,
            field_idx,
            temp,
            elem_ty,
        );
        install_bindings(ctx, &field_pat.pat, temp);
    }
}
