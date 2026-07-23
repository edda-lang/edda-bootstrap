//! Shared Win64-ABI classification helpers for the `Call` walker:
//! aggregate-vs-register passing rules, alloc-family raw-extern
//! recognition, and operand/return MIR-type recovery. Consumed by the
//! direct, indirect, and extern-declaration call paths so all three
//! agree on the wire shape.

use edda_mir::{FuncRef, MirPrim, MirType, MirTypeKind, Operand};
use edda_target::Os;
use inkwell::AddressSpace;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, PointerValue};

use crate::lower::ty::compute_type_size;
use crate::target_info::pointer_width;
use edda_mir::Idx;
use crate::lower::LowerCtx;

/// Conservative alignment used for the `build_memcpy` paths that
/// preserve cross-variant bytes on aggregate copies. The aggregate
/// types we copy (sum-shaped returns, slices) all have 8-byte
/// natural alignment on every v0.1 target; passing a smaller value
/// is sound but generates worse asm.
pub(super) const AGGREGATE_COPY_ALIGN: u32 = 8;

/// Runtime alloc-family raw externs whose C ABI moves the `T` slot
/// via an explicit pointer plus the prepended `size` argument, instead
/// of typing the slot as `T` directly. Recognising these by name at
/// the LLVM seam keeps the MIR-side signature natural (callers see
/// `value: take T` / `-> T`) while letting the runtime use a uniform
/// layout-agnostic wire (`*const u8` / `*mut u8`) that works for any
/// `T` regardless of register width.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub(super) enum AllocFamilyRaw {
    /// `__edda_alloc_raw`: the `value: T` param (index 2 after size /
    /// align) becomes a `ptr` to a caller-owned copy of T; return
    /// shape (`Result<Box, AllocError>`) is unchanged.
    Alloc,
    /// `__edda_box_read_raw`: append a trailing `ptr` (out value slot)
    /// to the param list; replace the `T` return with `void`.
    BoxRead,
    /// `__edda_box_unbox_raw`: append a trailing `ptr` (out value slot)
    /// to the param list; replace the `T` return with `void`.
    BoxUnbox,
}

impl AllocFamilyRaw {
    pub(super) fn from_extern_name(name: &str) -> Option<Self> {
        match name {
            "__edda_alloc_raw" => Some(Self::Alloc),
            "__edda_box_read_raw" => Some(Self::BoxRead),
            "__edda_box_unbox_raw" => Some(Self::BoxUnbox),
            _ => None,
        }
    }

    pub(super) fn from_func(func: &FuncRef, cx: &LowerCtx<'_, '_>) -> Option<Self> {
        if let FuncRef::Extern { name, .. } = func {
            Self::from_extern_name(cx.interner.resolve(*name))
        } else {
            None
        }
    }
}

/// The `std.os.raw.slice_addr` raw extern. On native targets the call
/// resolves to this `edda-rt` symbol at link time; a Wasm module has no
/// runtime library to link, so the call walker lowers it inline instead
/// of emitting an unresolvable `env.__edda_slice_addr` host import
/// (counterpart to `__edda_slice_from_raw`).
pub(super) const SLICE_ADDR_EXTERN: &str = "__edda_slice_addr";

/// Whether `func` is the `__edda_slice_addr` raw extern — the recognition
/// gate for the Wasm inline lowering in [`super::direct::lower_call`].
pub(super) fn is_slice_addr_extern(func: &FuncRef, cx: &LowerCtx<'_, '_>) -> bool {
    matches!(func, FuncRef::Extern { name, .. } if cx.interner.resolve(*name) == SLICE_ADDR_EXTERN)
}

/// Source-pointer probe for aggregate operands at call boundaries.
/// Mirrors `statement::aggregate_source_ptr` — used by the byval-send
/// and alloc-family value-slot paths to substitute `memcpy(<dst>,
/// <src_ptr>, sizeof)` for the typed `load <agg>; store <agg>` round
/// trip that would otherwise drop cross-variant padding bytes on
/// sum-typed aggregates.
pub(super) fn aggregate_operand_source_ptr<'ctx>(
    operand: &Operand,
    cx: &LowerCtx<'ctx, '_>,
) -> Option<PointerValue<'ctx>> {
    let place = match operand {
        Operand::Copy(p) | Operand::Move(p) => p,
        _ => return None,
    };
    if !place.projection.is_empty() {
        // Projected places need the full `walk_place` chain to recover
        // their leaf pointer. Defer that path — the load+store fallback
        // works for non-sum aggregates (tuples / products) and is the
        // pre-existing behaviour for projected sum reads.
        return None;
    }
    let decl = cx.body.locals.get(place.local)?;
    let is_aggregate = matches!(
        &decl.ty.kind,
        MirTypeKind::Prim(MirPrim::Str)
            | MirTypeKind::Slice(_)
            | MirTypeKind::Tuple(_)
            | MirTypeKind::Adt(_)
            // Fat function value `{ code, env }`
            | MirTypeKind::FnPtr(_)
    );
    if !is_aggregate {
        return None;
    }
    cx.locals[place.local.index()]
}

