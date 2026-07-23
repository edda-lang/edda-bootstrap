//! The direct (`FuncRef::Body` / `FuncRef::Extern`) `Call` walker plus
//! callee resolution. Resolves the callee to an inkwell
//! `FunctionValue`, builds the capability-then-user arg list with Win64
//! `byval`/`sret` translation, emits the `call`, stores the return into
//! `destination`, and handles the `?`-propagation success-vs-error split.

use edda_mir::{CallArg, CallMode, FuncRef, Operand, Place, ThreadedCapability};
use edda_target::Arch;
use inkwell::IntPredicate;
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::types::{AnyType, BasicType, BasicTypeEnum};
use inkwell::values::{
    BasicMetadataValueEnum, BasicValue, BasicValueEnum, FunctionValue, PointerValue,
};

use edda_mir::Idx;
use crate::error::CompileError;
use crate::lower::operand::lower_operand;
use crate::lower::place::lower_place_pointer;
use crate::lower::ty::inkwell_basic_type;
use crate::lower::LowerCtx;

use super::abi::{
    AGGREGATE_COPY_ALIGN, AllocFamilyRaw, RawMemInline, aggregate_operand_source_ptr,
    callee_return_ty, coerce_to_param_type, is_slice_addr_extern, mir_type_of_operand,
    use_indirect_abi, win64_small_aggregate_ret_size,
};
use super::args::{is_capability_operand, load_capability_value};
use super::extern_decl::declare_extern;
use super::indirect::lower_indirect_call;

