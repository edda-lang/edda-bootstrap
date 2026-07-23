//! MIR [`Body`] signature -> inkwell [`FunctionValue`] declaration.
//!
//! Primitive, tuple, product-ADT, and sum-ADT parameter and return
//! types are admitted (all four flow through
//! [`inkwell_basic_type`]). Capability slots thread as
//! leading opaque-pointer parameters per `backend-choice.md` §6.2 —
//! every body that opens an effect row receives one `ptr` argument
//! per `CapabilitySlot` before its declared user params.
//! `Unit`/`Never` returns lower to LLVM `void`; a `let`/`take`-mode
//! `Unit` param is zero-sized and contributes no ABI slot at all —
//! mirroring how `Unit` locals
//! ([`super::local::allocate_locals`]) and `Operand::Unit`
//! ([`super::operand::lower_operand`]) already carry no LLVM
//! representation. Remaining compound types (Slice, FnPtr) and `Str`
//! produce [`CompileError::UnsupportedMirShape`] with an informative
//! `shape` label.

use edda_intern::Interner;
use edda_mir::{Body, MirPrim, MirProgram, MirTypeKind, ParamMode};
use edda_target::Arch;
use inkwell::AddressSpace;
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::types::{AnyType, BasicMetadataTypeEnum, BasicType, BasicTypeEnum};
use inkwell::values::FunctionValue;

use crate::error::CompileError;

use super::body::{derive_os, win64_indirect_aggregate};
use super::ty::{inkwell_basic_type, inkwell_prim_type};

