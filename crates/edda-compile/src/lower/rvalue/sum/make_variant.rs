//! `MakeVariant` rvalue lowering and the shared
//! [`build_variant_value`] SSA-building helper.
//!
//! `MakeVariant` alloca's the outer `{ tag, max-variant-payload }`
//! sum struct, writes the variant's tag at field 0, writes the
//! variant's own payload struct at field 1, then loads the whole
//! sum back as an SSA value (LLVM does not admit a direct bitcast
//! between two structs whose layouts differ). The trailing payload
//! bytes of variants smaller than the max are left undef.
//!
//! [`build_variant_value`] is shared with the
//! [`crate::lower::terminator::raise`] walker, which packages an
//! error payload into the body's return sum.

use edda_mir::{AdtId, AdtKind, Idx, Operand, Rvalue, VariantIdx};
use inkwell::values::BasicValueEnum;

use crate::error::CompileError;

use crate::lower::rvalue::aggregate::build_aggregate;
use crate::lower::ty::{
    inkwell_basic_type, inkwell_prim_type, sum_max_variant_payload_type, sum_slot_access_align,
    sum_variant_payload_type,
};
use crate::lower::LowerCtx;

/// Lower `RvalueKind::MakeVariant`: alloca a sum, write the tag at
/// field 0, write the variant's payload struct into field 1, and
/// load the whole sum back as an SSA value.
pub(in crate::lower::rvalue) fn lower_make_variant<'ctx>(
    rvalue: &Rvalue,
    adt: AdtId,
    variant: VariantIdx,
    fields: &[Operand],
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    build_variant_value(&rvalue.ty, adt, variant, fields, cx)
}

/// Build a sum-typed SSA value for the given `variant`. Shared
/// between [`lower_make_variant`] (rvalue dispatch) and the Raise
/// terminator's lowering, both of which assemble a single
/// `{ tag, payload }` value for one variant.
pub(in crate::lower) fn build_variant_value<'ctx>(
    sum_ty: &edda_mir::MirType,
    adt: AdtId,
    variant: VariantIdx,
    fields: &[Operand],
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    let adt_def = cx
        .program
        .adts
        .get(adt)
        .expect("build_variant_value references an AdtId from the same program");
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
                "body {:?} build_variant_value on variant {} of ADT {:?}: discriminant is None — \
                 sum variants must carry a resolved discriminant",
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
                "body {:?} build_variant_value on sum ADT {:?}: tag_width is None — \
                 sum ADTs must carry a resolved tag-width primitive",
                cx.body_name, adt_def.name
            ),
        }
    })?;

    let _max_payload_ty = sum_max_variant_payload_type(cx.context, adt_def, cx.program, cx.arch)
        .ok_or_else(|| CompileError::UnsupportedMirShape {
            shape: "make-variant-non-lowerable-payload",
            detail: format!(
                "body {:?} build_variant_value on ADT {:?}: max-variant payload references a non-lowerable type",
                cx.body_name, adt_def.name
            ),
        })?;
    let variant_payload_ty =
        sum_variant_payload_type(cx.context, adt_def, variant.index(), cx.program, cx.arch)
            .ok_or_else(|| CompileError::UnsupportedMirShape {
                shape: "make-variant-non-lowerable-variant",
                detail: format!(
                    "body {:?} build_variant_value on variant {} of ADT {:?}: variant payload references a non-lowerable type",
                    cx.body_name,
                    variant.index(),
                    adt_def.name
                ),
            })?;
    let outer_ty = inkwell_basic_type(cx.context, sum_ty, cx.program, cx.arch)
        .ok_or_else(|| CompileError::UnsupportedMirShape {
            shape: "make-variant-non-lowerable-sum",
            detail: format!(
                "body {:?} build_variant_value produces sum ADT {:?} which is not lowerable",
                cx.body_name, adt_def.name
            ),
        })?
        .into_struct_type();

    // Build the variant's payload struct as an SSA value.
    let payload_val = build_aggregate(variant_payload_ty, fields, cx, "var", "variant")?;

    // Build the tag constant at the sum's tag-int width.
    let tag_int_ty = inkwell_prim_type(cx.context, tag_prim, cx.arch)
        .expect("sum tag_width is integer-typed; inkwell_prim_type is Some")
        .into_int_type();
    let tag_val = tag_int_ty.const_int(discriminant, false);

    // alloca outer_ty; store tag at field 0; store variant payload at field 1; load back.
    //
    // The trailing `build_load` of the outer sum is a per-field
    // decomposition in LLVM — it reads only the bytes at offsets
    // that correspond to the max-variant payload struct's fields
    // and skips the cross-variant padding. When the chosen
    // variant's bytes sit inside the padding (e.g. `Ok(double)`
    // writes 8 bytes at offset 8 inside an `{ i8, { i64 } }`-shaped
    // payload struct), the decomposition drops bytes 9-15 of the
    // double. Downstream paths (sret return, Assign, Move/Copy of
    // the aggregate) propagate the lossy SSA. Caller cooperation
    // (see [`crate::lower::statement::lower_statement`] and the
    // Raise terminator) memcpys from this assembled alloca so the
    // bytes survive verbatim.
    let alloca = cx.build_entry_alloca(outer_ty, "mv.sum");
    let tag_ptr = cx
        .builder
        .build_struct_gep(outer_ty, alloca, 0, "mv.tag.ptr")
        .expect("build_struct_gep into tag slot (field 0)");
    cx.builder
        .build_store(tag_ptr, tag_val)
        .expect("build_store of tag");
    let payload_ptr = cx
        .builder
        .build_struct_gep(outer_ty, alloca, 1, "mv.pay.ptr")
        .expect("build_struct_gep into payload slot (field 1)");
    let payload_store = cx
        .builder
        .build_store(payload_ptr, payload_val)
        .expect("build_store of variant payload");
    // The payload slot is only conservatively aligned (the sum alloca's
    // `i64`-chunk blob); a payload whose natural alignment is wider
    // (an `i128`/`u128` variant field) must not keep its 16-byte store
    // alignment, or LLVM emits an aligned `movaps` into an 8-aligned
    // slot and faults.
    let payload_align = variant_def
        .fields
        .iter()
        .map(|f| sum_slot_access_align(&f.ty, cx.program, cx.arch))
        .max()
        .unwrap_or(1);
    payload_store
        .set_alignment(payload_align)
        .expect("set conservative alignment on variant-payload store");
    let loaded = cx
        .builder
        .build_load(outer_ty, alloca, "mv")
        .expect("build_load of assembled sum");
    Ok(loaded)
}
