//! `scope(exec)` `group.spawn { ... }` / `.await` LLVM lowering.
//!
//! `edda-mir` already lowers `__edda_task_group_open` /
//! `__edda_task_group_join` as ordinary `TerminatorKind::Call` against
//! `FuncRef::Extern` (`crates/edda-mir/src/lower/spawn/mod.rs`), so the
//! existing `terminator::call` walker handles those with no new code
//! here. Only `TerminatorKind::Spawn` / `TerminatorKind::Await` are new.
//!
//! [`lower_spawn`] packs `args` into a byte buffer at each param's
//! natural (size, align) offset — a private wire format between this
//! function and the synthesised shim, never an extern ABI boundary, so
//! it carries none of the win64/byval concerns below. It resolves
//! `child`'s already-declared [`FunctionValue`] (the program's declare
//! pass registers every body before any body's instructions are
//! walked), synthesises a `TaskBodyFn`-shaped shim (`ptr, usize -> ptr`,
//! [`build_spawn_shim`]) that unpacks the buffer and calls `child` with
//! the win64/byval/sret ABI [`super::super::fn_sig::lower_fn_sig`]
//! already declared for it, then calls `__edda_task_spawn(shim, buf,
//! buf_len, group)` and stores the returned handle into `dest`.
//!
//! [`lower_await`] calls `__edda_task_await(task)` and reads the task's
//! real semantic result type out of the returned pointer (`memcpy` for
//! aggregates — preserving sum-type padding bytes a typed load/store
//! round trip can drop; `load` for scalars/capabilities) — per
//! [`edda_mir::TerminatorKind::Await`]'s invariant that `dest` is never
//! the raw pointer type `__edda_task_await` returns.

use edda_mir::{
    Body, BlockId, BodyId, Idx, LocalId, MirPrim, MirProgram, MirType, MirTypeKind, Operand,
    ParamMode, Place,
};
use edda_target::Arch;
use inkwell::AddressSpace;
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::module::Module;
use inkwell::types::{AnyType, BasicType, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, FunctionValue, PointerValue};

use crate::error::CompileError;
use crate::lower::LowerCtx;
use crate::lower::body::win64_indirect_aggregate;
use crate::lower::operand::lower_operand;
use crate::lower::place::{lower_place_pointer, lower_place_read};
use crate::lower::ty::{compute_type_align, compute_type_size, inkwell_basic_type, inkwell_prim_type};

const TASK_SPAWN_SYMBOL: &str = "__edda_task_spawn";
const TASK_AWAIT_SYMBOL: &str = "__edda_task_await";
const MALLOC_SYMBOL: &str = "malloc";

const PACKED_ALIGN: u32 = 8;

/// Whether `ty` is one of the aggregate kinds that must be copied
/// byte-for-byte (`memcpy`) rather than round-tripped through a typed
/// `load`/`store`, which can drop a sum type's inter-variant padding
/// bytes.
fn is_aggregate(ty: &MirType) -> bool {
    matches!(
        &ty.kind,
        MirTypeKind::Prim(MirPrim::Str)
            | MirTypeKind::Slice(_)
            | MirTypeKind::Tuple(_)
            | MirTypeKind::Adt(_)
            | MirTypeKind::FnPtr(_)
    )
}

/// One child-body param's byte offset in the packed argument buffer.
struct PackedParam {
    offset: u32,
}

/// Lay out `child`'s params sequentially in a private (packer/unpacker
/// only) byte buffer: each param at its own natural (size, align).
/// Returns the per-param offsets plus the buffer's total byte size.
fn packed_layout(child: &Body, program: &MirProgram, arch: Arch) -> (Vec<PackedParam>, u32) {
    let mut offset: u32 = 0;
    let mut slots = Vec::with_capacity(child.params.len());
    for param in &child.params {
        let align = compute_type_align(&param.ty, program, arch).max(1);
        offset = round_up(offset, align);
        slots.push(PackedParam { offset });
        offset = offset.saturating_add(compute_type_size(&param.ty, program, arch));
    }
    (slots, offset)
}

fn round_up(n: u32, align: u32) -> u32 {
    if align <= 1 {
        return n;
    }
    n.saturating_add(align - 1) / align * align
}

/// Source-pointer probe for aggregate operands, mirroring
/// `terminator::call::abi::aggregate_operand_source_ptr` (private to
/// that module, so re-derived here): lets aggregate packing/unpacking
/// prefer a byte-faithful `memcpy` over a typed load/store round trip.
fn aggregate_operand_source_ptr<'ctx>(
    operand: &Operand,
    cx: &LowerCtx<'ctx, '_>,
) -> Option<PointerValue<'ctx>> {
    let place = match operand {
        Operand::Copy(p) | Operand::Move(p) => p,
        _ => return None,
    };
    if !place.projection.is_empty() {
        return None;
    }
    cx.locals[place.local.index()]
}