/// Lower a `Call` terminator: resolve the callee, build the
/// capability-then-user arg list, emit `call`, store the return value
/// into `destination` (skipping the store when the callee returns
/// void), and branch to `target`. When `on_error: Some(block)` is
/// present, lower the success vs. error split per `?`-propagation
/// semantics in `backend-choice.md` §6.2: the callee returns a
/// `Result`-shaped sum, the walker inspects the tag, and conditionally
/// branches.
#[allow(clippy::too_many_arguments)]
pub(crate) fn lower_call<'ctx>(
    func: &FuncRef,
    args: &[CallArg],
    capabilities: &[ThreadedCapability],
    destination: &Place,
    target: edda_mir::BlockId,
    on_error: Option<edda_mir::BlockId>,
    llvm_blocks: &[inkwell::basic_block::BasicBlock<'ctx>],
    cx: &LowerCtx<'ctx, '_>,
) -> Result<(), CompileError> {
    // Indirect calls go through their own lowering path — the callee
    // is a fn-pointer operand, not a `FunctionValue`. The Win64 ABI
    // (sret / byval) and capability threading at indirect-call sites
    // are deferred; the current probe uses the simple non-aggregate
    // path.
    if let FuncRef::Indirect { callee, sig } = func {
        return lower_indirect_call(
            callee,
            sig,
            args,
            capabilities,
            destination,
            target,
            on_error,
            llvm_blocks,
            cx,
        );
    }
    // Wasm has no `edda-rt` to link, so the `__edda_slice_addr` raw extern
    // would otherwise lower to an unresolvable `env.__edda_slice_addr` host
    // import (native targets resolve the symbol at link time). Lower it
    // inline before `resolve_callee` ever declares the extern.
    if matches!(cx.arch, Arch::Wasm32 | Arch::Wasm64) && is_slice_addr_extern(func, cx) {
        return lower_slice_addr_wasm_inline(
            args,
            destination,
            target,
            on_error,
            llvm_blocks,
            cx,
        );
    }
    // Raw peek/poke externs lower inline on every target — a direct
    // `inttoptr` + align-1 `load`/`store` replaces the C-ABI call into
    // `edda-rt`, matching the native compiler's inline lowering.
    // The self-hosted allocator
    // (`std.mem.rt` / `std.mem.suballoc`) executes these on every
    // free-list step and every copied/zeroed byte, so at call-lowered
    // cost they dominate T1 build throughput.
    if let Some(access) = RawMemInline::from_func(func, cx) {
        return lower_raw_mem_inline(
            access,
            args,
            destination,
            target,
            on_error,
            llvm_blocks,
            cx,
        );
    }
    let fn_value = resolve_callee(func, cx)?;
    let alloc_family = AllocFamilyRaw::from_func(func, cx);

    // Win64 ABI: aggregates whose natural size is not 1/2/4/8 bytes
    // cross every call boundary via hidden pointer (applies uniformly
    // to extern and source-bodied callees so both sides of the call
    // agree on the wire shape — without this LLVM splits the struct
    // return into per-field registers and silently drops cross-field
    // stores the callee wrote into the alloca):
    //
    // - Return type: caller-allocated slot passed as the first arg
    //   with `sret`; callee returns void.
    // - Param type: caller-allocated copy passed as a `ptr` with
    //   `byval`; callee receives a pointer.
    //
    // Alloc-family raw externs short-circuit the sret path: their
    // return wire shape is patched at `declare_extern` to either
    // `void` (BoxRead / BoxUnbox — value comes back via the appended
    // out-pointer) or `Result<Box, AllocError>` directly (Alloc).
    let callee_ret_ty = callee_return_ty(func, cx);
    let force_void_return = matches!(
        alloc_family,
        Some(AllocFamilyRaw::BoxRead) | Some(AllocFamilyRaw::BoxUnbox)
    );
    let use_sret = !force_void_return
        && callee_ret_ty.as_ref().is_some_and(|t| use_indirect_abi(t, cx));
    // Extern small-aggregate returns are declared as iN; the
    // call result is then an IntValue whose bytes are stored through
    // the destination pointer (little-endian: byte 0 = tag). The
    // on_error tag check must read the low byte instead of doing an
    // extractvalue on a struct that no longer exists.
    let small_agg_ret = if matches!(func, FuncRef::Extern { .. }) && !force_void_return {
        callee_ret_ty
            .as_ref()
            .and_then(|t| win64_small_aggregate_ret_size(t, cx))
    } else {
        None
    };

    let mut llvm_args: Vec<BasicMetadataValueEnum<'ctx>> = Vec::new();
    let mut sret_slot: Option<(PointerValue<'ctx>, BasicTypeEnum<'ctx>)> = None;
    let mut byval_indices: Vec<(u32, BasicTypeEnum<'ctx>)> = Vec::new();

    // Position 0: sret slot if indirect return.
    if use_sret {
        let ret_ty = callee_ret_ty.as_ref().unwrap();
        let ret_llvm_ty = inkwell_basic_type(cx.context, ret_ty, cx.program, cx.arch)
            .ok_or_else(|| CompileError::UnsupportedMirShape {
                shape: "non-lowerable-sret-type",
                detail: format!(
                    "body {:?} calls extern with sret return type {:?} which is not lowerable",
                    cx.body_name, ret_ty.kind
                ),
            })?;
        let slot = cx.build_entry_alloca(ret_llvm_ty, "sret.tmp");
        sret_slot = Some((slot, ret_llvm_ty));
        llvm_args.push(slot.into());
    }

    // Capabilities — one opaque ptr per slot, materialised from the
    // paired positional operand when the MIR emitter recorded one
    // (the value dataflow), else from
    // the caller's EffectId-keyed capability alloca. Position depends
    // on whether sret occupies 0.
    for cap in capabilities {
        let cap_value = load_capability_value(cap, args, cx)?;
        llvm_args.push(cap_value.into());
    }

    // User args — with win64 indirect translation for aggregate args
    // on extern calls. The alloc-family raw value slot
    // (`__edda_alloc_raw` param index 2) is special: the runtime
    // signature is `value_ptr: *const u8`, so we always alloca a temp,
    // memcpy the value into it, and pass the temp's address — never a
    // byval attribute, never a value pass.
    //
    // For aggregate operands (sums in particular) we route through
    // `build_memcpy(<tmp>, <source_ptr>, sizeof)` rather than the
    // typed `load <agg>; store <agg>` round trip — the latter
    // decomposes into per-field operations that silently drop bytes
    // of a smaller variant living inside a larger variant's padding
    // (e.g. `num(f64)` payload inside an `{ i8, ptr, ptr }`-shaped
    // sum slot loses bytes 1..7 of the double).
    for (index, arg) in args.iter().enumerate() {
        if is_capability_operand(&arg.operand, cx) {
            continue;
        }
        // `mutable` / `init` args: pass a pointer to the caller's slot
        // rather than the loaded value, so writes the callee performs
        // through the param are observed by the caller (B19). For `init`
        // the callee initialises the caller's `uninit` binding in place
        // through this pointer — a copy would init a discarded temp and
        // leave the caller's binding uninitialised.
        // The arg operand is a Move/Copy
        // of the caller's place; recover its address via
        // `lower_place_pointer`. This bypasses both the win64-byval
        // path (which would pass a pointer-to-caller-owned-copy) and
        // the direct-by-value path (which loses writes entirely).
        if matches!(arg.mode, CallMode::Mutable | CallMode::Init) {
            let place = match &arg.operand {
                Operand::Copy(p) | Operand::Move(p) => p,
                _ => {
                    return Err(CompileError::UnsupportedMirShape {
                        shape: "inout-arg-non-place-operand",
                        detail: format!(
                            "body {:?} passes a non-Place operand at call arg #{index} \
                             with `mutable`/`init` mode; such arguments must originate \
                             from a named place",
                            cx.body_name
                        ),
                    });
                }
            };
            let arg_ptr = lower_place_pointer(place, cx)?;
            llvm_args.push(arg_ptr.into());
            continue;
        }
        let _ = arg.mode; // TODO: attribute mapping for remaining modes, not yet implemented.
        let arg_mir_ty = mir_type_of_operand(&arg.operand, cx);
        let is_alloc_value_slot =
            matches!(alloc_family, Some(AllocFamilyRaw::Alloc)) && index == 2;
        let win64_aggregate =
            arg_mir_ty.as_ref().is_some_and(|t| use_indirect_abi(t, cx));

        if is_alloc_value_slot || win64_aggregate {
            // Aggregate path — produce a stack temp + pointer.
            let ty = arg_mir_ty.expect("indirect arg has MIR type");
            let llvm_ty = inkwell_basic_type(cx.context, &ty, cx.program, cx.arch)
                .ok_or_else(|| CompileError::UnsupportedMirShape {
                    shape: if is_alloc_value_slot {
                        "alloc-value-arg-non-lowerable"
                    } else {
                        "byval-arg-non-lowerable"
                    },
                    detail: format!(
                        "body {:?} passes a value of type {:?} into call arg #{index} \
                         which does not lower to an LLVM basic type",
                        cx.body_name, ty.kind
                    ),
                })?;
            let tmp_name = if is_alloc_value_slot { "alloc.value.tmp" } else { "byval.tmp" };
            let tmp = cx.build_entry_alloca(llvm_ty, tmp_name);
            // Prefer byte-faithful memcpy from the source alloca; fall
            // back to typed load+store only when the operand is a
            // Const, Unit, or a projected place whose source pointer
            // we don't expose here. (The Const fallback is sound
            // because constants are not currently sum-typed.)
            if let Some(src_ptr) = aggregate_operand_source_ptr(&arg.operand, cx) {
                let size_val = llvm_ty
                    .size_of()
                    .expect("aggregate arg type has a sizeof");
                cx.builder
                    .build_memcpy(
                        tmp,
                        AGGREGATE_COPY_ALIGN,
                        src_ptr,
                        AGGREGATE_COPY_ALIGN,
                        size_val,
                    )
                    .expect("build_memcpy from source into aggregate-arg temp");
            } else {
                let value = lower_operand(&arg.operand, cx)?.ok_or_else(|| {
                    CompileError::UnsupportedMirShape {
                        shape: "call-arg-unit",
                        detail: format!(
                            "body {:?} passes a Unit operand at call arg #{index}; \
                             Unit-typed parameters are not supported at the wire ABI",
                            cx.body_name
                        ),
                    }
                })?;
                cx.builder
                    .build_store(tmp, value)
                    .expect("build_store of non-place aggregate arg into temp");
            }
            if win64_aggregate && !is_alloc_value_slot {
                let param_idx = llvm_args.len() as u32;
                byval_indices.push((param_idx, llvm_ty));
            }
            llvm_args.push(tmp.into());
        } else {
            // A Unit-typed arg is zero-sized and contributes no LLVM
            // value or ABI slot —
            // `lower_fn_sig` drops the matching callee param the same
            // way, so the two sides stay arity-consistent.
            if let Some(value) = lower_operand(&arg.operand, cx)? {
                llvm_args.push(value.into());
            }
        }
    }

    // Alloc-family BoxRead / BoxUnbox: append the destination's
    // pointer as the trailing out-value slot. The runtime writes the
    // `T` bytes through this pointer, so the destination must be a
    // real alloca whose pointer we can hand over.
    if force_void_return {
        let dest_ptr = lower_place_pointer(destination, cx)?;
        llvm_args.push(dest_ptr.into());
    }

    // Retype register-compatible args to the resolved callee's declared
    // parameter types. A module-defined `@abi` body (the self-hosted
    // `std.mem.rt` allocator) lowers its natural all-scalar signature,
    // while the alloc-family seam builds runtime-wire-shaped args
    // (`{ ptr }` box slots, `ptr` out slots); without coercion the call
    // is malformed IR that only works by register coincidence and that
    // the `default<O2>` inliner merges into an isel crash.
    let param_types = fn_value.get_type().get_param_types();
    if param_types.len() == llvm_args.len() {
        for (arg, param_ty) in llvm_args.iter_mut().zip(param_types.iter()) {
            *arg = coerce_to_param_type(*arg, param_ty, cx);
        }
    }
    let call_site = cx
        .builder
        .build_call(fn_value, &llvm_args, "call")
        .expect("build_call inside a positioned block must succeed");

    // Apply sret attribute on the first param when indirect return.
    if let Some((_, ret_llvm_ty)) = sret_slot {
        let kind_id = Attribute::get_named_enum_kind_id("sret");
        let attr = cx
            .context
            .create_type_attribute(kind_id, ret_llvm_ty.as_any_type_enum());
        call_site.add_attribute(AttributeLoc::Param(0), attr);
    }
    // Apply byval attributes on indirect-passed aggregate args.
    if !byval_indices.is_empty() {
        let kind_id = Attribute::get_named_enum_kind_id("byval");
        for (idx, ty) in &byval_indices {
            let attr = cx
                .context
                .create_type_attribute(kind_id, ty.as_any_type_enum());
            call_site.add_attribute(AttributeLoc::Param(*idx), attr);
        }
    }

    // Non-void calls write their result into `destination`. For the
    // sret path we memcpy from the caller-owned slot to the
    // destination — a load + store would decompose the aggregate
    // into per-field scalars (e.g. `{ i8, { { i8, { i64 } } } }`
    // becomes 3 loads/stores at offsets 0/8/16) and drop padding
    // bytes. When the variant payload is a smaller type whose bytes
    // overlap with padding (e.g. `Ok(double)` writes 8 bytes at
    // offset 8 inside a `{ i8, { i64 } }`-shaped payload), the
    // decomposed copy loses the high 7 bytes of the double. Memcpy
    // preserves the full byte range. The on-error tag check below
    // also reads the tag directly out of the sret slot for the
    // same reason.
    //
    // Alloc-family BoxRead / BoxUnbox already wrote through the
    // out-pointer; nothing further to do for the destination.
    let sret_tag: Option<inkwell::values::IntValue<'ctx>> = if force_void_return {
        None
    } else if let Some((slot, ret_ty)) = sret_slot {
        let dest_ptr = lower_place_pointer(destination, cx)?;
        let size_val = ret_ty
            .size_of()
            .expect("aggregate sret type has a sizeof");
        cx.builder
            .build_memcpy(dest_ptr, AGGREGATE_COPY_ALIGN, slot, AGGREGATE_COPY_ALIGN, size_val)
            .expect("build_memcpy from sret slot to destination");
        // Pre-extract the tag from the sret slot for the on_error
        // path so we don't need to re-load the aggregate.
        if on_error.is_some() {
            let i8_ty = cx.context.i8_type();
            let tag_val = cx
                .builder
                .build_load(i8_ty, slot, "call.tag")
                .expect("build_load of tag byte from sret slot")
                .into_int_value();
            Some(tag_val)
        } else {
            None
        }
    } else {
        let result_value: Option<BasicValueEnum<'ctx>> = call_site.try_as_basic_value().left();
        if let Some(value) = result_value {
            let dest_ptr = lower_place_pointer(destination, cx)?;
            cx.builder
                .build_store(dest_ptr, value)
                .expect("build_store of call result into destination");
        }
        None
    };
    let result_value: Option<BasicValueEnum<'ctx>> =
        if sret_slot.is_some() || force_void_return {
            None
        } else {
            call_site.try_as_basic_value().left()
        };

    match on_error {
        None => {
            let bb = llvm_blocks[target.index()];
            cx.builder
                .build_unconditional_branch(bb)
                .expect("build_unconditional_branch after Call must succeed");
        }
        Some(error_block) => {
            // `?` propagation: destination holds a Result-shaped sum
            // `{ tag, payload }`. Inspect the tag — branch to target
            // when tag == 0 (Ok by convention), else to error_block.
            // For sret returns we already pre-loaded the tag from
            // the sret slot (the aggregate-copy went via memcpy so
            // there's no SSA struct to extract from); otherwise the
            // tag comes from `extractvalue` on the call result.
            let tag_val = if let Some(t) = sret_tag {
                t
            } else if small_agg_ret.is_some() {
                // iN-coerced extern return: the tag is the low byte
                // (offset 0, little-endian) of the packed integer.
                let int_val = result_value
                    .ok_or_else(|| CompileError::UnsupportedMirShape {
                        shape: "call-on-error-void-return",
                        detail: format!(
                            "body {:?} has Call with on_error: Some(_) but the iN-coerced \
                             extern returned no value",
                            cx.body_name
                        ),
                    })?
                    .into_int_value();
                if int_val.get_type().get_bit_width() > 8 {
                    cx.builder
                        .build_int_truncate(int_val, cx.context.i8_type(), "call.tag")
                        .expect("trunc iN extern return to tag byte")
                } else {
                    int_val
                }
            } else {
                let result_val = result_value.ok_or_else(|| CompileError::UnsupportedMirShape {
                    shape: "call-on-error-void-return",
                    detail: format!(
                        "body {:?} has Call with on_error: Some(_) but the callee \
                         returns void; `?` propagation requires a Result-shaped return",
                        cx.body_name
                    ),
                })?;
                let sum_struct = match result_val {
                    BasicValueEnum::StructValue(s) => s,
                    _ => {
                        return Err(CompileError::UnsupportedMirShape {
                            shape: "call-on-error-non-sum-return",
                            detail: format!(
                                "body {:?} has Call with on_error: Some(_) but the callee \
                                 return value is not a struct; `?` propagation requires a \
                                 sum-typed (Result-shaped) return",
                                cx.body_name
                            ),
                        });
                    }
                };
                cx.builder
                    .build_extract_value(sum_struct, 0, "call.tag")
                    .expect("extract sum.tag at index 0")
                    .into_int_value()
            };
            let zero = tag_val.get_type().const_zero();
            let is_ok = cx
                .builder
                .build_int_compare(IntPredicate::EQ, tag_val, zero, "call.is_ok")
                .expect("icmp eq tag, 0");
            let target_bb = llvm_blocks[target.index()];
            let error_bb = llvm_blocks[error_block.index()];
            cx.builder
                .build_conditional_branch(is_ok, target_bb, error_bb)
                .expect("conditional branch after `?`-propagating Call");
        }
    }
    Ok(())
}

