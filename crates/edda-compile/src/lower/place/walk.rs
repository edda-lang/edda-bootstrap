//! Per-projection-step GEP walkers for [`super::walk_place`].
//!
//! Each `walk_*` function lowers one `Projection` step against the
//! current `(pointer, MirType)` pair, emitting the GEPs / loads that
//! advance to the next leaf. The pure-type resolvers live in
//! [`super::resolve`].

use edda_mir::{FieldIdx, Idx, LocalId, MirPrim, MirType, MirTypeKind, VariantIdx};
use inkwell::values::PointerValue;
use inkwell::AddressSpace;

use crate::error::CompileError;

use super::super::ty::{inkwell_basic_type, inkwell_prim_type};
use super::super::LowerCtx;
use super::resolve::{resolve_field_ty, resolve_variant_downcast_ty};

/// Walk a `Projection::Field` step: GEP into the current struct's field.
pub(super) fn walk_field<'ctx>(
    ptr: PointerValue<'ctx>,
    current_ty: &MirType,
    field_idx: FieldIdx,
    cx: &LowerCtx<'ctx, '_>,
    step_idx: usize,
) -> Result<(PointerValue<'ctx>, MirType), CompileError> {
    let field_ty = resolve_field_ty(current_ty, field_idx.index(), cx, step_idx)?;
    let struct_ty = inkwell_basic_type(cx.context, current_ty, cx.program, cx.arch)
        .expect("admitted root type lowers; tuple / product-ADT / variant-downcast tuple all produce struct")
        .into_struct_type();
    let next_ptr = cx
        .builder
        .build_struct_gep(struct_ty, ptr, field_idx.index() as u32, "f")
        .expect("build_struct_gep with a valid in-range index must succeed");
    Ok((next_ptr, field_ty))
}

/// Walk a `Projection::VariantDowncast` step: GEP into the sum's
/// payload slot and replace `current_ty` with the chosen variant's
/// synthetic tuple shape.
pub(super) fn walk_variant_downcast<'ctx>(
    ptr: PointerValue<'ctx>,
    current_ty: &MirType,
    variant_idx: VariantIdx,
    cx: &LowerCtx<'ctx, '_>,
    step_idx: usize,
) -> Result<(PointerValue<'ctx>, MirType), CompileError> {
    let next_ty = resolve_variant_downcast_ty(current_ty, variant_idx, cx, step_idx)?;
    let sum_struct_ty = inkwell_basic_type(cx.context, current_ty, cx.program, cx.arch)
        .expect("admitted sum-ADT root lowers to `{ tag, payload }`")
        .into_struct_type();
    let next_ptr = cx
        .builder
        .build_struct_gep(sum_struct_ty, ptr, 1, "down")
        .expect("build_struct_gep into payload slot (field 1) must succeed");
    Ok((next_ptr, next_ty))
}

/// Walk a `Projection::Index` step: load the slice value, extract its
/// data pointer, and GEP by the index local. Returns `(elem_ptr,
/// elem_ty)`.
pub(super) fn walk_index<'ctx>(
    ptr: PointerValue<'ctx>,
    current_ty: &MirType,
    idx_local: LocalId,
    cx: &LowerCtx<'ctx, '_>,
    step_idx: usize,
) -> Result<(PointerValue<'ctx>, MirType), CompileError> {
    let elem_ty = slice_element_ty(current_ty, cx, step_idx)?;
    let data_ptr = load_slice_data_ptr(ptr, current_ty, cx);
    let idx_val = load_index_local(idx_local, cx)?;
    let elem_llvm_ty = inkwell_basic_type(cx.context, &elem_ty, cx.program, cx.arch).ok_or_else(
        || CompileError::UnsupportedMirShape {
            shape: "index-non-lowerable-element",
            detail: format!(
                "body {:?} indexes a slice whose element type {:?} is not lowerable",
                cx.body_name, elem_ty.kind
            ),
        },
    )?;
    // `getelementptr <elem_ty>, ptr <data_ptr>, <idx>` — advances `idx`
    // elements forward (LLVM scales by sizeof(elem_ty)). Use the unsafe
    // (non-inbounds) builder; the typechecker is responsible for
    // bounds.
    let next_ptr = unsafe {
        cx.builder
            .build_gep(elem_llvm_ty, data_ptr, &[idx_val], "elem")
            .expect("build_gep with valid index must succeed")
    };
    Ok((next_ptr, elem_ty))
}