/// Build an inkwell `usize`-typed constant for `value`.
fn usize_const<'ctx>(cx: &LowerCtx<'ctx, '_>, value: u64) -> inkwell::values::IntValue<'ctx> {
    inkwell_prim_type(cx.context, MirPrim::Usize, cx.arch)
        .expect("usize is always lowerable")
        .into_int_type()
        .const_int(value, false)
}

/// Lazily declare `__edda_task_spawn(body_fn: ptr, args_ptr: ptr,
/// args_len: usize, group: ptr) -> ptr`, matching `edda-rt`'s
/// `__edda_task_spawn` extern C ABI.
fn declare_task_spawn<'ctx>(cx: &LowerCtx<'ctx, '_>) -> FunctionValue<'ctx> {
    if let Some(existing) = cx.module.get_function(TASK_SPAWN_SYMBOL) {
        return existing;
    }
    let ptr_ty = cx.context.ptr_type(AddressSpace::default());
    let usize_ty = inkwell_prim_type(cx.context, MirPrim::Usize, cx.arch)
        .expect("usize is always lowerable");
    let fn_ty = ptr_ty.fn_type(&[ptr_ty.into(), ptr_ty.into(), usize_ty.into(), ptr_ty.into()], false);
    cx.module.add_function(TASK_SPAWN_SYMBOL, fn_ty, None)
}

/// Lazily declare `__edda_task_await(handle: ptr) -> ptr`, matching
/// `edda-rt`'s `__edda_task_await` extern C ABI.
fn declare_task_await<'ctx>(cx: &LowerCtx<'ctx, '_>) -> FunctionValue<'ctx> {
    if let Some(existing) = cx.module.get_function(TASK_AWAIT_SYMBOL) {
        return existing;
    }
    let ptr_ty = cx.context.ptr_type(AddressSpace::default());
    let fn_ty = ptr_ty.fn_type(&[ptr_ty.into()], false);
    cx.module.add_function(TASK_AWAIT_SYMBOL, fn_ty, None)
}

/// Lazily declare the C runtime's `malloc(size: usize) -> ptr`. Used
/// only by a spawn shim's own heap-allocated result buffer — the value
/// a spawned task's `TaskBodyFn` must return outlives the shim's stack
/// frame, so it cannot be a local `alloca`.
fn declare_malloc<'ctx>(
    context: &'ctx inkwell::context::Context,
    module: &Module<'ctx>,
    arch: Arch,
) -> FunctionValue<'ctx> {
    if let Some(existing) = module.get_function(MALLOC_SYMBOL) {
        return existing;
    }
    let ptr_ty = context.ptr_type(AddressSpace::default());
    let usize_ty = inkwell_prim_type(context, MirPrim::Usize, arch).expect("usize is always lowerable");
    let fn_ty = ptr_ty.fn_type(&[usize_ty.into()], false);
    module.add_function(MALLOC_SYMBOL, fn_ty, None)
}

/// Store one packed-buffer argument: `memcpy` for aggregates (when a
/// source pointer is available), plain `store` for scalars/capabilities
/// and the aggregate fallback. `Unit`-typed operands store nothing (a
/// zero-size type has no bytes to write).
fn store_packed_arg<'ctx>(
    ty: &MirType,
    operand: &Operand,
    slot_ptr: PointerValue<'ctx>,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<(), CompileError> {
    if is_aggregate(ty)
        && let Some(src_ptr) = aggregate_operand_source_ptr(operand, cx)
    {
        let size = compute_type_size(ty, cx.program, cx.arch);
        let size_val = usize_const(cx, size as u64);
        cx.builder
            .build_memcpy(slot_ptr, PACKED_ALIGN, src_ptr, PACKED_ALIGN, size_val)
            .expect("memcpy of aggregate spawn arg into packed buffer");
        return Ok(());
    }
    let Some(value) = lower_operand(operand, cx)? else {
        return Ok(());
    };
    cx.builder
        .build_store(slot_ptr, value)
        .expect("store of spawn arg into packed buffer");
    Ok(())
}