/// Inline-lower the `__edda_slice_addr` raw extern on the Wasm target,
/// storing the slice's base address into `destination` and branching to
/// `target`. Gated by [`is_slice_addr_extern`] + a Wasm-arch check in
/// [`lower_call`]; native targets keep resolving the symbol through
/// `edda-rt` at link time.
fn lower_slice_addr_wasm_inline<'ctx>(
    args: &[CallArg],
    destination: &Place,
    target: edda_mir::BlockId,
    on_error: Option<edda_mir::BlockId>,
    llvm_blocks: &[inkwell::basic_block::BasicBlock<'ctx>],
    cx: &LowerCtx<'ctx, '_>,
) -> Result<(), CompileError> {
    if on_error.is_some() {
        return Err(CompileError::UnsupportedMirShape {
            shape: "slice-addr-on-error",
            detail: format!(
                "body {:?} calls __edda_slice_addr with an on_error edge; the \
                 intrinsic returns u64 and cannot propagate errors",
                cx.body_name
            ),
        });
    }
    let slice_operand = args
        .iter()
        .map(|a| &a.operand)
        .find(|&op| !is_capability_operand(op, cx))
        .ok_or_else(|| CompileError::UnsupportedMirShape {
            shape: "slice-addr-missing-arg",
            detail: format!(
                "body {:?} calls __edda_slice_addr with no slice argument",
                cx.body_name
            ),
        })?;
    let slice_value = lower_operand(slice_operand, cx)?.ok_or_else(|| {
        CompileError::UnsupportedMirShape {
            shape: "slice-addr-unit-arg",
            detail: format!(
                "body {:?} calls __edda_slice_addr with a Unit operand; expected a slice",
                cx.body_name
            ),
        }
    })?;
    let slice_struct = match slice_value {
        BasicValueEnum::StructValue(s) => s,
        _ => {
            return Err(CompileError::UnsupportedMirShape {
                shape: "slice-addr-non-aggregate-arg",
                detail: format!(
                    "body {:?} calls __edda_slice_addr but the argument did not lower \
                     to a {{ ptr, len }} fat pointer",
                    cx.body_name
                ),
            });
        }
    };
    let ptr_field = cx
        .builder
        .build_extract_value(slice_struct, 0, "slice.ptr")
        .expect("extractvalue slice.ptr at index 0 in a positioned block")
        .into_pointer_value();
    let addr = cx
        .builder
        .build_ptr_to_int(ptr_field, cx.context.i64_type(), "slice.addr")
        .expect("ptrtoint slice.ptr to i64 in a positioned block");
    let dest_ptr = lower_place_pointer(destination, cx)?;
    cx.builder
        .build_store(dest_ptr, addr)
        .expect("store inline slice.addr into destination");
    let bb = llvm_blocks[target.index()];
    cx.builder
        .build_unconditional_branch(bb)
        .expect("branch to target after inline slice_addr");
    Ok(())
}