/// Walk a `Projection::Deref(T)` step: load the pointer value out of
/// the current `HeapPtr` slot and continue from the pointed-to address
/// with leaf type `T`. The read-through-pointer primitive that lets
/// MIR read an aggregate value (record / sum / slice) behind a
/// `HeapPtr`, not just a scalar word.
pub(super) fn walk_deref<'ctx>(
    ptr: PointerValue<'ctx>,
    current_ty: &MirType,
    pointee_ty: &MirType,
    cx: &LowerCtx<'ctx, '_>,
    step_idx: usize,
) -> Result<(PointerValue<'ctx>, MirType), CompileError> {
    if !matches!(current_ty.kind, MirTypeKind::Prim(MirPrim::HeapPtr)) {
        return Err(CompileError::UnsupportedMirShape {
            shape: "deref-projection-non-pointer",
            detail: format!(
                "body {:?} uses a Deref projection at step {step_idx} on type {:?}; \
                 Deref requires a HeapPtr leaf",
                cx.body_name, current_ty.kind
            ),
        });
    }
    let ptr_ty = cx.context.ptr_type(AddressSpace::default());
    let loaded = cx
        .builder
        .build_load(ptr_ty, ptr, "deref")
        .expect("build_load of HeapPtr from a positioned block must succeed")
        .into_pointer_value();
    Ok((loaded, pointee_ty.clone()))
}

/// Unwrap a `Slice(T)` MirType to `T`; non-slice roots produce
/// `index-projection-non-slice`.
fn slice_element_ty(
    current_ty: &MirType,
    cx: &LowerCtx<'_, '_>,
    step_idx: usize,
) -> Result<MirType, CompileError> {
    match &current_ty.kind {
        MirTypeKind::Slice(elem) => Ok((**elem).clone()),
        other => Err(CompileError::UnsupportedMirShape {
            shape: "index-projection-non-slice",
            detail: format!(
                "body {:?} uses an Index projection at step {step_idx} \
                 on type {other:?}; Index requires a slice root",
                cx.body_name
            ),
        }),
    }
}

/// Load the slice's `{ ptr, isize }` value and `extractvalue` the
/// data pointer (field 0).
fn load_slice_data_ptr<'ctx>(
    ptr: PointerValue<'ctx>,
    slice_ty: &MirType,
    cx: &LowerCtx<'ctx, '_>,
) -> PointerValue<'ctx> {
    let slice_struct_ty = inkwell_basic_type(cx.context, slice_ty, cx.program, cx.arch)
        .expect("slice always lowers")
        .into_struct_type();
    let slice_val = cx
        .builder
        .build_load(slice_struct_ty, ptr, "slice")
        .expect("build_load of slice from a positioned block must succeed");
    let data_ptr_v = cx
        .builder
        .build_extract_value(slice_val.into_struct_value(), 0, "slice_ptr")
        .expect("extractvalue of slice.ptr (index 0)");
    data_ptr_v.into_pointer_value()
}

/// Load the index local (always pointer-width int) and return it.
fn load_index_local<'ctx>(
    idx_local: LocalId,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<inkwell::values::IntValue<'ctx>, CompileError> {
    let idx_slot = cx.locals[idx_local.index()].ok_or_else(|| {
        CompileError::UnsupportedMirShape {
            shape: "index-local-not-lowerable",
            detail: format!(
                "body {:?} indexes by local#{} whose type has no alloca",
                cx.body_name,
                idx_local.index()
            ),
        }
    })?;
    let isize_ty = inkwell_prim_type(cx.context, edda_mir::MirPrim::Isize, cx.arch)
        .expect("isize is always lowerable")
        .into_int_type();
    Ok(cx
        .builder
        .build_load(isize_ty, idx_slot, "idx")
        .expect("build_load of index local")
        .into_int_value())
}
