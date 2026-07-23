//! Entry-block prologue: per-local `alloca` + parameter store.
//!
//! Every local whose type lowers through [`inkwell_basic_type`] —
//! primitives, tuples, product ADTs, sum ADTs, slices, and fn-pointers
//! (as opaque `ptr`) — gets an `alloca` in the function's entry block.
//! Capability locals get an opaque-`ptr` alloca so the Call walker can
//! reload the capability handle when threading it into a callee.
//! Non-lowerable locals (`Unit`, `Never`, `Str`) receive `None` and any
//! `Copy`/`Move` of them is later rejected with
//! [`CompileError::UnsupportedMirShape`](crate::CompileError::UnsupportedMirShape).
//! Each [`ParamInfo::local`](edda_mir::ParamInfo) then receives the
//! corresponding inkwell function-argument value via `store`.
//!
//! On win64, aggregate user params >8 bytes (excluding 1/2/4/8) arrive
//! as `ptr byval(<ty>)` — the callee receives a pointer to a caller-
//! owned copy. We allocate the local alloca normally and load the value
//! through the byval pointer into it (matches `lower_fn_sig`'s
//! sret/byval translation). The return slot ([`LocalId::RETURN_SLOT`])
//! gets an alloca only when the return is direct; when the function
//! uses sret, locals[0] is the sret pointer itself so every
//! `_0 = <value>` assignment writes through to the caller's slot and
//! the Return terminator emits `ret void`.
//!
//! Called by [`super::body::lower_body`] with the builder positioned
//! at the start of the entry block.

use edda_mir::{Body, Idx, LocalId, MirProgram, MirTypeKind, ParamMode};
use edda_target::{Arch, Os};
use inkwell::AddressSpace;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::types::BasicType;
use inkwell::values::{FunctionValue, PointerValue};

use super::body::win64_indirect_aggregate;
use super::ty::inkwell_basic_type;

/// Conservative alignment used for the `build_memcpy` paths that
/// preserve cross-variant bytes on aggregate copies. Matches the
/// constant of the same name in `terminator/call.rs` and `statement.rs`.
const AGGREGATE_COPY_ALIGN: u32 = 8;