/// Win64 ABI: aggregates whose size is not 1/2/4/8 bytes must be
/// passed via a hidden pointer. This helper classifies a MIR type
/// against that rule using [`compute_type_size`] for the size proxy.
pub(super) fn win64_indirect_size(
    ty: &MirType,
    program: &edda_mir::MirProgram,
    arch: edda_target::Arch,
) -> Option<u32> {
    let is_aggregate = matches!(
        &ty.kind,
        MirTypeKind::Prim(MirPrim::Str)
            | MirTypeKind::Slice(_)
            | MirTypeKind::Tuple(_)
            | MirTypeKind::Adt(_)
            // Fat function value `{ code, env }`
            | MirTypeKind::FnPtr(_)
    );
    if !is_aggregate {
        return None;
    }
    let size = compute_type_size(ty, program, arch);
    if size == 0 || matches!(size, 1 | 2 | 4 | 8) {
        None
    } else {
        Some(size)
    }
}

/// Active-target predicate: does the extern boundary need indirect
/// passing for this type? Drives the `byval`/`sret` attribute work
/// in [`super::extern_decl::declare_extern`] and [`super::direct::lower_call`].
pub(super) fn use_indirect_abi(ty: &MirType, cx: &LowerCtx<'_, '_>) -> bool {
    cx.os == Os::Windows && win64_indirect_size(ty, cx.program, cx.arch).is_some()
}

/// Win64: byte size of a small aggregate (1/2/4/8 bytes) that a
/// rustc/clang-compiled extern returns packed in a single register
/// per the MSVC C ABI.
pub(super) fn win64_small_aggregate_ret_size(ty: &MirType, cx: &LowerCtx<'_, '_>) -> Option<u32> {
    if cx.os != Os::Windows {
        return None;
    }
    let is_aggregate = matches!(
        &ty.kind,
        MirTypeKind::Prim(MirPrim::Str)
            | MirTypeKind::Slice(_)
            | MirTypeKind::Tuple(_)
            | MirTypeKind::Adt(_)
            | MirTypeKind::FnPtr(_)
    );
    if !is_aggregate {
        return None;
    }
    let size = compute_type_size(ty, cx.program, cx.arch);
    matches!(size, 1 | 2 | 4 | 8).then_some(size)
}

/// Recover the MIR type of an [`Operand`] by consulting the body's
/// locals or program's constants. Returns `None` for `Operand::Unit`
/// or out-of-range references — callers fall through to direct passing
/// in those cases (`Unit` shouldn't reach the call boundary anyway).
pub(super) fn mir_type_of_operand(operand: &Operand, cx: &LowerCtx<'_, '_>) -> Option<MirType> {
    match operand {
        Operand::Copy(p) | Operand::Move(p) => {
            cx.body.locals.get(p.local).map(|d| d.ty.clone())
        }
        Operand::Const(id) => cx.program.consts.get(*id).map(|c| c.ty.clone()),
        Operand::Unit => None,
    }
}

/// Look up the callee's actual return type at the MIR layer. Used by
/// the win64 ABI translation to decide whether the call must use the
/// sret pattern.
pub(super) fn callee_return_ty(func: &FuncRef, cx: &LowerCtx<'_, '_>) -> Option<MirType> {
    match func {
        FuncRef::Body(body_id) => cx.program.bodies.get(*body_id).map(|b| b.return_ty.clone()),
        FuncRef::Extern { sig, .. } => Some(sig.ret.clone()),
        // The indirect-call signature carries the return type inline.
        FuncRef::Indirect { sig, .. } => Some(sig.ret.clone()),
    }
}

/// The `std.os.raw` load/store raw externs, inline-lowered by
/// [`super::direct::lower_raw_mem_inline`] to a direct `inttoptr` +
/// align-1 `load`/`store` instead of a C-ABI call into `edda-rt`.
/// The native compiler lowers these symbols inline on every target;
/// the bootstrap previously emitted
/// extern calls, taxing every step of the self-hosted allocator's
/// free-list walks and byte-copy loops with call overhead.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub(super) enum RawMemInline {
    /// `__edda_load_{u8,u16,u32,u64,usize}` — `(addr: u64) -> uN`; the
    /// payload is the loaded integer's bit width.
    Load(u32),
    /// `__edda_store_{u8,u16,u32,u64,usize}` — `(addr: u64, value: uN) -> ()`;
    /// the payload is the stored integer's bit width.
    Store(u32),
}

