//! Capability-slot lookup and call-argument assembly helpers shared by
//! the direct and indirect call paths: materialise capability handles
//! from the paired positional operand (falling back to the caller's
//! slot allocas), skip double-passed capability receivers, and lower
//! user-mode operands.

use edda_mir::{CallArg, EffectId, MirTypeKind, Operand, ThreadedCapability};
use inkwell::AddressSpace;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum};

use crate::error::CompileError;
use crate::lower::operand::lower_operand;
use edda_mir::Idx;
use crate::lower::LowerCtx;

/// Assemble the LLVM argument vector for a call: capability handles
/// materialised via [`load_capability_value`], then user args lowered
/// through [`lower_operand`].
pub(super) fn build_call_args<'ctx>(
    capabilities: &[ThreadedCapability],
    args: &[CallArg],
    cx: &LowerCtx<'ctx, '_>,
) -> Result<Vec<BasicMetadataValueEnum<'ctx>>, CompileError> {
    let mut llvm_args: Vec<BasicMetadataValueEnum<'ctx>> =
        Vec::with_capacity(capabilities.len() + args.len());

    for cap in capabilities {
        let cap_value = load_capability_value(cap, args, cx)?;
        llvm_args.push(cap_value.into());
    }

    for (index, arg) in args.iter().enumerate() {
        // Capability-typed user args arise when a method-call receiver is a
        // capability (e.g. `stdout.print_line(msg)` desugars the receiver as
        // arg 0). The capability is already provided as a leading ptr via the
        // capability-slot loop above. Including it again would double-pass the
        // pointer and shift every subsequent LLVM arg position, so skip it.
        if is_capability_operand(&arg.operand, cx) {
            continue;
        }
        // TODO: apply `CallMode` -> LLVM attribute mapping
        // here. `CallMode::Read` / `CallMode::Mutable` -> `byref` +
        // `noalias`; `CallMode::Take` -> `byval`; `CallMode::Init` ->
        // `sret`. Currently everything passes by value so the lowering
        // is type-correct but ABI-loose for non-`Let` modes.
        let _ = arg.mode;
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
        llvm_args.push(value.into());
    }

    Ok(llvm_args)
}

/// Materialise the capability handle for one [`ThreadedCapability`] of
/// a call: the paired positional operand's value when the emitter
/// recorded one, else the `EffectId`-keyed slot load.
pub(super) fn load_capability_value<'ctx>(
    cap: &ThreadedCapability,
    args: &[CallArg],
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    let ptr_ty = cx.context.ptr_type(AddressSpace::default());
    if let Some(value_arg) = cap.value_arg {
        let place = args
            .get(value_arg as usize)
            .and_then(|arg| match &arg.operand {
                Operand::Copy(p) | Operand::Move(p) => Some(p),
                _ => None,
            })
            .filter(|p| p.projection.is_empty())
            .filter(|p| {
                cx.body
                    .locals
                    .get(p.local)
                    .is_some_and(|d| matches!(d.ty.kind, MirTypeKind::Capability(_)))
            })
            .ok_or_else(|| CompileError::UnsupportedMirShape {
                shape: "cap-value-arg-malformed",
                detail: format!(
                    "body {:?} pairs capability EffectId#{} with value_arg={} which is \
                     not a capability-typed positional argument; the MIR emitter must \
                     validate the pairing before recording it",
                    cx.body_name,
                    cap.id.as_u32(),
                    value_arg
                ),
            })?;
        let cap_alloca = cx.locals[place.local.index()].expect(
            "capability local has an opaque-`ptr` alloca by the \
             `allocate_locals` invariant",
        );
        let cap_value = cx
            .builder
            .build_load(ptr_ty, cap_alloca, "cap")
            .expect("build_load of capability alloca must succeed in a positioned block");
        return Ok(cap_value);
    }
    let slot_local = lookup_capability_slot(cap.id, cx)?;
    let cap_alloca = cx.locals[slot_local.index()].expect(
        "capability local has an opaque-`ptr` alloca by the \
         `allocate_locals` invariant",
    );
    let cap_value = cx
        .builder
        .build_load(ptr_ty, cap_alloca, "cap")
        .expect("build_load of capability alloca must succeed in a positioned block");
    Ok(cap_value)
}

/// Returns `true` when `operand` is a `Copy` or `Move` of a
/// capability-typed local. Used by [`build_call_args`] to skip the
/// double-pass of capability receivers introduced by method-call
/// desugaring.
pub(super) fn is_capability_operand(operand: &edda_mir::Operand, cx: &LowerCtx<'_, '_>) -> bool {
    let place = match operand {
        edda_mir::Operand::Copy(p) | edda_mir::Operand::Move(p) => p,
        _ => return false,
    };
    cx.body
        .locals
        .get(place.local)
        .map(|d| matches!(d.ty.kind, MirTypeKind::Capability(_)))
        .unwrap_or(false)
}

/// Find the caller's [`CapabilitySlot`] with the given [`EffectId`]
/// and return its `param_local`. Rejects with
/// `unknown-capability-at-call` if the row does not contain the id —
/// this is an internal-consistency check, since
/// `edda-types`'s row-translation pass should have rewritten every
/// callee-side id to a caller-side id before MIR was emitted.
pub(super) fn lookup_capability_slot(
    id: EffectId,
    cx: &LowerCtx<'_, '_>,
) -> Result<edda_mir::LocalId, CompileError> {
    cx.body
        .effect_row
        .capabilities
        .iter()
        // Body-local capability values (forked child allocators)
        // live in a second slot list;
        // their `param_local` is an ordinary local whose alloca holds
        // the handle, so the load below is identical.
        .chain(cx.body.local_capabilities.iter())
        .find(|slot| slot.id == id)
        .map(|slot| slot.param_local)
        .ok_or_else(|| CompileError::UnsupportedMirShape {
            shape: "unknown-capability-at-call",
            detail: format!(
                "body {:?} threads capability EffectId#{} which is not in the caller's \
                 effect_row.capabilities or local_capabilities",
                cx.body_name,
                id.as_u32()
            ),
        })
}