/// Build (or reuse) the `TaskBodyFn`-shaped shim for `child`:
/// `ptr(args_ptr) -> ptr`. Unpacks the packed argument buffer per
/// [`packed_layout`], calls `child_fn`, heap-allocates the boxed
/// result, and returns it.
fn build_spawn_shim<'ctx>(
    child_body: &Body,
    child_fn: FunctionValue<'ctx>,
    shim_name: &str,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<FunctionValue<'ctx>, CompileError> {
    if let Some(existing) = cx.module.get_function(shim_name) {
        return Ok(existing);
    }
    let context = cx.context;
    let arch = cx.arch;
    let os = cx.os;
    let program = cx.program;

    let ptr_ty = context.ptr_type(AddressSpace::default());
    let usize_ty = inkwell_prim_type(context, MirPrim::Usize, arch).expect("usize is always lowerable");
    let shim_ty = ptr_ty.fn_type(&[ptr_ty.into(), usize_ty.into()], false);
    let shim_fn = cx.module.add_function(shim_name, shim_ty, None);

    let args_ptr = shim_fn
        .get_nth_param(0)
        .expect("shim declares 2 params")
        .into_pointer_value();

    let builder = context.create_builder();
    let entry_bb = context.append_basic_block(shim_fn, "entry");
    builder.position_at_end(entry_bb);

    let (slots, _total_size) = packed_layout(child_body, program, arch);

    let ret_ty = &child_body.return_ty;
    let ret_is_unit = matches!(ret_ty.kind, MirTypeKind::Unit | MirTypeKind::Never);
    let use_sret = !ret_is_unit && is_aggregate(ret_ty) && win64_indirect_aggregate(ret_ty, os, program, arch);

    let mut call_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::with_capacity(child_body.params.len() + 1);
    let mut sret_slot: Option<(PointerValue<'ctx>, BasicTypeEnum<'ctx>)> = None;
    if use_sret {
        let llvm_ret_ty = inkwell_basic_type(context, ret_ty, program, arch).ok_or_else(|| {
            CompileError::UnsupportedMirShape {
                shape: "spawn-shim-non-lowerable-return",
                detail: format!(
                    "spawn body {:?} returns {:?}, which has no LLVM lowering",
                    cx.interner.resolve(child_body.name),
                    ret_ty.kind
                ),
            }
        })?;
        let slot = builder
            .build_alloca(llvm_ret_ty, "shim.sret")
            .expect("alloca for shim sret slot must succeed");
        sret_slot = Some((slot, llvm_ret_ty));
        call_args.push(slot.into());
    }

    let mut byval_indices: Vec<(u32, BasicTypeEnum<'ctx>)> = Vec::new();
    for (param, slot) in child_body.params.iter().zip(slots.iter()) {
        if matches!(param.mode, ParamMode::Mutable | ParamMode::Init) {
            return Err(CompileError::UnsupportedMirShape {
                shape: "spawn-body-inout-param",
                detail: format!(
                    "spawn body {:?} declares a `mutable`/`init` param; CLAUDE.md \
                     forbids `mutable` crossing a spawn boundary — this is malformed MIR",
                    cx.interner.resolve(child_body.name)
                ),
            });
        }
        let offset_val = context.i64_type().const_int(slot.offset as u64, false);
        // Non-inbounds byte-offset GEP over an i8 element type — sound
        // because `offset` is computed by `packed_layout` against this
        // same buffer's own size, never externally supplied.
        let slot_ptr = unsafe {
            builder
                .build_gep(context.i8_type(), args_ptr, &[offset_val], "shim.arg.slot")
                .expect("gep into packed spawn-args buffer must succeed")
        };
        let llvm_ty = inkwell_basic_type(context, &param.ty, program, arch).ok_or_else(|| {
            CompileError::UnsupportedMirShape {
                shape: "spawn-shim-non-lowerable-param",
                detail: format!(
                    "spawn body {:?} has a param of type {:?}, which has no LLVM lowering",
                    cx.interner.resolve(child_body.name),
                    param.ty.kind
                ),
            }
        })?;
        if matches!(param.ty.kind, MirTypeKind::Capability(_)) {
            let value = builder
                .build_load(llvm_ty, slot_ptr, "shim.cap")
                .expect("load capability ptr from packed buffer must succeed");
            call_args.push(value.into());
        } else if win64_indirect_aggregate(&param.ty, os, program, arch) {
            let tmp = builder
                .build_alloca(llvm_ty, "shim.byval.tmp")
                .expect("alloca for shim byval temp must succeed");
            let size_val = llvm_ty.size_of().expect("aggregate param type has a sizeof");
            builder
                .build_memcpy(tmp, PACKED_ALIGN, slot_ptr, PACKED_ALIGN, size_val)
                .expect("memcpy of packed aggregate arg into byval temp must succeed");
            byval_indices.push((call_args.len() as u32, llvm_ty));
            call_args.push(tmp.into());
        } else {
            let value = builder
                .build_load(llvm_ty, slot_ptr, "shim.arg")
                .expect("load spawn arg from packed buffer must succeed");
            call_args.push(value.into());
        }
    }

    let call = builder
        .build_call(child_fn, &call_args, "shim.call")
        .expect("build_call to the spawned body must succeed");
    if let Some((_, sret_llvm_ty)) = sret_slot {
        let kind_id = Attribute::get_named_enum_kind_id("sret");
        let attr = context.create_type_attribute(kind_id, sret_llvm_ty.as_any_type_enum());
        call.add_attribute(AttributeLoc::Param(0), attr);
    }
    if !byval_indices.is_empty() {
        let kind_id = Attribute::get_named_enum_kind_id("byval");
        for (idx, ty) in &byval_indices {
            let attr = context.create_type_attribute(kind_id, ty.as_any_type_enum());
            call.add_attribute(AttributeLoc::Param(*idx), attr);
        }
    }

    let result_ptr = if ret_is_unit {
        ptr_ty.const_null()
    } else {
        let malloc_fn = declare_malloc(context, cx.module, arch);
        let result_size = compute_type_size(ret_ty, program, arch).max(1);
        let size_val = usize_const(cx, result_size as u64);
        let heap_ptr = builder
            .build_call(malloc_fn, &[size_val.into()], "shim.result.heap")
            .expect("build_call to malloc must succeed")
            .try_as_basic_value()
            .left()
            .expect("malloc returns a ptr")
            .into_pointer_value();
        if let Some((slot, sret_llvm_ty)) = sret_slot {
            let size_val = sret_llvm_ty.size_of().expect("aggregate return type has a sizeof");
            builder
                .build_memcpy(heap_ptr, PACKED_ALIGN, slot, PACKED_ALIGN, size_val)
                .expect("memcpy of shim sret result into heap buffer must succeed");
        } else {
            let value = call
                .try_as_basic_value()
                .left()
                .expect("a non-Unit/Never spawn body returns a basic value");
            builder
                .build_store(heap_ptr, value)
                .expect("store of shim result into heap buffer must succeed");
        }
        heap_ptr
    };
    builder
        .build_return(Some(&result_ptr))
        .expect("build_return of the shim result pointer must succeed");
    Ok(shim_fn)
}

