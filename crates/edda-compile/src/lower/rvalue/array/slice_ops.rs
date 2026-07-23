//! `SliceSubrange`, `ArrayLen`, and `StringBytes` rvalue lowering.
//!
//! `SliceSubrange` borrows a `[lo, hi)` half-open sub-slice via a
//! forward-GEP of the source pointer plus a `hi - lo` length
//! recomputation (no copy). `ArrayLen` extracts field 1. `StringBytes`
//! is a zero-cost identity transform: a `String`'s `{ ptr, isize }`
//! value flows through unchanged as a `[u8]` fat pointer.

use edda_mir::{MirTypeKind, Operand, Rvalue};
use inkwell::types::BasicTypeEnum;
use inkwell::values::BasicValueEnum;

use crate::error::CompileError;

use crate::lower::operand::lower_operand;
use crate::lower::ty::inkwell_basic_type;
use crate::lower::LowerCtx;

/// Lower `RvalueKind::SliceSubrange` — borrow a half-open `[lo, hi)`
/// sub-slice from the source. The result is a fresh fat pointer that
/// shares the source's backing storage; no copy is performed.
pub(in crate::lower::rvalue) fn lower_subrange<'ctx>(
    rvalue: &Rvalue,
    source: &Operand,
    lo: &Operand,
    hi: &Operand,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    let slice_val = lower_operand(source, cx)?.ok_or_else(|| CompileError::UnsupportedMirShape {
        shape: "slice-subrange-unit-source",
        detail: format!(
            "body {:?} SliceSubrange source operand is Unit",
            cx.body_name
        ),
    })?;
    let lo_val = lower_operand(lo, cx)?.ok_or_else(|| CompileError::UnsupportedMirShape {
        shape: "slice-subrange-unit-lo",
        detail: format!(
            "body {:?} SliceSubrange lo operand is Unit",
            cx.body_name
        ),
    })?;
    let hi_val = lower_operand(hi, cx)?.ok_or_else(|| CompileError::UnsupportedMirShape {
        shape: "slice-subrange-unit-hi",
        detail: format!(
            "body {:?} SliceSubrange hi operand is Unit",
            cx.body_name
        ),
    })?;
    // Pull the source's data pointer (field 0) — the length field is
    // unused on the input side: the new length comes from `hi - lo`.
    let src_ptr_v = cx
        .builder
        .build_extract_value(slice_val.into_struct_value(), 0, "src_ptr")
        .expect("extractvalue slice.ptr (index 0)");
    let src_ptr = src_ptr_v.into_pointer_value();
    // GEP forward by `lo` elements to compute the sub-slice's data ptr.
    // The GEP type is the slice's element type, recovered from the
    // result rvalue's type (`MirTypeKind::Slice(elem)`).
    let elem_mir_ty = match &rvalue.ty.kind {
        MirTypeKind::Slice(t) => (**t).clone(),
        other => {
            return Err(CompileError::UnsupportedMirShape {
                shape: "slice-subrange-non-slice-result",
                detail: format!(
                    "body {:?} SliceSubrange result type {other:?} is not a slice",
                    cx.body_name
                ),
            });
        }
    };
    let elem_llvm_ty = inkwell_basic_type(cx.context, &elem_mir_ty, cx.program, cx.arch)
        .ok_or_else(|| CompileError::UnsupportedMirShape {
            shape: "slice-subrange-non-lowerable-element",
            detail: format!(
                "body {:?} SliceSubrange element type {:?} is not lowerable",
                cx.body_name, elem_mir_ty.kind
            ),
        })?;
    let new_ptr = unsafe {
        cx.builder
            .build_gep(elem_llvm_ty, src_ptr, &[lo_val.into_int_value()], "sub_ptr")
            .expect("build_gep on slice subrange must succeed")
    };
    // Compute new length = hi - lo.
    let new_len = cx
        .builder
        .build_int_sub(hi_val.into_int_value(), lo_val.into_int_value(), "sub_len")
        .expect("isize sub for slice length");
    // Assemble the fat pointer `{ new_ptr, new_len }` via insertvalue
    // on a poison struct of the slice's LLVM shape.
    let slice_struct_ty = inkwell_basic_type(cx.context, &rvalue.ty, cx.program, cx.arch)
        .ok_or_else(|| CompileError::UnsupportedMirShape {
            shape: "slice-subrange-non-lowerable-result",
            detail: format!(
                "body {:?} SliceSubrange result type {:?} has no LLVM shape",
                cx.body_name, rvalue.ty.kind
            ),
        })?;
    let BasicTypeEnum::StructType(slice_st) = slice_struct_ty else {
        return Err(CompileError::UnsupportedMirShape {
            shape: "slice-subrange-non-struct-result",
            detail: format!(
                "body {:?} SliceSubrange result LLVM type is not a struct",
                cx.body_name
            ),
        });
    };
    let undef = slice_st.get_undef();
    let with_ptr = cx
        .builder
        .build_insert_value(undef, new_ptr, 0, "subslice_ptr")
        .expect("insertvalue subslice.ptr")
        .into_struct_value();
    let with_len = cx
        .builder
        .build_insert_value(with_ptr, new_len, 1, "subslice_len")
        .expect("insertvalue subslice.len")
        .into_struct_value();
    Ok(with_len.into())
}

/// Lower `RvalueKind::ArrayLen` — extract the slice's length field.
pub(in crate::lower::rvalue) fn lower_array_len<'ctx>(
    array: &Operand,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    let slice_val = lower_operand(array, cx)?.ok_or_else(|| CompileError::UnsupportedMirShape {
        shape: "array-len-unit-array",
        detail: format!("body {:?} ArrayLen array operand is Unit", cx.body_name),
    })?;
    let len_v = cx
        .builder
        .build_extract_value(slice_val.into_struct_value(), 1, "slice_len")
        .expect("extractvalue slice.len (index 1)");
    Ok(len_v)
}

/// Lower `RvalueKind::StringBytes` — expose a `String`'s UTF-8 byte buffer
/// as a `[u8]` fat pointer.
///
/// A `String` value shares the `{ ptr, isize }` runtime layout with
/// `[u8]`; this rvalue is a zero-cost identity transform — it passes the
/// same struct value through without modification. The LLVM type for the
/// result `[u8]` and for the input `String` are both lowered as a two-field
/// struct `{ ptr, isize }` by the slice lowering.
pub(in crate::lower::rvalue) fn lower_string_bytes<'ctx>(
    string: &Operand,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    // String and [u8] share the fat-pointer layout; pass the value through.
    let val = lower_operand(string, cx)?.ok_or_else(|| CompileError::UnsupportedMirShape {
        shape: "string-bytes-unit-string",
        detail: format!(
            "body {:?} StringBytes string operand is Unit",
            cx.body_name
        ),
    })?;
    Ok(val)
}
