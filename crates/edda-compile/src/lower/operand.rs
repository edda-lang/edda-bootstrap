//! MIR [`Operand`] -> inkwell `BasicValueEnum`.
//!
//! Three operand flavours:
//! - `Copy(place)` / `Move(place)` — emit `load` of the place's
//!   alloca slot. `Copy` and `Move` are treated identically; the
//!   linearity check is MIR's responsibility, not the LLVM walker's.
//! - `Const(const_id)` — materialise the [`Const`] at the given
//!   `ConstId` via [`lower_const`].
//! - `Unit` — returns `Ok(None)` to signal "no scalar value"; used by
//!   `ret void` paths.
//!
//! [`lower_const`] is the const-table side of the operand path:
//! looks up a [`Const`] and produces the corresponding inkwell value.

use edda_mir::{Const, ConstValue, Idx, MirPrim, MirTypeKind, Operand};
use inkwell::AddressSpace;
use inkwell::types::BasicTypeEnum;
use inkwell::values::BasicValueEnum;

use crate::error::CompileError;

use super::place::lower_place_read;
use super::ty::{inkwell_basic_type, inkwell_prim_type};
use super::LowerCtx;

/// Lower an [`Operand`] to an inkwell value (or `None` for `Unit` / no-alloca locals).
pub(crate) fn lower_operand<'ctx>(
    operand: &Operand,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<Option<BasicValueEnum<'ctx>>, CompileError> {
    match operand {
        Operand::Unit => Ok(None),
        Operand::Const(id) => {
            let value = lower_const(&cx.program.consts[*id], cx)?;
            Ok(Some(value))
        }
        Operand::Copy(place) | Operand::Move(place) => {
            // Locals with no alloca (Unit / Never) have no LLVM representation.
            // Treat them like Operand::Unit — e.g. `return move _0` where _0: ()
            // should lower to `ret void`, not fail.
            if cx.locals[place.local.index()].is_none() {
                return Ok(None);
            }
            let value = lower_place_read(place, cx)?;
            Ok(Some(value))
        }
    }
}