/// Lower `TerminatorKind::Spawn`: pack `args` into a byte buffer, call
/// `__edda_task_spawn` with a shim synthesised for `child`, store the
/// returned handle into `dest`, and branch to `target`.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_spawn<'ctx>(
    child: BodyId,
    args: &[Operand],
    group_local: LocalId,
    dest: LocalId,
    target: BlockId,
    llvm_blocks: &[inkwell::basic_block::BasicBlock<'ctx>],
    cx: &LowerCtx<'ctx, '_>,
) -> Result<(), CompileError> {
    let child_body = cx.program.bodies.get(child).ok_or_else(|| CompileError::UnsupportedMirShape {
        shape: "spawn-target-out-of-range",
        detail: format!(
            "body {:?} spawns BodyId#{} which is not in program.bodies",
            cx.body_name,
            child.index()
        ),
    })?;
    let child_symbol = cx
        .symbol_names
        .get(child.index())
        .map(|s| s.as_str())
        .unwrap_or_else(|| cx.interner.resolve(child_body.name));
    let child_fn = cx.module.get_function(child_symbol).ok_or_else(|| CompileError::UnsupportedMirShape {
        shape: "spawn-target-not-declared",
        detail: format!(
            "body {:?} spawns body {child_symbol:?} but the program declare pass \
             did not register it (lower_program must declare every body before \
             walking instruction streams)",
            cx.body_name
        ),
    })?;

    let shim_name = format!("{child_symbol}.__task_shim");
    let shim_fn = build_spawn_shim(child_body, child_fn, &shim_name, cx)?;

    let (slots, total_size) = packed_layout(child_body, cx.program, cx.arch);
    let buf_ty = cx.context.i8_type().array_type(total_size);
    let buf_alloca = cx.build_entry_alloca(buf_ty, "spawn.args");
    let i8_ty = cx.context.i8_type();

    for ((param, slot), operand) in child_body.params.iter().zip(slots.iter()).zip(args.iter()) {
        let offset_val = cx.context.i64_type().const_int(slot.offset as u64, false);
        // Non-inbounds byte-offset GEP over an i8 element type — sound
        // because `offset` is computed by `packed_layout` against this
        // same buffer's own size, never externally supplied.
        let slot_ptr = unsafe {
            cx.builder
                .build_gep(i8_ty, buf_alloca, &[offset_val], "spawn.arg.slot")
                .expect("gep into packed spawn-args buffer must succeed")
        };
        store_packed_arg(&param.ty, operand, slot_ptr, cx)?;
    }

    let group_value = lower_place_read(&Place::local(group_local), cx)?.into_pointer_value();

    let task_spawn_fn = declare_task_spawn(cx);
    let total_size_val = usize_const(cx, total_size as u64);
    let spawn_call_args: [BasicMetadataValueEnum<'ctx>; 4] = [
        shim_fn.as_global_value().as_pointer_value().into(),
        buf_alloca.into(),
        total_size_val.into(),
        group_value.into(),
    ];
    let call = cx
        .builder
        .build_call(task_spawn_fn, &spawn_call_args, "spawn.call")
        .expect("build_call to __edda_task_spawn must succeed");
    let handle = call
        .try_as_basic_value()
        .left()
        .expect("__edda_task_spawn returns a ptr");

    let dest_ptr = lower_place_pointer(&Place::local(dest), cx)?;
    cx.builder
        .build_store(dest_ptr, handle)
        .expect("store of the spawned task handle into dest must succeed");

    let bb = llvm_blocks[target.index()];
    cx.builder
        .build_unconditional_branch(bb)
        .expect("build_unconditional_branch after Spawn must succeed");
    Ok(())
}