/// Emit an LLVM function declaration for `body`'s signature into `module`.
///
/// Scope: primitive, tuple, product-ADT, sum-ADT, slice, fn-pointer,
/// and capability parameter types plus the corresponding return types
/// (`Unit` / `Never` lower to LLVM `void`). A `let`/`take`-mode `Unit`
/// param is zero-sized and lowers to no ABI slot at all. Capability slots in
/// `body.effect_row.capabilities` thread through as leading
/// opaque-`ptr` parameters per `backend-choice.md` §6.2 — the
/// wire-level capability ABI is one pointer per slot, even though the
/// type system tracks each slot's `CapabilityKind` separately. Fn-
/// pointer params/returns lower as plain opaque `ptr` because LLVM 15+
/// pointers carry no element type at the IR level; their signature
/// payload is consulted only at the indirect-call site where the
/// `lower_indirect_call` walker builds the matching `FunctionType`.
/// The resulting [`FunctionValue`] has no entry block yet —
/// `lower_body` walks the body's basic blocks and emits instructions.
///
/// On win64, aggregate parameters and returns that are not
/// 1/2/4/8 bytes use the platform's indirect ABI: a hidden pointer
/// (`byval(<ty>)` on the param, `sret(<ty>)` on the prepended return-slot
/// param). This mirrors what Rust emits for `extern "C"` on
/// `x86_64-pc-windows-msvc` and matches `declare_extern` so source-bodied
/// caller↔callee agree on the wire shape — without it LLVM's default
/// per-field register-splitting silently drops cross-field stores in the
/// callee (`store {double}` at offset 8 vanishes when the struct is
/// returned via al/dl/rcx).
pub(crate) fn lower_fn_sig<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    body: &Body,
    program: &MirProgram,
    interner: &Interner,
    arch: Arch,
    symbol: &str,
) -> Result<FunctionValue<'ctx>, CompileError> {
    let name = symbol;
    let _ = interner; // body.name -> diagnostic text done by `lower_body`; signature pass uses the symbol
    let os = derive_os(module);

    // Win64: aggregate returns >8 bytes (excluding 1/2/4/8) go via
    // hidden sret pointer; the function returns void and the caller
    // prepends a `ptr sret(<ret_ty>)` as the first arg.
    let ret_is_aggregate = matches!(
        &body.return_ty.kind,
        MirTypeKind::Prim(MirPrim::Str)
            | MirTypeKind::Tuple(_)
            | MirTypeKind::Adt(_)
            | MirTypeKind::Slice(_)
            // Fat function value `{ code, env }`
            | MirTypeKind::FnPtr(_)
    );
    let use_sret = ret_is_aggregate
        && win64_indirect_aggregate(&body.return_ty, os, program, arch);
    let ret_llvm_ty: Option<BasicTypeEnum<'ctx>> = if ret_is_aggregate {
        Some(
            inkwell_basic_type(context, &body.return_ty, program, arch).ok_or_else(|| {
                CompileError::UnsupportedMirShape {
                    shape: "non-lowerable-aggregate-return",
                    detail: format!(
                        "body {name:?} returns {:?}, which is not lowerable \
                         (nested capabilities, fn pointers, or similar also fail here)",
                        body.return_ty.kind
                    ),
                }
            })?,
        )
    } else {
        None
    };

    let mut param_tys: Vec<BasicMetadataTypeEnum<'ctx>> =
        Vec::with_capacity(body.params.len() + if use_sret { 1 } else { 0 });
    let mut sret_param: Option<(u32, BasicTypeEnum<'ctx>)> = None;
    let mut byval_params: Vec<(u32, BasicTypeEnum<'ctx>)> = Vec::new();

    if use_sret {
        let ret_ty = ret_llvm_ty.expect("indirect-return aggregate has llvm type");
        sret_param = Some((param_tys.len() as u32, ret_ty));
        param_tys.push(context.ptr_type(AddressSpace::default()).into());
    }

    for (index, param) in body.params.iter().enumerate() {
        match &param.ty.kind {
            // Zero-sized: no physical ABI slot at all — mirrors the
            // `Unit` treatment already applied to locals
            // (`super::local::allocate_locals` gives them no alloca)
            // and call-site operands (`Operand::Unit` lowers to
            // `Ok(None)` in `super::operand::lower_operand`). A
            // `mutable`/`init` Unit param has no caller slot to write
            // through and is left to the mutable/init arm below,
            // which still rejects it via `inkwell_basic_type`.
            MirTypeKind::Unit if matches!(param.mode, ParamMode::Let | ParamMode::Take) => {}
            // Capability slots lower to an opaque pointer regardless
            // of their `CapabilityKind` — the kind is type-system
            // bookkeeping; the wire-level ABI is one pointer per slot
            // (backend-choice.md §6.2). The `BodyBuilder` invariant
            // "capability params must be added before any non-
            // capability param" guarantees these are the leading
            // entries of `body.params`, so the resulting LLVM
            // signature gets capabilities first by construction.
            MirTypeKind::Capability(_) => {
                param_tys.push(context.ptr_type(AddressSpace::default()).into());
            }
            // `mutable` and `init` user params lower as opaque `ptr`
            // — the wire-level ABI is a pointer to the caller's slot
            // so writes through the parameter are observed by the
            // caller (B19). For `init` the callee initialises a
            // previously-`uninit` caller binding in place, so the
            // pointer-to-caller-slot is mandatory: a byval / by-value
            // copy would init the callee's private copy and leave the
            // caller's binding uninitialised.
            // The byval translation is *not* applied here: byval
            // semantics pass a pointer-to-caller-owned-copy. We still
            // lower the value type (`llvm_ty`) below so `allocate_locals`
            // can use it; but the LLVM parameter slot itself is a plain
            // `ptr` with no attribute.
            _ if matches!(param.mode, ParamMode::Mutable | ParamMode::Init) => {
                let _llvm_ty = inkwell_basic_type(context, &param.ty, program, arch)
                    .ok_or_else(|| CompileError::UnsupportedMirShape {
                        shape: "non-lowerable-param",
                        detail: format!(
                            "param #{index} of body {name:?} has type {:?}; `mutable` \
                             passing requires a lowerable underlying type",
                            param.ty.kind
                        ),
                    })?;
                param_tys.push(context.ptr_type(AddressSpace::default()).into());
            }
            _ => {
                let llvm_ty = inkwell_basic_type(context, &param.ty, program, arch)
                    .ok_or_else(|| CompileError::UnsupportedMirShape {
                        shape: "non-lowerable-param",
                        detail: format!(
                            "param #{index} of body {name:?} has type {:?}, which is currently \
                             only handled for capabilities, primitives, tuples, product ADTs, \
                             and sum ADTs",
                            param.ty.kind
                        ),
                    })?;
                if win64_indirect_aggregate(&param.ty, os, program, arch) {
                    let param_idx = param_tys.len() as u32;
                    param_tys.push(context.ptr_type(AddressSpace::default()).into());
                    byval_params.push((param_idx, llvm_ty));
                } else {
                    param_tys.push(llvm_ty.into());
                }
            }
        }
    }

    let fn_ty = if use_sret {
        context.void_type().fn_type(&param_tys, false)
    } else {
        match &body.return_ty.kind {
            MirTypeKind::Prim(MirPrim::Str)
            | MirTypeKind::Tuple(_)
            | MirTypeKind::Adt(_)
            | MirTypeKind::Slice(_)
            // Fat function value `{ code, env }` is an aggregate return:
            // when it is not Win64-indirect
            // it lowers by value as its `{ ptr, usize }` struct, matching
            // `inkwell_basic_type`; when it is, `use_sret` above already
            // routed it through the hidden sret pointer.
            | MirTypeKind::FnPtr(_) => ret_llvm_ty
                .expect("aggregate-return type was lowered above")
                .fn_type(&param_tys, false),
            MirTypeKind::Prim(prim) => {
                let ret_ty = inkwell_prim_type(context, *prim, arch)
                    .expect("non-Str primitive must lower; Str is handled above");
                ret_ty.fn_type(&param_tys, false)
            }
            MirTypeKind::Capability(_) => {
                // Capability returns lower to opaque `ptr`. LLVM 15+
                // pointers are untyped; the kind discriminator is
                // type-system bookkeeping only — the ABI is one ptr.
                // Required so capability narrowing methods
                // (`Subprocess.allowing(...)` etc.) lower end-to-end.
                context
                    .ptr_type(AddressSpace::default())
                    .fn_type(&param_tys, false)
            }
            MirTypeKind::Unit | MirTypeKind::Never => {
                context.void_type().fn_type(&param_tys, false)
            }
        }
    };

    let func = module.add_function(name, fn_ty, None);

    if let Some((idx, ret_ty)) = sret_param {
        let kind_id = Attribute::get_named_enum_kind_id("sret");
        let attr = context.create_type_attribute(kind_id, ret_ty.as_any_type_enum());
        func.add_attribute(AttributeLoc::Param(idx), attr);
    }
    if !byval_params.is_empty() {
        let kind_id = Attribute::get_named_enum_kind_id("byval");
        for (idx, ty) in &byval_params {
            let attr = context.create_type_attribute(kind_id, ty.as_any_type_enum());
            func.add_attribute(AttributeLoc::Param(*idx), attr);
        }
    }

    Ok(func)
}