/// Inline-lower a `std.os.raw` load/store extern (`__edda_load_*` /
/// `__edda_store_*`): `inttoptr` the address and emit a direct align-1
/// memory access, then branch to `target`. Gated by
/// [`RawMemInline::from_func`] in [`lower_call`]; applies on every
/// target so bootstrap-compiled binaries match the native compiler's
/// inline semantics instead of paying a C-ABI call per accessed byte
/// or word.
fn lower_raw_mem_inline<'ctx>(
    access: RawMemInline,
    args: &[CallArg],
    destination: &Place,
    target: edda_mir::BlockId,
    on_error: Option<edda_mir::BlockId>,
    llvm_blocks: &[inkwell::basic_block::BasicBlock<'ctx>],
    cx: &LowerCtx<'ctx, '_>,
) -> Result<(), CompileError> {
    if on_error.is_some() {
        return Err(CompileError::UnsupportedMirShape {
            shape: "raw-mem-on-error",
            detail: format!(
                "body {:?} calls a raw load/store extern with an on_error edge; \
                 the intrinsic is non-fallible",
                cx.body_name
            ),
        });
    }
    let mut user_args = args
        .iter()
        .map(|a| &a.operand)
        .filter(|op| !is_capability_operand(op, cx));
    let addr_operand =
        user_args
            .next()
            .ok_or_else(|| CompileError::UnsupportedMirShape {
                shape: "raw-mem-missing-addr",
                detail: format!(
                    "body {:?} calls a raw load/store extern with no address argument",
                    cx.body_name
                ),
            })?;
    let addr_value = lower_operand(addr_operand, cx)?
        .ok_or_else(|| CompileError::UnsupportedMirShape {
            shape: "raw-mem-unit-addr",
            detail: format!(
                "body {:?} passes a Unit operand as a raw load/store address",
                cx.body_name
            ),
        })?
        .into_int_value();
    let ptr = cx
        .builder
        .build_int_to_ptr(
            addr_value,
            cx.context.ptr_type(inkwell::AddressSpace::default()),
            "raw.addr",
        )
        .expect("inttoptr of raw address in a positioned block");
    match access {
        RawMemInline::Load(bits) => {
            let int_ty = cx.context.custom_width_int_type(bits);
            let loaded = cx
                .builder
                .build_load(int_ty, ptr, "raw.load")
                .expect("raw load in a positioned block");
            loaded
                .as_instruction_value()
                .expect("a load produces an instruction value")
                .set_alignment(1)
                .expect("set align 1 on raw load");
            let dest_ptr = lower_place_pointer(destination, cx)?;
            cx.builder
                .build_store(dest_ptr, loaded)
                .expect("store raw load result into destination");
        }
        RawMemInline::Store(_) => {
            let value_operand =
                user_args
                    .next()
                    .ok_or_else(|| CompileError::UnsupportedMirShape {
                        shape: "raw-mem-missing-value",
                        detail: format!(
                            "body {:?} calls a raw store extern with no value argument",
                            cx.body_name
                        ),
                    })?;
            let value = lower_operand(value_operand, cx)?.ok_or_else(|| {
                CompileError::UnsupportedMirShape {
                    shape: "raw-mem-unit-value",
                    detail: format!(
                        "body {:?} passes a Unit operand as a raw store value",
                        cx.body_name
                    ),
                }
            })?;
            let store = cx
                .builder
                .build_store(ptr, value)
                .expect("raw store in a positioned block");
            store
                .set_alignment(1)
                .expect("set align 1 on raw store");
        }
    }
    let bb = llvm_blocks[target.index()];
    cx.builder
        .build_unconditional_branch(bb)
        .expect("branch to target after inline raw access");
    Ok(())
}

