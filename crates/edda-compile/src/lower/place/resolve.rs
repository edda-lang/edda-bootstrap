//! Pure-type projection resolvers for the place walker.
//!
//! These mirror [`super::walk`] but emit no inkwell instructions —
//! they walk a projection chain to the leaf [`MirType`] only. Used by
//! the GEP walkers (for the next leaf's type) and by rvalue paths that
//! need a subject's static type without materialising pointer-walks.

use edda_mir::{AdtKind, Idx, MirType, MirTypeKind, Place, Projection, VariantIdx};

use crate::error::CompileError;

use super::super::LowerCtx;

/// Pure-type analogue of [`walk_place`]: walks the projection chain
/// to produce the leaf [`MirType`] without emitting GEPs. Used by
/// [`super::rvalue::lower_extract_field`] and other rvalue paths that
/// need to know the subject's static type (in particular, the
/// enclosing sum's [`edda_mir::AdtId`]) without first materialising
/// pointer-walks.
pub(in crate::lower) fn place_leaf_ty(
    place: &Place,
    cx: &LowerCtx<'_, '_>,
) -> Result<MirType, CompileError> {
    let local_decl = cx
        .body
        .locals
        .get(place.local)
        .expect("Place::local indexes a real local; MIR validation guarantees this");
    let mut current_ty = local_decl.ty.clone();
    for (step_idx, proj) in place.projection.iter().enumerate() {
        match proj {
            Projection::Field(field_idx) => {
                current_ty = resolve_field_ty(&current_ty, field_idx.index(), cx, step_idx)?;
            }
            Projection::VariantDowncast(variant_idx) => {
                current_ty = resolve_variant_downcast_ty(&current_ty, *variant_idx, cx, step_idx)?;
            }
            Projection::Index(_) => {
                return Err(CompileError::UnsupportedMirShape {
                    shape: "index-projection",
                    detail: format!(
                        "body {:?} uses an Index projection at step {step_idx}; \
                         Array/Slice indexing is not yet enabled",
                        cx.body_name
                    ),
                });
            }
            Projection::Deref(pointee_ty) => {
                // Read-through-pointer: the static leaf is the carried
                // pointee type.
                current_ty = pointee_ty.clone();
            }
        }
    }
    Ok(current_ty)
}

/// Resolve the leaf type of a `VariantDowncast` step against the
/// current root type. Provides sum-ADT roots; product ADTs and non-ADT
/// types fail.
pub(super) fn resolve_variant_downcast_ty(
    current_ty: &MirType,
    variant_idx: VariantIdx,
    cx: &LowerCtx<'_, '_>,
    step_idx: usize,
) -> Result<MirType, CompileError> {
    let adt_id = match &current_ty.kind {
        MirTypeKind::Adt(id) => *id,
        other => {
            return Err(CompileError::UnsupportedMirShape {
                shape: "variant-downcast-non-adt",
                detail: format!(
                    "body {:?} VariantDowncast at step {step_idx} on non-ADT type {other:?}",
                    cx.body_name
                ),
            });
        }
    };
    let adt = cx
        .program
        .adts
        .get(adt_id)
        .expect("AdtId in a Place's local ty was issued by the same program");
    if adt.kind != AdtKind::Sum {
        return Err(CompileError::UnsupportedMirShape {
            shape: "variant-downcast-on-product",
            detail: format!(
                "body {:?} VariantDowncast at step {step_idx} on product ADT {:?}; \
                 only sum ADTs admit a downcast",
                cx.body_name, adt.name
            ),
        });
    }
    let variant = adt.variants.get(variant_idx.index()).ok_or_else(|| {
        CompileError::UnsupportedMirShape {
            shape: "variant-downcast-out-of-range",
            detail: format!(
                "body {:?} VariantDowncast({}) on ADT with {} variants at step {step_idx}",
                cx.body_name,
                variant_idx.index(),
                adt.variants.len()
            ),
        }
    })?;
    let elems: Vec<MirType> = variant.fields.iter().map(|f| f.ty.clone()).collect();
    Ok(MirType::new(MirTypeKind::Tuple(elems)))
}

/// Resolve the leaf type of a `Field` projection step against the
/// current root type. Provides tuple and `AdtKind::Product` roots;
/// `AdtKind::Sum` roots without a preceding `VariantDowncast` and
/// any other root type fail with a shape-specific error label.
pub(super) fn resolve_field_ty(
    current_ty: &MirType,
    field_index: usize,
    cx: &LowerCtx<'_, '_>,
    step_idx: usize,
) -> Result<MirType, CompileError> {
    match &current_ty.kind {
        MirTypeKind::Tuple(elems) => elems.get(field_index).cloned().ok_or_else(|| {
            CompileError::UnsupportedMirShape {
                shape: "field-projection-out-of-range",
                detail: format!(
                    "body {:?} projects Field#{} of a {}-tuple at step {step_idx}",
                    cx.body_name,
                    field_index,
                    elems.len()
                ),
            }
        }),
        MirTypeKind::Adt(adt_id) => {
            let adt = cx.program.adts.get(*adt_id).expect(
                "AdtId in a Place's local ty was issued by the same program",
            );
            if adt.kind != AdtKind::Product {
                return Err(CompileError::UnsupportedMirShape {
                    shape: "field-projection-on-sum",
                    detail: format!(
                        "body {:?} projects Field#{field_index} of a sum ADT at step \
                         {step_idx} without a preceding VariantDowncast — sum-typed \
                         field access must downcast first",
                        cx.body_name
                    ),
                });
            }
            let variant = adt
                .variants
                .first()
                .expect("AdtKind::Product has exactly one variant by construction");
            variant.fields.get(field_index).map(|f| f.ty.clone()).ok_or_else(|| {
                CompileError::UnsupportedMirShape {
                    shape: "field-projection-out-of-range",
                    detail: format!(
                        "body {:?} projects Field#{} of a {}-field record at step {step_idx}",
                        cx.body_name,
                        field_index,
                        variant.fields.len()
                    ),
                }
            })
        }
        other => Err(CompileError::UnsupportedMirShape {
            shape: "field-projection-non-aggregate",
            detail: format!(
                "body {:?} projects Field#{field_index} of type {other:?} at step \
                 {step_idx}; field projections require a tuple or product-ADT root",
                cx.body_name
            ),
        }),
    }
}