impl RawMemInline {
    pub(super) fn from_func(func: &FuncRef, cx: &LowerCtx<'_, '_>) -> Option<Self> {
        let FuncRef::Extern { name, .. } = func else {
            return None;
        };
        let ptr_bits = pointer_width(cx.arch);
        match cx.interner.resolve(*name) {
            "__edda_load_u8" => Some(Self::Load(8)),
            "__edda_load_u16" => Some(Self::Load(16)),
            "__edda_load_u32" => Some(Self::Load(32)),
            "__edda_load_u64" => Some(Self::Load(64)),
            "__edda_load_usize" => Some(Self::Load(ptr_bits)),
            "__edda_store_u8" => Some(Self::Store(8)),
            "__edda_store_u16" => Some(Self::Store(16)),
            "__edda_store_u32" => Some(Self::Store(32)),
            "__edda_store_u64" => Some(Self::Store(64)),
            "__edda_store_usize" => Some(Self::Store(ptr_bits)),
            _ => None,
        }
    }
}

/// Classify a `{ ptr }` single-pointer-field struct type — the shape a
/// `Box` value takes at an alloc-family call boundary.
fn single_ptr_struct(ty: &BasicTypeEnum<'_>) -> bool {
    match ty {
        BasicTypeEnum::StructType(s) => {
            s.count_fields() == 1
                && matches!(s.get_field_type_at_index(0), Some(BasicTypeEnum::PointerType(_)))
        }
        _ => false,
    }
}

/// Coerce one call argument to the callee's declared parameter type
/// when the two disagree but occupy the same register class.
pub(super) fn coerce_to_param_type<'ctx>(
    value: BasicMetadataValueEnum<'ctx>,
    param_ty: &BasicTypeEnum<'ctx>,
    cx: &LowerCtx<'ctx, '_>,
) -> BasicMetadataValueEnum<'ctx> {
    match (value, param_ty) {
        (BasicMetadataValueEnum::PointerValue(p), BasicTypeEnum::IntType(t)) => cx
            .builder
            .build_ptr_to_int(p, *t, "abi.coerce")
            .expect("ptrtoint arg coercion in a positioned block")
            .into(),
        (BasicMetadataValueEnum::IntValue(v), BasicTypeEnum::PointerType(t)) => cx
            .builder
            .build_int_to_ptr(v, *t, "abi.coerce")
            .expect("inttoptr arg coercion in a positioned block")
            .into(),
        (BasicMetadataValueEnum::StructValue(s), BasicTypeEnum::IntType(t))
            if single_ptr_struct(&s.get_type().into()) =>
        {
            let inner = cx
                .builder
                .build_extract_value(s, 0, "abi.coerce.ptr")
                .expect("extractvalue single-ptr struct field 0")
                .into_pointer_value();
            cx.builder
                .build_ptr_to_int(inner, *t, "abi.coerce")
                .expect("ptrtoint extracted struct pointer")
                .into()
        }
        (BasicMetadataValueEnum::StructValue(s), BasicTypeEnum::PointerType(_))
            if single_ptr_struct(&s.get_type().into()) =>
        {
            cx.builder
                .build_extract_value(s, 0, "abi.coerce.ptr")
                .expect("extractvalue single-ptr struct field 0")
                .into_pointer_value()
                .into()
        }
        (BasicMetadataValueEnum::IntValue(v), BasicTypeEnum::StructType(st))
            if single_ptr_struct(param_ty) =>
        {
            let as_ptr = cx
                .builder
                .build_int_to_ptr(v, cx.context.ptr_type(AddressSpace::default()), "abi.coerce")
                .expect("inttoptr int arg into struct pointer field");
            let agg = cx
                .builder
                .build_insert_value(st.get_poison(), as_ptr, 0, "abi.coerce.box")
                .expect("insertvalue pointer into single-ptr struct");
            BasicValueEnum::StructValue(agg.into_struct_value()).into()
        }
        (BasicMetadataValueEnum::PointerValue(p), BasicTypeEnum::StructType(st))
            if single_ptr_struct(param_ty) =>
        {
            let agg = cx
                .builder
                .build_insert_value(st.get_poison(), p, 0, "abi.coerce.box")
                .expect("insertvalue pointer into single-ptr struct");
            BasicValueEnum::StructValue(agg.into_struct_value()).into()
        }
        (value, _) => value,
    }
}