/// Resolve a [`FuncRef`] to its inkwell [`FunctionValue`]. Program-
/// defined bodies are looked up by name (the declare pass populated
/// the module); externs are declared lazily and idempotently.
fn resolve_callee<'ctx>(
    func: &FuncRef,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<FunctionValue<'ctx>, CompileError> {
    match func {
        FuncRef::Body(id) => {
            let callee_body = cx.program.bodies.get(*id).ok_or_else(|| {
                CompileError::UnsupportedMirShape {
                    shape: "call-target-out-of-range",
                    detail: format!(
                        "body {:?} calls BodyId#{} which is not in program.bodies",
                        cx.body_name,
                        id.as_u32()
                    ),
                }
            })?;
            let callee_name = cx
                .symbol_names
                .get(id.index())
                .map(|s| s.as_str())
                .unwrap_or_else(|| cx.interner.resolve(callee_body.name));
            cx.module.get_function(callee_name).ok_or_else(|| {
                CompileError::UnsupportedMirShape {
                    shape: "call-target-not-declared",
                    detail: format!(
                        "body {:?} calls body {callee_name:?} but the program declare pass \
                         did not register it (lower_program must declare every body before \
                         walking instruction streams)",
                        cx.body_name
                    ),
                }
            })
        }
        FuncRef::Extern { name, sig } => {
            let extern_name = cx.interner.resolve(*name);
            if let Some(existing) = cx.module.get_function(extern_name) {
                return Ok(existing);
            }
            declare_extern(extern_name, sig, cx)
        }
        // FuncRef::Indirect is handled in `lower_call`'s prologue and
        // never reaches `resolve_callee`. The arm exists only to keep
        // the match exhaustive.
        FuncRef::Indirect { .. } => unreachable!(
            "FuncRef::Indirect is dispatched in lower_call before resolve_callee runs"
        ),
    }
}