/// Allocate every lowerable-typed local in the entry block and store
/// incoming arguments into their parameter slots.
pub(crate) fn allocate_locals<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    body: &edda_mir::Body,
    program: &MirProgram,
    function: FunctionValue<'ctx>,
    arch: Arch,
    os: Os,
) -> Vec<Option<PointerValue<'ctx>>> {
    // On win64 the return slot may be the sret pointer rather than a
    // local alloca; track that here so the alloca loop skips it and the
    // per-param iteration accounts for the prepended sret arg.
    let use_sret = win64_indirect_aggregate(&body.return_ty, os, program, arch);
    let sret_offset = if use_sret { 1 } else { 0 };

    // `mutable` and `init` user params skip their own alloca: their
    // local slot is the incoming `ptr` arg itself (a pointer to the
    // caller's slot), threaded into `locals[local_idx]` in the param-
    // store loop below. For `init` this is what makes the callee
    // initialise the caller's `uninit` binding in place rather than a
    // private copy. Collect the set of
    // skipped local indices up-front so the per-local alloca loop can
    // recognise them.
    let by_pointer_param_locals: std::collections::HashSet<usize> = body
        .params
        .iter()
        .filter(|p| matches!(p.mode, ParamMode::Mutable | ParamMode::Init)
            && !matches!(p.ty.kind, MirTypeKind::Capability(_)))
        .map(|p| p.local.index())
        .collect();

    let return_slot_index = LocalId::RETURN_SLOT.index();
    let mut locals: Vec<Option<PointerValue<'ctx>>> = Vec::with_capacity(body.locals.len());
    for (local_idx, decl) in body.locals.iter_enumerated() {
        // On a sret-returning body the return slot is supplied by the
        // caller; we plug the sret pointer in below instead of allocating.
        if use_sret && local_idx.index() == return_slot_index {
            locals.push(None);
            continue;
        }
        // `mutable`/`init` param locals are plugged in from the incoming
        // `ptr` arg below — do not allocate a fresh slot here.
        if by_pointer_param_locals.contains(&local_idx.index()) {
            locals.push(None);
            continue;
        }
        let ty_opt = match &decl.ty.kind {
            // Capability locals are wire-level opaque pointers — one
            // alloca per slot regardless of `CapabilityKind`. This
            // mirrors the leading-`ptr` parameter shape `lower_fn_sig`
            // produces, so the param-store loop below can write the
            // incoming capability arg into the slot uniformly with
            // user-typed params.
            MirTypeKind::Capability(_) => {
                Some(context.ptr_type(AddressSpace::default()).into())
            }
            _ => inkwell_basic_type(context, &decl.ty, program, arch),
        };
        let slot = match ty_opt {
            Some(ty) => {
                let name = format!("l{}", local_idx.index());
                let ptr = builder.build_alloca(ty, &name).expect("build_alloca in entry");
                Some(ptr)
            }
            None => None,
        };
        locals.push(slot);
    }

    if use_sret {
        let sret_ptr = function
            .get_nth_param(0)
            .expect("sret-returning body has a leading ptr param")
            .into_pointer_value();
        locals[return_slot_index] = Some(sret_ptr);
    }

    // Store function arguments into their parameter-local allocas.
    // `body.params` is ordered [capabilities..., user params...] by
    // the `BodyBuilder::add_capability` precedes-non-capability-params
    // invariant, and `lower_fn_sig` emits the LLVM signature in the
    // same order — so in-order iteration here pairs each slot-bearing
    // param with the next LLVM argument. A `let`/`take`-mode `Unit`
    // param is zero-sized and has NO LLVM arg at all,
    // so the pairing tracks a running
    // `llvm_index` that only advances on params that occupy an ABI
    // slot — positional `param_index + sret_offset` indexing would
    // overrun the declared param count as soon as a Unit param
    // precedes it (or IS the last param), tripping the get_nth_param
    // expect (monomorphized derived
    // hash/ord walking a payload-less sum variant instantiates
    // `hash_value(v: ())`). On win64 with sret in effect LLVM arg 0 is
    // the sret pointer; the running index starts at `sret_offset`.
    let mut llvm_index = sret_offset as u32;
    for param_info in body.params.iter() {
        if matches!(param_info.ty.kind, MirTypeKind::Unit)
            && matches!(param_info.mode, ParamMode::Let | ParamMode::Take)
        {
            // Zero-sized param: no LLVM arg, no alloca, nothing to
            // store. Reads of the local lower through `Operand::Unit`'s
            // no-representation path.
            continue;
        }
        let arg_value = function
            .get_nth_param(llvm_index)
            .expect("LLVM function param count matches MIR body.params plus sret prefix");
        llvm_index += 1;
        let is_by_pointer_user = matches!(param_info.mode, ParamMode::Mutable | ParamMode::Init)
            && !matches!(param_info.ty.kind, MirTypeKind::Capability(_));
        if is_by_pointer_user {
            // `mutable` / `init` user param: the LLVM arg IS the local's
            // pointer (it points at the caller's slot). Install it
            // directly so subsequent reads/writes through this local
            // thread to the caller's memory — for `init`, the callee's
            // in-place construction initialises the caller's binding.
            let ptr = arg_value.into_pointer_value();
            locals[param_info.local.index()] = Some(ptr);
            continue;
        }
        let slot = locals[param_info.local.index()].expect(
            "lowerable param (primitive / tuple / product-ADT / capability) local has an alloca by construction",
        );
        let is_byval = !matches!(param_info.ty.kind, MirTypeKind::Capability(_))
            && win64_indirect_aggregate(&param_info.ty, os, program, arch);
        if is_byval {
            // `arg_value` is a pointer to the caller-owned copy. Use
            // `build_memcpy` rather than load+store so the full byte
            // range is preserved — for sum-typed aggregates the
            // outer LLVM type spans the largest variant's payload,
            // so a typed load+store of a smaller variant decomposes
            // into per-field reads that drop the variant's actual
            // bytes anywhere they overlap the larger variant's
            // padding (e.g. an `f64` payload at offset 8 inside a
            // `{ tag, { i8, ptr, ptr } }` slot loses bytes 1..7 of
            // the double to the larger variant's padding hole).
            let llvm_ty = inkwell_basic_type(context, &param_info.ty, program, arch)
                .expect("byval param type lowers via inkwell_basic_type");
            let byval_ptr = arg_value.into_pointer_value();
            let size_val = llvm_ty
                .size_of()
                .expect("byval-param aggregate has a sizeof");
            builder
                .build_memcpy(
                    slot,
                    AGGREGATE_COPY_ALIGN,
                    byval_ptr,
                    AGGREGATE_COPY_ALIGN,
                    size_val,
                )
                .expect("build_memcpy from byval pointer into local alloca");
        } else {
            builder
                .build_store(slot, arg_value)
                .expect("build_store of arg into param alloca");
        }
    }

    locals
}

/// Whether the body uses the win64 sret return convention. Read by
/// the Return-terminator walker to decide whether to emit `ret void`
/// after the value has been written through `locals[RETURN_SLOT]`.
pub(super) fn body_uses_sret(
    body: &Body,
    program: &MirProgram,
    arch: Arch,
    os: Os,
) -> bool {
    win64_indirect_aggregate(&body.return_ty, os, program, arch)
}