/// Lower `TerminatorKind::Await`: call `__edda_task_await(task)`, read
/// the result into `dest` at its real semantic type, and branch to
/// `target`.
pub(super) fn lower_await<'ctx>(
    task: &Operand,
    dest: LocalId,
    target: BlockId,
    llvm_blocks: &[inkwell::basic_block::BasicBlock<'ctx>],
    cx: &LowerCtx<'ctx, '_>,
) -> Result<(), CompileError> {
    let Some(task_value) = lower_operand(task, cx)? else {
        return Err(CompileError::UnsupportedMirShape {
            shape: "await-unit-task",
            detail: format!(
                "body {:?} awaits a Unit-typed task operand; a task handle is always \
                 HeapPtr-typed",
                cx.body_name
            ),
        });
    };
    let task_ptr = task_value.into_pointer_value();

    let task_await_fn = declare_task_await(cx);
    let call = cx
        .builder
        .build_call(task_await_fn, &[task_ptr.into()], "await.call")
        .expect("build_call to __edda_task_await must succeed");
    let raw_result = call
        .try_as_basic_value()
        .left()
        .expect("__edda_task_await returns a ptr")
        .into_pointer_value();

    let dest_decl = cx.body.locals.get(dest).ok_or_else(|| CompileError::UnsupportedMirShape {
        shape: "await-dest-local-missing",
        detail: format!(
            "body {:?} awaits into local #{}, which has no LocalDecl",
            cx.body_name,
            dest.index()
        ),
    })?;
    let dest_ty = dest_decl.ty.clone();

    if !matches!(dest_ty.kind, MirTypeKind::Unit | MirTypeKind::Never) {
        let llvm_ty = inkwell_basic_type(cx.context, &dest_ty, cx.program, cx.arch).ok_or_else(|| {
            CompileError::UnsupportedMirShape {
                shape: "await-non-lowerable-result",
                detail: format!(
                    "body {:?} awaits a task whose result type {:?} has no LLVM lowering",
                    cx.body_name, dest_ty.kind
                ),
            }
        })?;
        let dest_ptr = lower_place_pointer(&Place::local(dest), cx)?;
        if is_aggregate(&dest_ty) {
            let size = compute_type_size(&dest_ty, cx.program, cx.arch);
            let size_val = usize_const(cx, size as u64);
            cx.builder
                .build_memcpy(dest_ptr, PACKED_ALIGN, raw_result, PACKED_ALIGN, size_val)
                .expect("memcpy of the awaited task result into dest must succeed");
        } else {
            let value = cx
                .builder
                .build_load(llvm_ty, raw_result, "await.result")
                .expect("load of the awaited task result must succeed");
            cx.builder
                .build_store(dest_ptr, value)
                .expect("store of the awaited task result into dest must succeed");
        }
    }

    let bb = llvm_blocks[target.index()];
    cx.builder
        .build_unconditional_branch(bb)
        .expect("build_unconditional_branch after Await must succeed");
    Ok(())
}
