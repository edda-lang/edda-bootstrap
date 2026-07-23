//! `MakeArray` rvalue lowering.
//!
//! `MakeArray` allocates `[N x T]` backing storage at the *current*
//! builder position, fills each slot, and assembles a fat-pointer
//! `{ ptr, isize }` slice value via `insertvalue`.

use edda_mir::{MirPrim, MirTypeKind, Operand, Rvalue};
use inkwell::types::BasicTypeEnum;
use inkwell::values::BasicValueEnum;

use crate::error::CompileError;

use crate::lower::operand::lower_operand;
use crate::lower::ty::{inkwell_basic_type, inkwell_prim_type};
use crate::lower::LowerCtx;

/// Lower `RvalueKind::MakeArray` — alloca `[N x T]` storage, fill it,
/// and assemble a `{ ptr, isize }` slice value via insertvalue.
pub(in crate::lower::rvalue) fn lower_make_array<'ctx>(
    rvalue: &Rvalue,
    elems: &[Operand],
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    let elem_ty = match &rvalue.ty.kind {
        MirTypeKind::Slice(t) => (**t).clone(),
        other => {
            return Err(CompileError::UnsupportedMirShape {
                shape: "make-array-non-slice-result",
                detail: format!(
                    "body {:?} MakeArray result type {other:?} is not a slice; \
                     MakeArray only builds slices",
                    cx.body_name
                ),
            });
        }
    };
    let elem_llvm_ty = inkwell_basic_type(cx.context, &elem_ty, cx.program, cx.arch).ok_or_else(
        || CompileError::UnsupportedMirShape {
            shape: "make-array-non-lowerable-element",
            detail: format!(
                "body {:?} MakeArray element type {:?} is not lowerable",
                cx.body_name, elem_ty.kind
            ),
        },
    )?;

    // Backing storage: alloca [N x T].
    let n = elems.len() as u32;
    let array_ty: BasicTypeEnum<'ctx> = match elem_llvm_ty {
        BasicTypeEnum::IntType(t) => t.array_type(n).into(),
        BasicTypeEnum::FloatType(t) => t.array_type(n).into(),
        BasicTypeEnum::StructType(t) => t.array_type(n).into(),
        BasicTypeEnum::PointerType(t) => t.array_type(n).into(),
        BasicTypeEnum::ArrayType(t) => t.array_type(n).into(),
        BasicTypeEnum::VectorType(t) => t.array_type(n).into(),
    };
    let storage_ptr = cx
        .builder
        .build_alloca(array_ty, "arrtmp")
        .expect("build_alloca in a positioned block must succeed");

    // Fill each slot: GEP [N x T], ptr, 0, i → elem_ptr ; store elem.
    let i32_ty = cx.context.i32_type();
    let zero = i32_ty.const_zero();
    for (i, elem) in elems.iter().enumerate() {
        let val = lower_operand(elem, cx)?.ok_or_else(|| CompileError::UnsupportedMirShape {
            shape: "make-array-unit-element",
            detail: format!(
                "body {:?} MakeArray element #{i} is Unit; slices never carry unit elements",
                cx.body_name
            ),
        })?;
        let idx_const = i32_ty.const_int(i as u64, false);
        let elem_ptr = unsafe {
            cx.builder
                .build_gep(array_ty, storage_ptr, &[zero, idx_const], "slot")
                .expect("build_gep into in-range array slot must succeed")
        };
        cx.builder
            .build_store(elem_ptr, val)
            .expect("build_store of MakeArray slot");
    }

    // Data pointer = GEP [N x T], ptr, 0, 0 → pointer to the first
    // element, decaying the array.
    let data_ptr = unsafe {
        cx.builder
            .build_gep(array_ty, storage_ptr, &[zero, zero], "data")
            .expect("build_gep [0,0] decay must succeed")
    };

    // Length constant: isize n.
    let isize_ty = inkwell_prim_type(cx.context, MirPrim::Isize, cx.arch)
        .expect("isize is always lowerable")
        .into_int_type();
    let len_const = isize_ty.const_int(elems.len() as u64, false);

    // Assemble the fat-pointer slice: `{ ptr, isize }`.
    let slice_struct_ty = inkwell_basic_type(cx.context, &rvalue.ty, cx.program, cx.arch)
        .expect("slice type is always lowerable")
        .into_struct_type();
    let undef = slice_struct_ty.get_undef();
    let with_ptr = cx
        .builder
        .build_insert_value(undef, data_ptr, 0, "slice_ptr")
        .expect("insertvalue slice.ptr")
        .into_struct_value();
    let with_len = cx
        .builder
        .build_insert_value(with_ptr, len_const, 1, "slice_len")
        .expect("insertvalue slice.len")
        .into_struct_value();
    Ok(with_len.into())
}
