//! MIR [`Rvalue`] -> inkwell value, split by rvalue family.
//!
//! Covers `Use`, `BinOp`, and `UnOp` on primitive operands; `Cast`
//! between primitives via [`lower_cast`]; tuple makers / extractors;
//! record makers + record-subject field extraction; and sum-typed rvalues:
//!
//! - `RvalueKind::MakeVariant` — builds the variant's payload via
//!   `build_aggregate`, then composes the outer
//!   `{ tag, max-variant-payload }` struct through an alloca-store-load
//!   round-trip (LLVM does not admit a direct bitcast between two
//!   structs whose layouts differ). The trailing payload bytes of
//!   smaller variants are left undef.
//! - `RvalueKind::ExtractTag` — `extractvalue` at index 0 of the
//!   sum struct.
//! - `RvalueKind::ExtractField { variant: Some(v), .. }` — alloca
//!   the sum, store it, GEP into the payload slot, typed-load the
//!   variant's own payload struct, and `extractvalue` the field.
//!
//! The slice/array family: `MakeArray` allocates
//! backing storage and assembles a fat-pointer slice; `ArrayIndex`
//! GEPs into the slice's data pointer; `ArrayLen` extracts the
//! length field.
//!
//! [`lower_binop`] dispatches on operand class (float / bool / int);
//! each class has its own helper. [`lower_unop`] consumes the
//! [`crate::ops::llvm_unop_shape`] dispatch table.
//!
//! # Submodule layout
//!
//! - [`aggregate`] — tuple and record makers (`MakeTuple`,
//!   `MakeRecord`) plus the shared `build_aggregate` helper.
//! - [`sum`] — sum-typed rvalues (`MakeVariant`); also exports
//!   [`build_variant_value`] which the [`super::terminator::Raise`]
//!   walker reuses.
//! - [`array`] — slice/array rvalues (`MakeArray`, `ArrayIndex`,
//!   `ArrayLen`).
//! - [`extract`] — field/tag extraction (`ExtractField`,
//!   `ExtractTag`).
//! - [`binop`] — primitive [`BinOp`] dispatch (int / bool / float
//!   classes).
//! - [`unop_cast`] — primitive [`UnOp`] and primitive `Cast`.

pub(super) mod aggregate;
pub(super) mod array;
pub(super) mod binop;
pub(super) mod extract;
pub(super) mod sum;
pub(super) mod unop_cast;

use edda_mir::{Idx, Operand, Rvalue, RvalueKind};
use inkwell::values::BasicValueEnum;

use crate::error::CompileError;

use super::operand::lower_operand;
use super::place::lower_place_pointer;
use super::LowerCtx;

// Re-export so existing callers (`super::rvalue::build_variant_value`
// in `terminator::raise`, `super::rvalue::lower_binop` etc.) keep
// resolving against the same path after the split.
pub(super) use binop::lower_binop;
pub(super) use sum::{build_variant_in_alloca, build_variant_value};
pub(super) use unop_cast::{lower_cast, lower_unop};

/// Lower an [`Rvalue`] to a single inkwell value usable as the RHS of
/// an `Assign` or as a `Return` operand. Covers `MakeVariant`,
/// `ExtractTag`, and the sum-variant `ExtractField` case on top of
/// the tuple + record set, plus the slice family
/// (`MakeArray`, `ArrayIndex`, `SliceSubrange`, `ArrayLen`,
/// `StringBytes`).
pub(crate) fn lower_rvalue<'ctx>(
    rvalue: &Rvalue,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    match &rvalue.kind {
        RvalueKind::Use(operand) => lower_use(operand, cx),
        RvalueKind::BinOp { op, lhs, rhs, prim } => {
            let lhs_val = lower_operand(lhs, cx)?.ok_or_else(|| CompileError::UnsupportedMirShape {
                shape: "binop-unit-operand",
                detail: format!("body {:?} BinOp lhs is Unit", cx.body_name),
            })?;
            let rhs_val = lower_operand(rhs, cx)?.ok_or_else(|| CompileError::UnsupportedMirShape {
                shape: "binop-unit-operand",
                detail: format!("body {:?} BinOp rhs is Unit", cx.body_name),
            })?;
            lower_binop(*op, lhs_val, rhs_val, *prim, cx)
        }
        RvalueKind::UnOp { op, arg, prim } => {
            let val = lower_operand(arg, cx)?.ok_or_else(|| CompileError::UnsupportedMirShape {
                shape: "unop-unit-operand",
                detail: format!("body {:?} UnOp operand is Unit", cx.body_name),
            })?;
            lower_unop(*op, val, *prim, cx.context, cx.builder, cx.body_name)
        }
        RvalueKind::Cast {
            src,
            src_prim,
            dst_prim,
        } => {
            let val = lower_operand(src, cx)?.ok_or_else(|| CompileError::UnsupportedMirShape {
                shape: "cast-unit-operand",
                detail: format!("body {:?} Cast source operand is Unit", cx.body_name),
            })?;
            lower_cast(val, *src_prim, *dst_prim, cx.context, cx.builder, cx.arch, cx.body_name)
        }
        RvalueKind::MakeTuple { elems } => aggregate::lower_make_tuple(rvalue, elems, cx),
        RvalueKind::MakeRecord { adt, fields } => {
            aggregate::lower_make_record(rvalue, *adt, fields, cx)
        }
        RvalueKind::MakeVariant {
            adt,
            variant,
            fields,
        } => sum::lower_make_variant(rvalue, *adt, *variant, fields, cx),
        RvalueKind::ExtractField {
            subject,
            variant,
            field,
        } => extract::lower_extract_field(subject, *variant, *field, cx),
        RvalueKind::ExtractTag { subject } => extract::lower_extract_tag(subject, cx),
        RvalueKind::MakeArray { elems } => array::lower_make_array(rvalue, elems, cx),
        RvalueKind::ArrayIndex { array, idx } => array::lower_array_index(rvalue, array, idx, cx),
        RvalueKind::SliceSubrange { source, lo, hi } => {
            array::lower_subrange(rvalue, source, lo, hi, cx)
        }
        RvalueKind::ArrayLen { array } => array::lower_array_len(array, cx),
        RvalueKind::StringBytes(op) => array::lower_string_bytes(op, cx),
        RvalueKind::FunctionRef(body_id) => lower_function_ref(rvalue, *body_id, cx),
        RvalueKind::MakeClosure { code, env } => lower_make_closure(rvalue, *code, env, cx),
        RvalueKind::Ref { place } => {
            let ptr = lower_place_pointer(place, cx)?;
            Ok(ptr.into())
        }
    }
}