/// Lower a [`Const`] to an inkwell [`BasicValueEnum`] under the
/// primitive-only contract.
pub(crate) fn lower_const<'ctx>(
    constant: &Const,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    // `Zero` of any lowerable type is the type's zero value (a zeroed
    // aggregate for Str / tuples / ADTs / slices / fn-values, not just a
    // scalar zero). Handled ahead of the primitive-only extraction below
    // so a zeroed aggregate constant — e.g. the match result-slot seed in
    // `edda-mir`'s `lower_match` — lowers
    // instead of hitting the `non-primitive-const` rejection.
    if matches!(constant.value, ConstValue::Zero) {
        let llvm_ty = inkwell_basic_type(cx.context, &constant.ty, cx.program, cx.arch)
            .ok_or_else(|| CompileError::UnsupportedMirShape {
                shape: "zero-of-non-lowerable",
                detail: format!(
                    "body {:?} builds Const Zero of type {:?}, which has no LLVM \
                     representation",
                    cx.body_name, constant.ty.kind
                ),
            })?;
        return Ok(match llvm_ty {
            BasicTypeEnum::IntType(t) => t.const_zero().into(),
            BasicTypeEnum::FloatType(t) => t.const_zero().into(),
            BasicTypeEnum::PointerType(t) => t.const_null().into(),
            BasicTypeEnum::StructType(t) => t.const_zero().into(),
            BasicTypeEnum::ArrayType(t) => t.const_zero().into(),
            BasicTypeEnum::VectorType(t) => t.const_zero().into(),
        });
    }

    let prim = match &constant.ty.kind {
        MirTypeKind::Prim(p) => *p,
        other => {
            return Err(CompileError::UnsupportedMirShape {
                shape: "non-primitive-const",
                detail: format!(
                    "body {:?} returns a Const of type {other:?}; \
                     compound-type constants are not yet handled",
                    cx.body_name
                ),
            });
        }
    };

    match (&constant.value, prim) {
        (ConstValue::Str(sym), MirPrim::Str) => {
            let s = cx.interner.resolve(*sym);
            let len = s.len() as u64;
            // Create a null-terminated global constant and take a pointer to it.
            let global = cx
                .builder
                .build_global_string_ptr(s, "str.data")
                .expect("build_global_string_ptr must succeed in a positioned block");
            let data_ptr = global.as_pointer_value();
            let len_ty = inkwell_prim_type(cx.context, MirPrim::Usize, cx.arch)
                .expect("usize is always lowerable")
                .into_int_type();
            let len_val = len_ty.const_int(len, false);
            let ptr_ty: BasicTypeEnum<'ctx> =
                cx.context.ptr_type(AddressSpace::default()).into();
            let fat_ptr_ty = cx.context.struct_type(&[ptr_ty, len_ty.into()], false);
            let fat0 = fat_ptr_ty.const_zero();
            let fat1 = cx
                .builder
                .build_insert_value(fat0, data_ptr, 0, "str.ptr")
                .expect("insertvalue ptr into fat-ptr struct")
                .into_struct_value();
            let fat2 = cx
                .builder
                .build_insert_value(fat1, len_val, 1, "str.len")
                .expect("insertvalue len into fat-ptr struct")
                .into_struct_value();
            Ok(fat2.into())
        }
        (ConstValue::Int(v), p) if is_integer_prim(p) => {
            let ty = inkwell_prim_type(cx.context, p, cx.arch).expect("non-Str primitive lowers");
            let int_ty = ty.into_int_type();
            Ok(int_const_signed(int_ty, *v).into())
        }
        (ConstValue::Uint(v), p) if is_integer_prim(p) => {
            let ty = inkwell_prim_type(cx.context, p, cx.arch).expect("non-Str primitive lowers");
            let int_ty = ty.into_int_type();
            Ok(int_const_unsigned(int_ty, *v).into())
        }
        (ConstValue::Float(bits), MirPrim::F32 | MirPrim::F64) => {
            let ty = inkwell_prim_type(cx.context, prim, cx.arch).expect("F32/F64 lowers");
            let float_ty = ty.into_float_type();
            Ok(float_ty.const_float(f64::from_bits(*bits)).into())
        }
        (ConstValue::Bool(b), MirPrim::Bool) => Ok(cx
            .context
            .bool_type()
            .const_int(if *b { 1 } else { 0 }, false)
            .into()),
        // An integer payload typed `HeapPtr` lowers to an `inttoptr` constant:
        // the integer is widened to pointer width and reinterpreted as an
        // opaque `ptr`. The entry-capability materialisation
        // uses this to seed `main`'s
        // fd-backed capability slots (`Stdout` -> 1, `Stderr` -> 2,
        // `Filesystem` -> AT_FDCWD) with their backing descriptor so a
        // pure-Edda runtime can read the fd out of the capability value.
        (ConstValue::Uint(v), MirPrim::HeapPtr) => {
            let usize_ty = inkwell_prim_type(cx.context, MirPrim::Usize, cx.arch)
                .expect("usize is always lowerable")
                .into_int_type();
            let ptr_ty = cx.context.ptr_type(AddressSpace::default());
            Ok(int_const_unsigned(usize_ty, *v)
                .const_to_pointer(ptr_ty)
                .into())
        }
        (ConstValue::Zero, p) => {
            let ty = inkwell_prim_type(cx.context, p, cx.arch).ok_or_else(|| {
                CompileError::UnsupportedMirShape {
                    shape: "zero-of-str",
                    detail: format!(
                        "Const Zero of type Str in body {:?}; \
                         slice constants are not yet handled",
                        cx.body_name
                    ),
                }
            })?;
            Ok(match ty {
                BasicTypeEnum::IntType(t) => t.const_zero().into(),
                BasicTypeEnum::FloatType(t) => t.const_zero().into(),
                // `HeapPtr` is the one primitive that lowers to an opaque
                // `ptr` rather than an int/float — `Zero` of it is the
                // null pointer. The entry-capability materialisation
                // seeds `main`'s allocator
                // slot with `Const(Zero: HeapPtr)`; the runtime ignores the
                // value so a null handle is sound.
                BasicTypeEnum::PointerType(t) => t.const_null().into(),
                _ => unreachable!("primitive types are int, float, or opaque pointer"),
            })
        }
        _ => Err(CompileError::UnsupportedMirShape {
            shape: "const-shape",
            detail: format!(
                "ConstValue {:?} with type {:?} is not yet lowered; \
                 Str support awaits the string pool",
                constant.value, constant.ty.kind
            ),
        }),
    }
}

/// Whether `prim` lowers to an inkwell integer type.
fn is_integer_prim(prim: MirPrim) -> bool {
    matches!(
        prim,
        MirPrim::I8
            | MirPrim::I16
            | MirPrim::I32
            | MirPrim::I64
            | MirPrim::I128
            | MirPrim::U8
            | MirPrim::U16
            | MirPrim::U32
            | MirPrim::U64
            | MirPrim::U128
            | MirPrim::Isize
            | MirPrim::Usize
            | MirPrim::Char
    )
}

/// Build a signed-integer inkwell `IntValue` from an `i128` payload.
fn int_const_signed<'ctx>(int_ty: inkwell::types::IntType<'ctx>, v: i128) -> inkwell::values::IntValue<'ctx> {
    match i64::try_from(v) {
        Ok(v64) => int_ty.const_int(v64 as u64, true),
        Err(_) => {
            let bits = v as u128;
            let low = bits as u64;
            let high = (bits >> 64) as u64;
            int_ty.const_int_arbitrary_precision(&[low, high])
        }
    }
}

/// Build an unsigned-integer inkwell `IntValue` from a `u128` payload.
fn int_const_unsigned<'ctx>(int_ty: inkwell::types::IntType<'ctx>, v: u128) -> inkwell::values::IntValue<'ctx> {
    match u64::try_from(v) {
        Ok(v64) => int_ty.const_int(v64, false),
        Err(_) => {
            let low = v as u64;
            let high = (v >> 64) as u64;
            int_ty.const_int_arbitrary_precision(&[low, high])
        }
    }
}
