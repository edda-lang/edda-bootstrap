//! Scalar terminator-arm walkers: `Return`, `Goto`, `SwitchBool`,
//! `Unreachable`, `Panic`.
//!
//! The simple terminator set lowered by [`super::lower_terminator`]'s
//! dispatcher. Each arm is a `pub(super)` walker the dispatcher
//! delegates to; the Return walker also hosts the sret-aware
//! aggregate-copy helpers ([`is_return_slot_load`],
//! [`aggregate_operand_ptr`]) it needs.

use edda_mir::{Idx, LocalId, Operand};
use inkwell::AddressSpace;
use inkwell::types::BasicType;

use crate::error::CompileError;

use super::super::local::body_uses_sret;
use super::super::operand::lower_operand;
use super::super::LowerCtx;

/// See `terminator::call::AGGREGATE_COPY_ALIGN`.
const RETURN_COPY_ALIGN: u32 = 8;

/// Whether the Return operand reads from the return slot local
/// (`_0`) — used by the sret-aware Return walker to skip a
/// redundant store when prior `_0 = <rvalue>` statements already
/// wrote through the sret pointer.
fn is_return_slot_load(operand: &Operand) -> bool {
    match operand {
        Operand::Copy(p) | Operand::Move(p) => {
            p.local == LocalId::RETURN_SLOT && p.projection.is_empty()
        }
        _ => false,
    }
}

/// Get a pointer to the operand's source memory when it reads an
/// aggregate local — used by the Return walker to memcpy into the
/// sret slot instead of going through an SSA round-trip that drops
/// padding bytes.
fn aggregate_operand_ptr<'ctx>(
    operand: &Operand,
    cx: &LowerCtx<'ctx, '_>,
) -> Option<inkwell::values::PointerValue<'ctx>> {
    let place = match operand {
        Operand::Copy(p) | Operand::Move(p) => p,
        _ => return None,
    };
    if !place.projection.is_empty() {
        return None;
    }
    let decl = cx.body.locals.get(place.local)?;
    let is_aggregate = matches!(
        &decl.ty.kind,
        edda_mir::MirTypeKind::Prim(edda_mir::MirPrim::Str)
            | edda_mir::MirTypeKind::Slice(_)
            | edda_mir::MirTypeKind::Tuple(_)
            | edda_mir::MirTypeKind::Adt(_)
    );
    if !is_aggregate {
        return None;
    }
    cx.locals[place.local.index()]
}

/// Lower a `Return` terminator arm.
pub(super) fn lower_return<'ctx>(
    operand: &Operand,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<(), CompileError> {
    // Win64 sret: the function returns void and writes the
    // result through the caller-supplied pointer that lives
    // in `locals[RETURN_SLOT]`. Every prior `_0 = <rvalue>`
    // already wrote through to that pointer (via memcpy when
    // the rvalue was a MakeVariant or aggregate Use), so a
    // final `Move(_0)` reads from sret and a re-store would
    // be redundant.
    //
    // For `Return(Move(_other_aggregate_local))` we memcpy
    // from the source local's alloca to the sret pointer —
    // SSA round-trip would decompose the struct and drop
    // padding bytes (the same loss that breaks `Ok(double)`
    // inside an `{ i8, { i64 } }`-shaped payload). Scalar
    // returns and constants use the regular load+store.
    if body_uses_sret(cx.body, cx.program, cx.arch, cx.os) {
        let sret_ptr = cx.locals[edda_mir::LocalId::RETURN_SLOT.index()].expect(
            "sret-returning body has its return slot bound to the sret pointer",
        );
        if !is_return_slot_load(operand) {
            if let Some(src_ptr) = aggregate_operand_ptr(operand, cx) {
                let leaf_ty = super::super::ty::inkwell_basic_type(
                    cx.context,
                    &cx.body.return_ty,
                    cx.program,
                    cx.arch,
                )
                .expect("aggregate return type lowers");
                let size_val = leaf_ty
                    .size_of()
                    .expect("aggregate return type has a sizeof");
                cx.builder
                    .build_memcpy(
                        sret_ptr,
                        RETURN_COPY_ALIGN,
                        src_ptr,
                        RETURN_COPY_ALIGN,
                        size_val,
                    )
                    .expect("build_memcpy of aggregate Return into sret slot");
            } else if let Some(v) = lower_operand(operand, cx)? {
                cx.builder
                    .build_store(sret_ptr, v)
                    .expect("build_store of Return value into sret slot");
            }
        }
        cx.builder
            .build_return(None)
            .expect("build_return inside a positioned block must succeed");
    } else {
        let value = lower_operand(operand, cx)?;
        match value {
            Some(v) => cx
                .builder
                .build_return(Some(&v))
                .expect("build_return inside a positioned block must succeed"),
            None => cx
                .builder
                .build_return(None)
                .expect("build_return inside a positioned block must succeed"),
        };
    }
    Ok(())
}

