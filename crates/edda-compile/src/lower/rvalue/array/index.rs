//! `ArrayIndex` rvalue lowering and the shared [`array_element_ptr`]
//! element-pointer resolver.
//!
//! `ArrayIndex` extracts the slice's data pointer, GEPs by the index,
//! and loads. [`array_element_ptr`] stops short of the load so the
//! aggregate `Assign` path can memcpy the element bytes instead.

use edda_mir::{Operand, Rvalue};
use inkwell::values::BasicValueEnum;

use crate::error::CompileError;

use crate::lower::operand::lower_operand;
use crate::lower::ty::inkwell_basic_type;
use crate::lower::LowerCtx;

/// Lower `RvalueKind::ArrayIndex` — extract the slice's data pointer,
/// GEP by the index, load the element.
pub(in crate::lower::rvalue) fn lower_array_index<'ctx>(
    rvalue: &Rvalue,
    array: &Operand,
    idx: &Operand,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    let (elem_ptr, elem_llvm_ty) = array_element_ptr(rvalue, array, idx, cx)?;
    let loaded = cx
        .builder
        .build_load(elem_llvm_ty, elem_ptr, "v")
        .expect("build_load of ArrayIndex result");
    Ok(loaded)
}

/// Resolve `RvalueKind::ArrayIndex` to an element pointer without
/// loading. The aggregate `Assign` path uses this so it can memcpy
/// the element bytes instead of doing a typed load that decomposes
/// the aggregate (and silently drops cross-variant padding bytes of
/// a sum-typed element). The pure-load path stays in
/// [`lower_array_index`].
pub(in crate::lower) fn array_element_ptr<'ctx>(
    rvalue: &Rvalue,
    array: &Operand,
    idx: &Operand,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<(inkwell::values::PointerValue<'ctx>, inkwell::types::BasicTypeEnum<'ctx>), CompileError>
{
    let slice_val = lower_operand(array, cx)?.ok_or_else(|| CompileError::UnsupportedMirShape {
        shape: "array-index-unit-array",
        detail: format!("body {:?} ArrayIndex array operand is Unit", cx.body_name),
    })?;
    let data_ptr_v = cx
        .builder
        .build_extract_value(slice_val.into_struct_value(), 0, "slice_ptr")
        .expect("extractvalue slice.ptr (index 0)");
    let data_ptr = data_ptr_v.into_pointer_value();
    let idx_val = lower_operand(idx, cx)?.ok_or_else(|| CompileError::UnsupportedMirShape {
        shape: "array-index-unit-index",
        detail: format!("body {:?} ArrayIndex index operand is Unit", cx.body_name),
    })?;
    let elem_llvm_ty = inkwell_basic_type(cx.context, &rvalue.ty, cx.program, cx.arch)
        .ok_or_else(|| CompileError::UnsupportedMirShape {
            shape: "array-index-non-lowerable-element",
            detail: format!(
                "body {:?} ArrayIndex result type {:?} is not lowerable",
                cx.body_name, rvalue.ty.kind
            ),
        })?;
    let elem_ptr = unsafe {
        cx.builder
            .build_gep(elem_llvm_ty, data_ptr, &[idx_val.into_int_value()], "elem")
            .expect("build_gep with valid index must succeed")
    };
    Ok((elem_ptr, elem_llvm_ty))
}
