//! Place-extraction + type-decl helpers shared by the mode transitions.
//!
//! `path_binding` / `field_projection_binding` extract the binding
//! (and field) a mode transition fires on; `type_field_set` resolves a
//! nominal product type to its declared field set.

use std::collections::BTreeSet;

use edda_intern::Symbol;

/// Extract the [`Symbol`] of a single-segment `Path` expression, if
/// any. Mode transitions only fire for plain `x` references
/// (assign targets, single-name `mutable` / `take` / `init` call-args);
/// `obj.field` and `xs[i]` are §5's per-field tracker's territory and
/// skip silently.
pub(crate) fn path_binding(expr: &super::super::HirExpr) -> Option<edda_intern::Symbol> {
    match &expr.kind {
        super::super::HirExprKind::Path(p) if p.segments.len() == 1 => Some(p.segments[0].name),
        _ => None,
    }
}

/// Extract `(binding, field)` from a `HirExprKind::Field { receiver,
/// name }` whose receiver is a single-segment path naming a binding.
/// Returns `None` for nested-field projections (`x.a.b`), non-Field
/// expressions, or expressions whose receiver isn't a binding name
/// — those defer to a later wave.
pub(crate) fn field_projection_binding(
    expr: &super::super::HirExpr,
) -> Option<(Symbol, Symbol)> {
    let super::super::HirExprKind::Field { receiver, name } = &expr.kind else {
        return None;
    };
    let binding = path_binding(receiver)?;
    Some((binding, name.name))
}

// `field_projection_binding` for tuple staged-init
/// Extract `(binding, index)` from a `HirExprKind::TupleIndex { receiver,
/// index }` whose receiver is a single-segment path naming a binding.
/// Returns `None` for nested receivers (`x.a.(i)`), non-TupleIndex
/// expressions, or receivers that aren't a bare binding name. Powers the
/// §5 per-element tracker for a tuple-typed `uninit` binding, where
/// element `i` plays the role a named field does for a record.
pub(crate) fn tuple_index_binding(
    expr: &super::super::HirExpr,
) -> Option<(Symbol, u32)> {
    let super::super::HirExprKind::TupleIndex { receiver, index } = &expr.kind else {
        return None;
    };
    let binding = path_binding(receiver)?;
    Some((binding, *index))
}

/// Compute the full set of member keys for a staged-init aggregate. For
/// a nominal product type this is the declared field-name [`Symbol`]s;
/// for a tuple type it is the interned decimal strings of its element
/// indices (`0`, `1`, …), so tuple element `i` reuses the
/// exact same per-member [`super::super::BindingState::PartialInit`]
/// tracker a record field does (a tuple has no field names, and a
/// decimal string is never a valid field identifier, so the two keyings
/// never alias within one binding). Used by §5's per-member transitions
/// to decide when a binding promotes from `PartialInit(F)` back to
/// `Valid` (i.e. when `F = members(T)`).
pub(super) fn type_field_set(
    ty: crate::ty::TyId,
    ic: &super::super::InferCx<'_>,
) -> Option<BTreeSet<Symbol>> {
    match ic.lower.ty_interner.kind(ty) {
        crate::ty::TyKind::Nominal(binding_id) => {
            let decl = ic.ty_cx.type_decl(*binding_id)?;
            match &decl.kind {
                crate::cx::TypeDeclShape::Product { fields } => {
                    Some(fields.iter().map(|f| f.name).collect())
                }
                crate::cx::TypeDeclShape::Sum { .. } => None,
            }
        }
        crate::ty::TyKind::Tuple(elems) => {
            Some((0..elems.len()).map(|i| ic.lower.interner.intern(&i.to_string())).collect())
        }
        _ => None,
    }
}
