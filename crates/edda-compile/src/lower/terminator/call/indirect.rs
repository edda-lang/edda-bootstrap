//! The [`FuncRef::Indirect`] `Call` walker: construct an LLVM
//! `FunctionType` from the MIR `FnSig`, lower the fat `{ code, env }`
//! callee operand, thread capabilities + the env word, lower user args
//! (Win64 `byval`), and emit `build_indirect_call`. Mirrors the direct
//! path's ABI so direct + indirect calls of the same body stay
//! wire-compatible.

use edda_mir::{CallArg, CallMode, FnSig, MirPrim, MirTypeKind, Operand, Place, ThreadedCapability};
use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::types::{AnyType, BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, PointerValue};

use crate::error::CompileError;
use crate::lower::operand::lower_operand;
use crate::lower::place::lower_place_pointer;
use crate::lower::ty::{inkwell_basic_type, inkwell_prim_type};
use crate::lower::LowerCtx;

use super::abi::{
    AGGREGATE_COPY_ALIGN, aggregate_operand_source_ptr, mir_type_of_operand, use_indirect_abi,
};
use super::args::{is_capability_operand, load_capability_value};

/// Lower a [`FuncRef::Indirect`] Call terminator.
///
/// Constructs an LLVM `FunctionType` from `sig` (optional leading `sret`
/// pointer, then capabilities, then params, then return), lowers the
/// callee operand to a `PointerValue`, threads the caller's capabilities
/// as opaque-`ptr` args, lowers user args (with Win64 `byval` aggregate
/// translation), then emits `build_indirect_call`. Mirrors `lower_call`'s
/// Win64 ABI (sret return / byval params) and `?`-propagation through a
/// Result-shaped return so direct and indirect calls of the same body are
/// wire-compatible.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_indirect_call<'ctx>(
    callee: &Operand,
    sig: &FnSig,
    args: &[CallArg],
    capabilities: &[ThreadedCapability],
    destination: &Place,
    target: edda_mir::BlockId,
    on_error: Option<edda_mir::BlockId>,
    llvm_blocks: &[inkwell::basic_block::BasicBlock<'ctx>],
    cx: &LowerCtx<'ctx, '_>,
) -> Result<(), CompileError> {
    // Capability arity is an internal consistency check — the
    // typechecker's row-translation rewrites callee-side ids to caller-
    // side ids before MIR, so a mismatch here is a compiler bug, not a
    // user error. Everything else (Win64 sret return / byval aggregate
    // params, `?`-propagation through a Result-shaped return, panicking
    // callees) is handled below, mirroring the direct-call path
    // `lower_call` so direct and indirect calls of the same body stay
    // wire-compatible.
    if capabilities.len() != sig.capabilities.len() {
        return Err(CompileError::UnsupportedMirShape {
            shape: "indirect-call-capability-arity-mismatch",
            detail: format!(
                "body {:?} indirect-calls a fn-pointer expecting {} capability slots \
                 but the call site provides {}; typechecker should have proven these \
                 equal",
                cx.body_name,
                sig.capabilities.len(),
                capabilities.len()
            ),
        });
    }
    // `may_panic` needs no call-site work: a panicking callee runs its own
    // Panic terminator and the caller just calls it (same as the direct
    // path, which has no panic-handler wiring either). A Win64-indirect
    // return type crosses the boundary via a hidden caller-allocated
    // `sret` pointer — the function then returns void. Mirrors
    // `lower_call`'s `use_sret`.
    let use_sret = use_indirect_abi(&sig.ret, cx);

    // Build the LLVM function type from the MIR signature. The wire
    // layout matches the body-side `lower_fn_sig`: [sret ptr?,
    // capability ptrs…, user params…], void return when sret. A user
    // param that is `mutable` or a Win64-indirect aggregate occupies one
    // opaque `ptr` slot (the aggregate ones additionally carry a `byval`
    // attribute at the call site); everything else lowers to its natural
    // LLVM type.
    let ptr_ty = cx.context.ptr_type(AddressSpace::default());
    let mut param_tys: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::with_capacity(
        usize::from(use_sret) + sig.capabilities.len() + sig.params.len(),
    );
    if use_sret {
        param_tys.push(ptr_ty.into());
    }
    for _ in &sig.capabilities {
        param_tys.push(ptr_ty.into());
    }
    // Unified fat fn-value ABI: every
    // fat-callable body carries a leading env word after its capability
    // slots and before its user params. The callee operand is the fat
    // `{ code, env }` value; the env word is forwarded as this argument.
    let env_param_ty = inkwell_prim_type(cx.context, MirPrim::Usize, cx.arch)
        .expect("usize is always lowerable");
    param_tys.push(env_param_ty.into());
    for (i, (mode, param_ty)) in sig.params.iter().enumerate() {
        // Validate the underlying type lowers even when the slot is just
        // a pointer (so we reject e.g. `mutable Unit`).
        let llvm_ty = inkwell_basic_type(cx.context, param_ty, cx.program, cx.arch)
            .ok_or_else(|| CompileError::UnsupportedMirShape {
                shape: "non-lowerable-indirect-param",
                detail: format!(
                    "body {:?} indirect-calls a fn-pointer whose param #{i} type \
                     ({:?}) is not lowerable to LLVM",
                    cx.body_name, param_ty.kind
                ),
            })?;
        // `mutable` / `init` → caller-slot pointer (B19);
        // Win64-aggregate → `ptr byval(T)`
        // (byval attribute added at the call site). All occupy one opaque
        // `ptr` in the type.
        if matches!(*mode, edda_mir::ParamMode::Mutable | edda_mir::ParamMode::Init)
            || use_indirect_abi(param_ty, cx)
        {
            param_tys.push(ptr_ty.into());
        } else {
            param_tys.push(llvm_ty.into());
        }
    }
    let returns_void = use_sret
        || matches!(&sig.ret.kind, MirTypeKind::Unit | MirTypeKind::Never);
    let fn_type = if returns_void {
        cx.context.void_type().fn_type(&param_tys, false)
    } else {
        let ret_llvm_ty = inkwell_basic_type(cx.context, &sig.ret, cx.program, cx.arch)
            .ok_or_else(|| CompileError::UnsupportedMirShape {
                shape: "non-lowerable-indirect-return",
                detail: format!(
                    "body {:?} indirect-calls a fn-pointer whose return type ({:?}) \
                     is not lowerable to LLVM",
                    cx.body_name, sig.ret.kind
                ),
            })?;
        ret_llvm_ty.fn_type(&param_tys, false)
    };

    // Lower the callee operand to the fat function value
    // `{ code: ptr, env: usize }`. The
    // MIR-side operand type is `MirTypeKind::FnPtr(...)`, which
    // `inkwell_basic_type` lowers to that struct; reading the value
    // from its alloca produces a `StructValue`. Field 0 is the code
    // pointer dispatched through, field 1 is the env word forwarded as
    // the leading actual argument.
    let callee_value = lower_operand(callee, cx)?
        .ok_or_else(|| CompileError::UnsupportedMirShape {
            shape: "indirect-call-unit-callee",
            detail: format!(
                "body {:?} indirect-calls a Unit-typed callee operand; the typechecker \
                 should have proven this can't happen",
                cx.body_name
            ),
        })?;
    let callee_struct = match callee_value {
        BasicValueEnum::StructValue(s) => s,
        other => {
            return Err(CompileError::UnsupportedMirShape {
                shape: "indirect-call-non-struct-callee",
                detail: format!(
                    "body {:?} indirect-call callee lowered to {other:?}, expected the fat \
                     function value {{ code, env }} StructValue",
                    cx.body_name
                ),
            });
        }
    };
    let callee_ptr: PointerValue<'ctx> = cx
        .builder
        .build_extract_value(callee_struct, 0, "icall.code")
        .expect("extract code ptr from fat fn value")
        .into_pointer_value();
    let env_arg = cx
        .builder
        .build_extract_value(callee_struct, 1, "icall.env")
        .expect("extract env word from fat fn value");

    // Assemble LLVM args: capabilities first (one opaque `ptr` per
    // slot, loaded from the caller's own capability alloca), then user
    // args. This matches the parameter layout the body-side declaration
    // would produce for the same signature.
    let mut llvm_args: Vec<BasicMetadataValueEnum<'ctx>> =
        Vec::with_capacity(usize::from(use_sret) + capabilities.len() + args.len());
    let mut sret_slot: Option<(PointerValue<'ctx>, BasicTypeEnum<'ctx>)> = None;
    let mut byval_indices: Vec<(u32, BasicTypeEnum<'ctx>)> = Vec::new();

    // Position 0: the caller-allocated sret slot for an aggregate return.
    if use_sret {
        let ret_llvm_ty = inkwell_basic_type(cx.context, &sig.ret, cx.program, cx.arch)
            .ok_or_else(|| CompileError::UnsupportedMirShape {
                shape: "non-lowerable-indirect-sret-type",
                detail: format!(
                    "body {:?} indirect-calls a fn-pointer with sret return type {:?} \
                     which is not lowerable",
                    cx.body_name, sig.ret.kind
                ),
            })?;
        let slot = cx.build_entry_alloca(ret_llvm_ty, "icall.sret.tmp");
        sret_slot = Some((slot, ret_llvm_ty));
        llvm_args.push(slot.into());
    }
    for cap in capabilities {
        let cap_value = load_capability_value(cap, args, cx)?;
        llvm_args.push(cap_value.into());
    }
    // Leading env word — extracted from the fat callee above. Position
    // matches the `env_param_ty` slot pushed into the FunctionType after
    // the capability slots.
    llvm_args.push(env_arg.into());
    for (index, arg) in args.iter().enumerate() {
        // Mirror `build_call_args`: skip capability-typed user-arg
        // operands so we don't double-pass the pointer (the leading
        // capability loop already handled it).
        if is_capability_operand(&arg.operand, cx) {
            continue;
        }
        // `mutable` / `init` args at indirect-call sites lower the same
        // way as at direct-call sites: pass the caller-slot pointer so
        // writes through the param survive (B19) and an `init` out-param
        // initialises the caller's binding in place.
        if matches!(arg.mode, CallMode::Mutable | CallMode::Init) {
            let place = match &arg.operand {
                Operand::Copy(p) | Operand::Move(p) => p,
                _ => {
                    return Err(CompileError::UnsupportedMirShape {
                        shape: "inout-indirect-arg-non-place-operand",
                        detail: format!(
                            "body {:?} passes a non-Place operand at indirect-call \
                             arg #{index} with `mutable`/`init` mode",
                            cx.body_name
                        ),
                    });
                }
            };
            let arg_ptr = lower_place_pointer(place, cx)?;
            llvm_args.push(arg_ptr.into());
            continue;
        }
        // Win64-indirect aggregate args cross the boundary as `ptr
        // byval(T)`: stack-copy the value into a temp and pass its
        // address (byte-faithful memcpy from the source alloca, falling
        // back to a typed store for non-place operands). Mirrors
        // `lower_call`'s byval path; the `byval` attribute is added at
        // the call site below.
        let arg_mir_ty = mir_type_of_operand(&arg.operand, cx);
        let win64_aggregate = arg_mir_ty.as_ref().is_some_and(|t| use_indirect_abi(t, cx));
        if win64_aggregate {
            let ty = arg_mir_ty.expect("win64-aggregate indirect arg has a MIR type");
            let llvm_ty = inkwell_basic_type(cx.context, &ty, cx.program, cx.arch)
                .ok_or_else(|| CompileError::UnsupportedMirShape {
                    shape: "byval-indirect-arg-non-lowerable",
                    detail: format!(
                        "body {:?} passes a value of type {:?} into indirect-call arg \
                         #{index} which does not lower to an LLVM basic type",
                        cx.body_name, ty.kind
                    ),
                })?;
            let tmp = cx.build_entry_alloca(llvm_ty, "icall.byval.tmp");
            if let Some(src_ptr) = aggregate_operand_source_ptr(&arg.operand, cx) {
                let size_val = llvm_ty.size_of().expect("aggregate arg type has a sizeof");
                cx.builder
                    .build_memcpy(tmp, AGGREGATE_COPY_ALIGN, src_ptr, AGGREGATE_COPY_ALIGN, size_val)
                    .expect("build_memcpy from source into byval-arg temp");
            } else {
                let value = lower_operand(&arg.operand, cx)?.ok_or_else(|| {
                    CompileError::UnsupportedMirShape {
                        shape: "indirect-call-arg-unit",
                        detail: format!(
                            "body {:?} passes a Unit operand at indirect-call arg #{index}",
                            cx.body_name
                        ),
                    }
                })?;
                cx.builder
                    .build_store(tmp, value)
                    .expect("build_store of non-place aggregate arg into temp");
            }
            byval_indices.push((llvm_args.len() as u32, llvm_ty));
            llvm_args.push(tmp.into());
            continue;
        }
        let value = lower_operand(&arg.operand, cx)?
            .ok_or_else(|| CompileError::UnsupportedMirShape {
                shape: "indirect-call-arg-unit",
                detail: format!(
                    "body {:?} passes a Unit operand at indirect-call arg #{index}",
                    cx.body_name
                ),
            })?;
        llvm_args.push(value.into());
    }

    let call_site = cx
        .builder
        .build_indirect_call(fn_type, callee_ptr, &llvm_args, "icall")
        .expect("build_indirect_call inside a positioned block must succeed");

    // sret attribute on Param(0) when the return is indirect.
    if let Some((_, ret_llvm_ty)) = sret_slot {
        let kind_id = Attribute::get_named_enum_kind_id("sret");
        let attr = cx
            .context
            .create_type_attribute(kind_id, ret_llvm_ty.as_any_type_enum());
        call_site.add_attribute(AttributeLoc::Param(0), attr);
    }
    // byval attributes on the Win64-indirect aggregate params.
    if !byval_indices.is_empty() {
        let kind_id = Attribute::get_named_enum_kind_id("byval");
        for (idx, ty) in &byval_indices {
            let attr = cx
                .context
                .create_type_attribute(kind_id, ty.as_any_type_enum());
            call_site.add_attribute(AttributeLoc::Param(*idx), attr);
        }
    }

    // Write the return value to the destination. For the sret path the
    // callee already wrote through the slot; memcpy it to the
    // destination (a typed load+store would decompose the aggregate and
    // drop cross-variant padding bytes — same reasoning as `lower_call`),
    // and pre-extract the Result tag from the slot for the `?` path.
    let sret_tag: Option<inkwell::values::IntValue<'ctx>> =
        if let Some((slot, ret_ty)) = sret_slot {
            let dest_ptr = lower_place_pointer(destination, cx)?;
            let size_val = ret_ty.size_of().expect("aggregate sret type has a sizeof");
            cx.builder
                .build_memcpy(dest_ptr, AGGREGATE_COPY_ALIGN, slot, AGGREGATE_COPY_ALIGN, size_val)
                .expect("build_memcpy from sret slot to destination");
            if on_error.is_some() {
                let i8_ty = cx.context.i8_type();
                let tag_val = cx
                    .builder
                    .build_load(i8_ty, slot, "icall.tag")
                    .expect("build_load of tag byte from sret slot")
                    .into_int_value();
                Some(tag_val)
            } else {
                None
            }
        } else {
            None
        };
    let result_value: Option<BasicValueEnum<'ctx>> = if use_sret || returns_void {
        None
    } else {
        let ret_value = call_site
            .try_as_basic_value()
            .left()
            .expect("non-void indirect call returns a basic value");
        let dest_ptr = lower_place_pointer(destination, cx)?;
        cx.builder
            .build_store(dest_ptr, ret_value)
            .expect("build_store of indirect-call return value");
        Some(ret_value)
    };

    // Branch: unconditional to `target` on the no-raise path, or
    // tag-extract + conditional branch on `?`-propagation. The sret
    // path is rejected above, so the result is always a struct in SSA
    // when `on_error` is set.
    match on_error {
        None => {
            let target_bb = llvm_blocks[target.as_u32() as usize];
            cx.builder
                .build_unconditional_branch(target_bb)
                .expect("build_unconditional_branch after indirect call");
        }
        Some(error_block) => {
            // For sret returns the tag was already loaded from the slot
            // (the aggregate came back via memcpy, not an SSA struct);
            // otherwise extract it from the register-returned Result.
            let tag_val = if let Some(t) = sret_tag {
                t
            } else {
                let result_val =
                    result_value.ok_or_else(|| CompileError::UnsupportedMirShape {
                        shape: "indirect-call-on-error-void-return",
                        detail: format!(
                            "body {:?} has indirect Call with on_error: Some(_) but the \
                             callee returns void; `?` propagation requires a Result-shaped \
                             return",
                            cx.body_name
                        ),
                    })?;
                let sum_struct = match result_val {
                    BasicValueEnum::StructValue(s) => s,
                    _ => {
                        return Err(CompileError::UnsupportedMirShape {
                            shape: "indirect-call-on-error-non-sum-return",
                            detail: format!(
                                "body {:?} has indirect Call with on_error: Some(_) but the \
                                 callee return value is not a struct; `?` propagation \
                                 requires a sum-typed (Result-shaped) return",
                                cx.body_name
                            ),
                        });
                    }
                };
                cx.builder
                    .build_extract_value(sum_struct, 0, "icall.tag")
                    .expect("extract sum.tag at index 0")
                    .into_int_value()
            };
            let zero = tag_val.get_type().const_zero();
            let is_ok = cx
                .builder
                .build_int_compare(IntPredicate::EQ, tag_val, zero, "icall.is_ok")
                .expect("icmp eq tag, 0");
            let target_bb = llvm_blocks[target.as_u32() as usize];
            let error_bb = llvm_blocks[error_block.as_u32() as usize];
            cx.builder
                .build_conditional_branch(is_ok, target_bb, error_bb)
                .expect("conditional branch after `?`-propagating indirect call");
        }
    }
    Ok(())
}
