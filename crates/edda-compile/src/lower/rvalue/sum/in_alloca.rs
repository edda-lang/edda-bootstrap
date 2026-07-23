//! Byte-preserving sum-variant construction directly into a stack
//! alloca ([`build_variant_in_alloca`]) plus its aggregate-field
//! source-pointer probe.
//!
//! Used by the `Assign` and `Raise` callers that follow construction
//! with a memcpy: building into an alloca and handing back the pointer
//! (instead of the lossy SSA round-trip [`super::build_variant_value`]
//! performs) preserves the cross-variant padding bytes an SSA
//! load/store would drop.

use edda_mir::{AdtId, AdtKind, Idx, Operand, VariantIdx};
use inkwell::types::BasicType;

use crate::error::CompileError;

use crate::lower::ty::{
    inkwell_basic_type, inkwell_prim_type, sum_slot_access_align, sum_variant_payload_type,
};
use crate::lower::LowerCtx;

/// Build a sum-typed variant aggregate directly into a fresh stack
/// alloca and return the alloca pointer (without the trailing
/// per-field `load` that the regular `lower_make_variant` does).
/// Used by `Assign` and `Raise` callers that follow up with a
/// memcpy — this is the byte-preserving construction path. The
/// alloca's outer LLVM type is `inkwell_basic_type(sum_ty)` so the
/// caller knows the size for the memcpy.
pub(in crate::lower) fn build_variant_in_alloca<'ctx>(
    sum_ty: &edda_mir::MirType,
    adt: AdtId,
    variant: VariantIdx,
    fields: &[Operand],
    cx: &LowerCtx<'ctx, '_>,
) -> Result<(inkwell::values::PointerValue<'ctx>, inkwell::types::BasicTypeEnum<'ctx>), CompileError>
{
    let adt_def = cx
        .program
        .adts
        .get(adt)
        .expect("build_variant_in_alloca references an AdtId from the same program");
    if !matches!(adt_def.kind, AdtKind::Sum) {
        return Err(CompileError::UnsupportedMirShape {
            shape: "make-variant-on-product",
            detail: format!(
                "body {:?} builds a variant of product ADT {:?}; sum-only path",
                cx.body_name, adt_def.name
            ),
        });
    }
    if variant.index() >= adt_def.variants.len() {
        return Err(CompileError::UnsupportedMirShape {
            shape: "make-variant-out-of-range",
            detail: format!(
                "body {:?} variant index {} but ADT {:?} has {} variants",
                cx.body_name,
                variant.index(),
                adt_def.name,
                adt_def.variants.len()
            ),
        });
    }
    let variant_def = &adt_def.variants[variant.index()];
    let discriminant = variant_def.discriminant.ok_or_else(|| {
        CompileError::UnsupportedMirShape {
            shape: "make-variant-missing-discriminant",
            detail: format!(
                "body {:?} build_variant_in_alloca on variant {} of ADT {:?}: \
                 discriminant is None — sum variants must carry a resolved discriminant",
                cx.body_name,
                variant.index(),
                adt_def.name
            ),
        }
    })?;
    let tag_prim = adt_def.tag_width.ok_or_else(|| {
        CompileError::UnsupportedMirShape {
            shape: "make-variant-missing-tag-width",
            detail: format!(
                "body {:?} build_variant_in_alloca on sum ADT {:?}: tag_width is None — \
                 sum ADTs must carry a resolved tag-width primitive",
                cx.body_name, adt_def.name
            ),
        }
    })?;
    let variant_payload_ty =
        sum_variant_payload_type(cx.context, adt_def, variant.index(), cx.program, cx.arch)
            .ok_or_else(|| CompileError::UnsupportedMirShape {
                shape: "make-variant-non-lowerable-variant",
                detail: format!(
                    "body {:?} build_variant_in_alloca on variant {} of ADT {:?}: \
                     variant payload references a non-lowerable type",
                    cx.body_name,
                    variant.index(),
                    adt_def.name
                ),
            })?;
    let outer_ty = inkwell_basic_type(cx.context, sum_ty, cx.program, cx.arch)
        .ok_or_else(|| CompileError::UnsupportedMirShape {
            shape: "make-variant-non-lowerable-sum",
            detail: format!(
                "body {:?} build_variant_in_alloca produces sum ADT {:?} which is not lowerable",
                cx.body_name, adt_def.name
            ),
        })?
        .into_struct_type();
    let tag_int_ty = inkwell_prim_type(cx.context, tag_prim, cx.arch)
        .expect("sum tag_width is integer-typed")
        .into_int_type();
    let tag_val = tag_int_ty.const_int(discriminant, false);
    let alloca = cx.build_entry_alloca(outer_ty, "mv.sum");
    let tag_ptr = cx
        .builder
        .build_struct_gep(outer_ty, alloca, 0, "mv.tag.ptr")
        .expect("build_struct_gep into tag slot");
    cx.builder
        .build_store(tag_ptr, tag_val)
        .expect("build_store of tag");
    let payload_ptr = cx
        .builder
        .build_struct_gep(outer_ty, alloca, 1, "mv.pay.ptr")
        .expect("build_struct_gep into payload slot");
    // Populate the variant payload field-by-field directly in the
    // payload slot. Aggregate-typed fields go through memcpy from
    // their source pointer (the SSA `load <agg>; insertvalue, idx`
    // chain that `build_aggregate` would produce decomposes the
    // aggregate and drops bytes of any smaller-variant sum). Scalar
    // fields use a plain store.
    let variant_def = &adt_def.variants[variant.index()];
    for (i, field_op) in fields.iter().enumerate() {
        let Some(field_def) = variant_def.fields.get(i) else {
            return Err(CompileError::UnsupportedMirShape {
                shape: "make-variant-field-arity",
                detail: format!(
                    "body {:?} build_variant_in_alloca: field arity mismatch at index {i} for variant {} of ADT {:?}",
                    cx.body_name, variant.index(), adt_def.name
                ),
            });
        };
        let field_llvm_ty = inkwell_basic_type(cx.context, &field_def.ty, cx.program, cx.arch)
            .ok_or_else(|| CompileError::UnsupportedMirShape {
                shape: "make-variant-non-lowerable-field",
                detail: format!(
                    "body {:?} build_variant_in_alloca: variant field #{i} of type {:?} does not lower",
                    cx.body_name, field_def.ty.kind
                ),
            })?;
        let field_ptr = cx
            .builder
            .build_struct_gep(variant_payload_ty, payload_ptr, i as u32, "mv.var.field.ptr")
            .expect("build_struct_gep into variant payload field");
        if let Some(src_ptr) = aggregate_field_source_ptr(field_op, &field_def.ty, cx)? {
            let size_val = field_llvm_ty
                .size_of()
                .expect("aggregate variant field type has a sizeof");
            cx.builder
                .build_memcpy(
                    field_ptr,
                    AGGREGATE_COPY_ALIGN,
                    src_ptr,
                    AGGREGATE_COPY_ALIGN,
                    size_val,
                )
                .expect("build_memcpy of aggregate variant field into payload slot");
        } else if let Some(val) = crate::lower::operand::lower_operand(field_op, cx)? {
            let store = cx
                .builder
                .build_store(field_ptr, val)
                .expect("build_store of scalar variant field into payload slot");
            // The payload slot is only conservatively aligned (the sum
            // alloca's `i64`-chunk blob), so a wider scalar (`i128`/
            // `u128`) must not keep its natural 16-byte store alignment,
            // or LLVM emits an aligned `movaps` into an 8-aligned slot
            // and faults.
            store
                .set_alignment(sum_slot_access_align(&field_def.ty, cx.program, cx.arch))
                .expect("set conservative alignment on scalar variant-field store");
        }
        // else: the operand yields no LLVM value (a Unit / Never-typed
        // operand — error-recovery `Operand::Unit` or a `Copy`/`Move`
        // of a no-alloca Unit/Never local). A zero-sized value carries
        // no bytes, so we skip the store and leave the slot undef. The
        // payload struct still reserves a real slot at index `i` (the
        // declared field type lowered via `inkwell_basic_type` above),
        // so the GEP-by-`i` field-index mapping stays aligned with the
        // read side (`extract::lower_extract_field_variant`, which GEPs
        // the same `sum_variant_payload_type` struct by `field.index()`).
    }
    Ok((alloca, outer_ty.into()))
}

