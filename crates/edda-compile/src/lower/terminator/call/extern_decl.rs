//! Lazy extern-function declaration for the `Call` walker. Mirrors
//! `lower_fn_sig`'s body-side type lowering so a declared extern's wire
//! ABI agrees with the call site byte-for-byte, including the
//! alloc-family raw-extern shape patches.

use edda_mir::{FnSig, MirPrim, MirTypeKind};
use inkwell::AddressSpace;
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::types::{AnyType, BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::FunctionValue;

use crate::error::CompileError;
use crate::lower::ty::{inkwell_basic_type, inkwell_prim_type};
use crate::lower::LowerCtx;

use super::abi::{AllocFamilyRaw, use_indirect_abi, win64_small_aggregate_ret_size};

/// Declare an extern function with the given [`FnSig`] into the
/// current module. Mirrors `lower_fn_sig`'s body-side type lowering
/// because both reach the same wire-level ABI per
/// `backend-choice.md` §6.2.
pub(super) fn declare_extern<'ctx>(
    name: &str,
    sig: &FnSig,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<FunctionValue<'ctx>, CompileError> {
    let alloc_family = AllocFamilyRaw::from_extern_name(name);
    let mut param_tys: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::with_capacity(
        sig.capabilities.len() + sig.params.len() + 1, /* +1 for possible sret */
    );
    let mut sret_param: Option<(u32, BasicTypeEnum<'ctx>)> = None;
    let mut byval_params: Vec<(u32, BasicTypeEnum<'ctx>)> = Vec::new();

    // Alloc family raw externs replace the `T` return with `void` plus
    // a trailing out-pointer (BoxRead / BoxUnbox), bypassing the sret
    // calculation entirely.
    let force_void_return = matches!(
        alloc_family,
        Some(AllocFamilyRaw::BoxRead) | Some(AllocFamilyRaw::BoxUnbox)
    );

    // Win64 ABI: when the return type is an aggregate > 8 bytes, the
    // function returns void and the caller passes a hidden pointer to
    // a result slot as the first param. Both sides apply this with
    // `sret`.
    let use_sret = !force_void_return && use_indirect_abi(&sig.ret, cx);
    let ret_aggregate_lower = matches!(
        &sig.ret.kind,
        MirTypeKind::Prim(MirPrim::Str)
            | MirTypeKind::Tuple(_)
            | MirTypeKind::Adt(_)
            | MirTypeKind::Slice(_)
    );
    let ret_llvm_ty: Option<BasicTypeEnum<'ctx>> = if ret_aggregate_lower && !force_void_return {
        Some(
            inkwell_basic_type(cx.context, &sig.ret, cx.program, cx.arch).ok_or_else(|| {
                CompileError::UnsupportedMirShape {
                    shape: "non-lowerable-extern-return",
                    detail: format!(
                        "extern {name:?} returns {:?} which is not lowerable \
                         (nested unsupported elements also fail here)",
                        sig.ret.kind
                    ),
                }
            })?,
        )
    } else {
        None
    };
    if use_sret {
        let ret_ty = ret_llvm_ty.expect("indirect-return aggregate has llvm type");
        param_tys.push(cx.context.ptr_type(AddressSpace::default()).into());
        sret_param = Some((0, ret_ty));
    }

    // Capabilities — one opaque pointer per slot.
    for _ in &sig.capabilities {
        param_tys.push(cx.context.ptr_type(AddressSpace::default()).into());
    }

    // User params — aggregate args > 8 bytes pass as opaque pointers
    // with `byval` attribute on win64. `AllocFamilyRaw::Alloc` overrides
    // this for the `value: T` slot at index 2 (after size, align): the
    // runtime takes a plain `ptr` (no byval attribute) and reads `size`
    // bytes from it via `copy_nonoverlapping`, so the wire type is just
    // `ptr` and byval byte-preservation is the caller's job. `mutable`
    // and `init` params also lower as a plain `ptr` (no byval, no
    // by-value): the pointer references the caller's slot so writes
    // survive (B19) and an `init` out-param is initialised in place.
    for (index, (mode, ty)) in sig.params.iter().enumerate() {
        let llvm_ty = inkwell_basic_type(cx.context, ty, cx.program, cx.arch).ok_or_else(|| {
            CompileError::UnsupportedMirShape {
                shape: "non-lowerable-extern-param",
                detail: format!(
                    "extern {name:?} param #{index} has type {:?}, which is currently only \
                     handled for primitives, tuples, and product ADTs",
                    ty.kind
                ),
            }
        })?;
        let is_alloc_value_slot =
            matches!(alloc_family, Some(AllocFamilyRaw::Alloc)) && index == 2;
        let is_inout = matches!(*mode, edda_mir::ParamMode::Mutable | edda_mir::ParamMode::Init);
        if is_alloc_value_slot || is_inout {
            // Plain `ptr` — no byval, no value pass.
            let _ = llvm_ty; // type validated above, only the ptr wire is emitted
            param_tys.push(cx.context.ptr_type(AddressSpace::default()).into());
        } else if use_indirect_abi(ty, cx) {
            let param_idx = param_tys.len() as u32;
            param_tys.push(cx.context.ptr_type(AddressSpace::default()).into());
            byval_params.push((param_idx, llvm_ty));
        } else {
            param_tys.push(llvm_ty.into());
        }
    }

    // Alloc-family raw with out-pointer return: append the trailing
    // `ptr` slot the runtime writes the `T` bytes into.
    if force_void_return {
        param_tys.push(cx.context.ptr_type(AddressSpace::default()).into());
    }

    // Return type — void when indirect or when the alloc-family raw
    // shape forces a void return; otherwise lower as before.
    let fn_ty = if use_sret || force_void_return {
        cx.context.void_type().fn_type(&param_tys, false)
    } else {
        match &sig.ret.kind {
            // Small aggregates (1/2/4/8 bytes) on win64: the MSVC C ABI
            // returns them packed in RAX's low bytes (rustc emits e.g.
            // `i16` for a `#[repr(C)] { tag: u8, val: u8 }`), while a
            // literal LLVM struct return splits fields across AL/DL/...
            // Declare the return as iN so the caller reads the register
            // the callee actually wrote.
            MirTypeKind::Prim(MirPrim::Str)
            | MirTypeKind::Tuple(_)
            | MirTypeKind::Adt(_)
            | MirTypeKind::Slice(_) => {
                if let Some(size) = win64_small_aggregate_ret_size(&sig.ret, cx) {
                    cx.context
                        .custom_width_int_type(size * 8)
                        .fn_type(&param_tys, false)
                } else {
                    ret_llvm_ty
                        .expect("aggregate-return type was lowered above")
                        .fn_type(&param_tys, false)
                }
            }
            MirTypeKind::Prim(prim) => inkwell_prim_type(cx.context, *prim, cx.arch)
                .expect("non-Str primitive must lower")
                .fn_type(&param_tys, false),
            // Capability returns lower to opaque `ptr` — the same wire
            // shape `lower_fn_sig` uses for capability params/returns
            // (one ptr per slot, kind discriminator is type-system
            // bookkeeping only). A capability is a scalar pointer, NOT
            // an aggregate, so it is intentionally absent from the
            // `ret_aggregate_lower` set above and lowers by value here.
            // Required so a capability-returning `@abi` extern (e.g.
            // `__edda_fs_read_only`) can be declared end-to-end.
            MirTypeKind::Capability(_) => cx
                .context
                .ptr_type(AddressSpace::default())
                .fn_type(&param_tys, false),
            MirTypeKind::Unit | MirTypeKind::Never => {
                cx.context.void_type().fn_type(&param_tys, false)
            }
            other => {
                return Err(CompileError::UnsupportedMirShape {
                    shape: "non-lowerable-extern-return",
                    detail: format!(
                        "extern {name:?} returns {other:?}, which is currently only handled for \
                         primitive / tuple / product-ADT / sum-ADT / str / slice / capability / Unit / Never"
                    ),
                });
            }
        }
    };

    let func = cx.module.add_function(name, fn_ty, None);

    // Apply sret attribute on the (prepended) return-slot param.
    if let Some((idx, ret_ty)) = sret_param {
        let kind_id = Attribute::get_named_enum_kind_id("sret");
        let attr = cx
            .context
            .create_type_attribute(kind_id, ret_ty.as_any_type_enum());
        func.add_attribute(AttributeLoc::Param(idx), attr);
    }

    // Apply byval attributes on indirect-passed aggregate params.
    if !byval_params.is_empty() {
        let kind_id = Attribute::get_named_enum_kind_id("byval");
        for (idx, ty) in &byval_params {
            let attr = cx
                .context
                .create_type_attribute(kind_id, ty.as_any_type_enum());
            func.add_attribute(AttributeLoc::Param(*idx), attr);
        }
    }

    Ok(func)
}