/// Lower `RvalueKind::FunctionRef(body_id)` — take the address of an
/// in-program body and produce an *env-less* fat function value
/// `{ code, 0 }`. Used for captureless closures and the forwarding
/// shims synthesised for named functions used as values; both have a
/// leading (ignored) env parameter so the indirect call's leading
/// env arg lands harmlessly.
fn lower_function_ref<'ctx>(
    rvalue: &Rvalue,
    body_id: edda_mir::BodyId,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    let code_ptr = code_ptr_of_body(body_id, cx)?;
    let env_ty = crate::lower::ty::inkwell_prim_type(cx.context, edda_mir::MirPrim::Usize, cx.arch)
        .expect("usize is always lowerable");
    let zero_env = env_ty.into_int_type().const_zero();
    build_fn_value(rvalue, code_ptr, zero_env.into(), cx)
}

/// Lower `RvalueKind::MakeClosure { code, env }` — construct a fat
/// function value carrying a non-trivial environment word.
fn lower_make_closure<'ctx>(
    rvalue: &Rvalue,
    code: edda_mir::BodyId,
    env: &Operand,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    let code_ptr = code_ptr_of_body(code, cx)?;
    let env_val = lower_operand(env, cx)?.ok_or_else(|| CompileError::UnsupportedMirShape {
        shape: "make-closure-unit-env",
        detail: format!("body {:?} MakeClosure env operand is Unit", cx.body_name),
    })?;
    build_fn_value(rvalue, code_ptr, env_val, cx)
}

/// Assemble a fat function value `{ code, env }` from a code pointer and
/// an env word, via chained `insertvalue` over `undef`.
fn build_fn_value<'ctx>(
    rvalue: &Rvalue,
    code_ptr: inkwell::values::PointerValue<'ctx>,
    env_val: BasicValueEnum<'ctx>,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    let _ = rvalue;
    let struct_ty = crate::lower::ty::fn_value_struct_type(cx.context, cx.arch);
    let with_code = cx
        .builder
        .build_insert_value(struct_ty.get_undef(), code_ptr, 0, "fnval.code")
        .expect("insert code ptr into fat fn value")
        .into_struct_value();
    let with_env = cx
        .builder
        .build_insert_value(with_code, env_val, 1, "fnval.env")
        .expect("insert env word into fat fn value")
        .into_struct_value();
    Ok(with_env.into())
}

/// Resolve a body's code pointer. The declare pass (lower_program) has
/// already registered every body with the module under the per-body
/// symbol name produced by
/// [`super::super::program::compute_symbol_names`], so
/// `module.get_function(name)` returns the body's inkwell
/// `FunctionValue`. Cast it to a pointer via
/// `as_global_value().as_pointer_value()`; LLVM 15+ opaque pointers
/// make this a no-op at the IR level.
fn code_ptr_of_body<'ctx>(
    body_id: edda_mir::BodyId,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<inkwell::values::PointerValue<'ctx>, CompileError> {
    let callee_body = cx.program.bodies.get(body_id).ok_or_else(|| {
        CompileError::UnsupportedMirShape {
            shape: "function-ref-out-of-range",
            detail: format!(
                "body {:?} references BodyId#{} which is not in program.bodies",
                cx.body_name,
                body_id.as_u32()
            ),
        }
    })?;
    let callee_name = cx
        .symbol_names
        .get(body_id.index())
        .map(|s| s.as_str())
        .unwrap_or_else(|| cx.interner.resolve(callee_body.name));
    let function_value = cx.module.get_function(callee_name).ok_or_else(|| {
        CompileError::UnsupportedMirShape {
            shape: "function-ref-not-declared",
            detail: format!(
                "body {:?} references body {callee_name:?} but the program declare pass \
                 did not register it",
                cx.body_name
            ),
        }
    })?;
    Ok(function_value.as_global_value().as_pointer_value())
}

/// Lower `RvalueKind::Use` — pass through the operand's value.
fn lower_use<'ctx>(
    operand: &Operand,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    let value = lower_operand(operand, cx)?;
    value.ok_or_else(|| CompileError::UnsupportedMirShape {
        shape: "use-of-unit-operand",
        detail: format!(
            "body {:?} uses `Operand::Unit` as the RHS of an Rvalue::Use; \
             unit values have no LLVM scalar form",
            cx.body_name
        ),
    })
}