/// Conservative alignment used for the per-field memcpy when
/// populating a variant payload slot. See `statement.rs` for the
/// same constant.
const AGGREGATE_COPY_ALIGN: u32 = 8;

/// Source-pointer probe for an aggregate-typed variant field.
/// Mirrors `statement::aggregate_use_source_ptr` — used here to feed
/// `build_variant_in_alloca`'s per-field memcpy. Errors propagate
/// from `lower_place_pointer` when a projection step cannot lower.
fn aggregate_field_source_ptr<'ctx>(
    operand: &Operand,
    field_ty: &edda_mir::MirType,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<Option<inkwell::values::PointerValue<'ctx>>, CompileError> {
    use edda_mir::{MirPrim, MirTypeKind};
    let is_aggregate = matches!(
        &field_ty.kind,
        MirTypeKind::Prim(MirPrim::Str)
            | MirTypeKind::Slice(_)
            | MirTypeKind::Tuple(_)
            | MirTypeKind::Adt(_)
            // Fat function value `{ code, env }`
            | MirTypeKind::FnPtr(_)
    );
    if !is_aggregate {
        return Ok(None);
    }
    let place = match operand {
        Operand::Copy(p) | Operand::Move(p) => p,
        _ => return Ok(None),
    };
    if place.projection.is_empty() {
        return Ok(cx.locals[place.local.index()]);
    }
    Ok(Some(crate::lower::place::lower_place_pointer(place, cx)?))
}