/// Lower a `Goto` terminator arm.
pub(super) fn lower_goto<'ctx>(
    target: edda_mir::BlockId,
    llvm_blocks: &[inkwell::basic_block::BasicBlock<'ctx>],
    cx: &LowerCtx<'ctx, '_>,
) -> Result<(), CompileError> {
    let bb = llvm_blocks[target.index()];
    cx.builder
        .build_unconditional_branch(bb)
        .expect("build_unconditional_branch inside a positioned block must succeed");
    Ok(())
}

/// Lower a `SwitchBool` terminator arm.
pub(super) fn lower_switch_bool<'ctx>(
    cond: &Operand,
    true_bb: edda_mir::BlockId,
    false_bb: edda_mir::BlockId,
    llvm_blocks: &[inkwell::basic_block::BasicBlock<'ctx>],
    cx: &LowerCtx<'ctx, '_>,
) -> Result<(), CompileError> {
    let cond_val = lower_operand(cond, cx)?.ok_or_else(|| CompileError::UnsupportedMirShape {
        shape: "switch-bool-unit-cond",
        detail: format!("body {:?} SwitchBool cond is Unit", cx.body_name),
    })?;
    let cond_int = cond_val.into_int_value();
    let then_bb = llvm_blocks[true_bb.index()];
    let else_bb = llvm_blocks[false_bb.index()];
    cx.builder
        .build_conditional_branch(cond_int, then_bb, else_bb)
        .expect("build_conditional_branch inside a positioned block must succeed");
    Ok(())
}

/// Lower an `Unreachable` terminator arm.
pub(super) fn lower_unreachable(cx: &LowerCtx<'_, '_>) -> Result<(), CompileError> {
    cx.builder
        .build_unreachable()
        .expect("build_unreachable inside a positioned block must succeed");
    Ok(())
}

/// Lower a `Panic` terminator arm.
pub(super) fn lower_panic<'ctx>(
    msg: &Operand,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<(), CompileError> {
    // Lower the message (a `{ ptr, isize }` fat pointer), extract its
    // ptr + len, and call `__edda_panic(ptr, len)` so the panic prints
    // its message and aborts at runtime instead of being a silent
    // `unreachable` (UB). The `unreachable` after the call keeps the
    // block well-formed — `__edda_panic` is `-> !`, so it never returns.
    if let Some(msg_val) = lower_operand(msg, cx)? {
        let struct_val = msg_val.into_struct_value();
        let msg_ptr = cx
            .builder
            .build_extract_value(struct_val, 0, "panic.msg.ptr")
            .expect("extract panic msg ptr from fat pointer")
            .into_pointer_value();
        let msg_len = cx
            .builder
            .build_extract_value(struct_val, 1, "panic.msg.len")
            .expect("extract panic msg len from fat pointer")
            .into_int_value();
        let panic_fn = cx.module.get_function("__edda_panic").unwrap_or_else(|| {
            let ptr_ty = cx.context.ptr_type(AddressSpace::default());
            let i64_ty = cx.context.i64_type();
            let fn_ty = cx
                .context
                .void_type()
                .fn_type(&[ptr_ty.into(), i64_ty.into()], false);
            cx.module.add_function("__edda_panic", fn_ty, None)
        });
        cx.builder
            .build_call(panic_fn, &[msg_ptr.into(), msg_len.into()], "panic.call")
            .expect("build __edda_panic call");
    }
    cx.builder
        .build_unreachable()
        .expect("build_unreachable inside a positioned block must succeed");
    Ok(())
}
