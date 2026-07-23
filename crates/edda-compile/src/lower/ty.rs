//! Shared MIR-type -> inkwell-type adapters and natural-size proxies.
//!
//! Every submodule of the `lower/` walker reaches for these helpers,
//! which is why they live in their own focused module rather than
//! buried in `mod.rs` alongside [`super::LowerCtx`]. Two concerns:
//!
//! - **Type lowering** ([`inkwell_prim_type`], [`inkwell_basic_type`])
//!   — pure conversions from a [`MirPrim`] / [`MirType`] to the
//!   corresponding inkwell [`BasicTypeEnum`]. Primitives (including
//!   `Str` as a fat pointer), tuples, product ADTs, sum ADTs, slices,
//!   and fn-pointers (as opaque `ptr`) admit; capabilities, `Unit`,
//!   and `Never` return `None` (the caller produces a precise
//!   `UnsupportedMirShape`).
//! - **Sum-variant layout** ([`sum_max_variant_payload_type`],
//!   [`sum_variant_payload_type`], [`compute_variant_size`],
//!   [`compute_type_size`]) — the alignment-aware ABI size that picks
//!   the representative payload variant for a sum ADT's `{ tag,
//!   max-variant-payload }` LLVM shape per `backend-choice.md` §6.2 and
//!   classifies the win64 register-vs-indirect boundary. The size is
//!   byte-accurate against the LLVM struct layout [`inkwell_basic_type`]
//!   emits (alignment padding included) so both consumers see the size
//!   LLVM does. `@align(...)` overrides
//!   from `abi-and-layout.md` §4 still land separately.

use edda_mir::{AdtDef, AdtKind, LayoutPolicy, MirPrim, MirProgram, MirType, MirTypeKind};
use edda_target::Arch;
use inkwell::AddressSpace;
use inkwell::context::Context;
use inkwell::types::{BasicTypeEnum, StructType};

use crate::mir_prim::{align_of_prim, size_of_prim};
use crate::target_info::pointer_width;

/// Map a [`MirPrim`] to the matching inkwell scalar type. `None` for
/// [`MirPrim::Str`] (a slice, not a single scalar).
pub(crate) fn inkwell_prim_type<'ctx>(
    context: &'ctx Context,
    prim: MirPrim,
    arch: Arch,
) -> Option<BasicTypeEnum<'ctx>> {
    let ty: BasicTypeEnum<'ctx> = match prim {
        MirPrim::I8 | MirPrim::U8 => context.i8_type().into(),
        MirPrim::I16 | MirPrim::U16 => context.i16_type().into(),
        MirPrim::I32 | MirPrim::U32 | MirPrim::Char => context.i32_type().into(),
        MirPrim::I64 | MirPrim::U64 => context.i64_type().into(),
        MirPrim::I128 | MirPrim::U128 => context.i128_type().into(),
        MirPrim::F32 => context.f32_type().into(),
        MirPrim::F64 => context.f64_type().into(),
        MirPrim::Bool => context.bool_type().into(),
        MirPrim::Usize | MirPrim::Isize => {
            if pointer_width(arch) == 32 {
                context.i32_type().into()
            } else {
                context.i64_type().into()
            }
        }
        // Opaque pointer at LLVM 15+ — no element type at the IR level.
        // Pointee size/align are passed explicitly to the allocation
        // intrinsics that produce/consume `HeapPtr` values.
        MirPrim::HeapPtr => context.ptr_type(AddressSpace::default()).into(),
        MirPrim::Str => return None,
    };
    Some(ty)
}

/// LLVM struct type for the unified fat function value
/// `{ code: ptr, env: usize }`. Used by [`inkwell_basic_type`]'s
/// `FnPtr` arm and by the `FunctionRef` / `MakeClosure` rvalue lowering
/// that constructs the value.
pub(crate) fn fn_value_struct_type<'ctx>(context: &'ctx Context, arch: Arch) -> StructType<'ctx> {
    let code_ty: BasicTypeEnum<'ctx> = context.ptr_type(AddressSpace::default()).into();
    let env_ty = inkwell_prim_type(context, MirPrim::Usize, arch).expect("usize is always lowerable");
    context.struct_type(&[code_ty, env_ty], false)
}

/// Convert a supported [`MirType`] to an inkwell
/// [`BasicTypeEnum`]. Primitives and tuples-of-admitted-types,
/// product (`AdtKind::Product`) ADTs, and sum (`AdtKind::Sum`) ADTs
/// whose variants' fields are themselves admitted are all supported,
/// as are slices and the `Str` primitive
/// (both as fat-pointer `{ ptr, isize }` structs).
/// Capabilities, function pointers, `Unit`, and `Never` still return
/// `None`; the caller is responsible for producing a
/// [`CompileError::UnsupportedMirShape`](crate::CompileError::UnsupportedMirShape)
/// with an informative label.
pub(crate) fn inkwell_basic_type<'ctx>(
    context: &'ctx Context,
    ty: &MirType,
    program: &MirProgram,
    arch: Arch,
) -> Option<BasicTypeEnum<'ctx>> {
    match &ty.kind {
        // Str (String) is a fat pointer { ptr, isize } — handled before the
        // general Prim arm so MirPrim::Str doesn't fall through to inkwell_prim_type
        // which returns None for it.
        MirTypeKind::Prim(MirPrim::Str) => {
            let ptr_ty: BasicTypeEnum<'ctx> = context.ptr_type(AddressSpace::default()).into();
            let len_ty = inkwell_prim_type(context, MirPrim::Usize, arch)
                .expect("usize is always lowerable");
            Some(context.struct_type(&[ptr_ty, len_ty], false).into())
        }
        MirTypeKind::Prim(prim) => inkwell_prim_type(context, *prim, arch),
        MirTypeKind::Tuple(elems) => {
            let mut field_tys: Vec<BasicTypeEnum<'ctx>> = Vec::with_capacity(elems.len());
            for elem in elems {
                field_tys.push(inkwell_basic_type(context, elem, program, arch)?);
            }
            Some(context.struct_type(&field_tys, false).into())
        }
        MirTypeKind::Adt(adt_id) => {
            let adt = program.adts.get(*adt_id)?;
            match adt.kind {
                AdtKind::Product => {
                    // Products carry exactly one variant (invariant on AdtDef).
                    let variant = adt.variants.first()?;
                    let mut field_tys: Vec<BasicTypeEnum<'ctx>> =
                        Vec::with_capacity(variant.fields.len());
                    for field in &variant.fields {
                        field_tys.push(inkwell_basic_type(context, &field.ty, program, arch)?);
                    }
                    let packed = matches!(adt.layout.policy, LayoutPolicy::Packed);
                    Some(context.struct_type(&field_tys, packed).into())
                }
                AdtKind::Sum => {
                    let tag_prim = adt.tag_width?;
                    let tag_ty = inkwell_prim_type(context, tag_prim, arch)?;
                    let payload_ty = sum_max_variant_payload_type(context, adt, program, arch)?;
                    let packed = matches!(adt.layout.policy, LayoutPolicy::Packed);
                    Some(
                        context
                            .struct_type(&[tag_ty, payload_ty.into()], packed)
                            .into(),
                    )
                }
            }
        }
        MirTypeKind::Slice(_) => {
            // Fat pointer `{ ptr, isize }`. Element type does not appear in
            // the IR-level type (LLVM 15+ opaque pointers).
            let ptr_ty: BasicTypeEnum<'ctx> = context.ptr_type(AddressSpace::default()).into();
            let len_ty = inkwell_prim_type(context, MirPrim::Usize, arch)
                .expect("usize is always lowerable");
            Some(context.struct_type(&[ptr_ty, len_ty], false).into())
        }
        // Fn-pointers lower to the unified fat function value
        // `{ code: ptr, env: usize }`.
        // The code pointer is opaque (LLVM 15+); the env word is a
        // pointer-width integer holding an inline capture by value (or
        // a pointer to a larger env in a future slice). The callee's
        // user-facing signature payload is consulted only at the
        // indirect-call site, which extracts `code` + `env` and
        // prepends `env` as the leading actual argument.
        MirTypeKind::FnPtr(_) => Some(fn_value_struct_type(context, arch).into()),
        // Capability handles lower to an opaque pointer — the same wire
        // shape `lower_fn_sig` uses for capability params (one ptr per
        // slot, kind discriminator is type-system bookkeeping only).
        // Required so ADT fields and variant payloads that store a
        // value-position capability (e.g. `Option_Stdin.some(value: h)`,
        // `FsHandle.read_only(h)`) reach a real LLVM type instead of the
        // `non-lowerable-aggregate-return` rejection.
        MirTypeKind::Capability(_) => {
            Some(context.ptr_type(AddressSpace::default()).into())
        }
        _ => None,
    }
}

/// Build the LLVM struct type for a sum ADT's payload slot — the
/// variant with the largest natural byte size.
///
/// Variants smaller than the max are zero-extended at construction
/// time (the alloca round-trip in `lower_make_variant` leaves trailing
/// payload bytes undef) and reinterpreted on read via a load typed at
/// the variant's own payload struct.
pub(crate) fn sum_max_variant_payload_type<'ctx>(
    context: &'ctx Context,
    adt: &AdtDef,
    program: &MirProgram,
    arch: Arch,
) -> Option<StructType<'ctx>> {
    let packed = matches!(adt.layout.policy, LayoutPolicy::Packed);
    let mut best_index: usize = 0;
    let mut best_size: u32 = 0;
    for (i, variant) in adt.variants.iter().enumerate() {
        let size = compute_variant_size(variant, packed, program, arch);
        if i == 0 || size > best_size {
            best_size = size;
            best_index = i;
        }
    }
    sum_variant_payload_type(context, adt, best_index, program, arch)
}

/// Build the LLVM payload struct for one specific variant of a sum
/// ADT. Used by [`sum_max_variant_payload_type`] to pick the
/// representative shape and by [`super::rvalue::lower_make_variant`] /
/// [`super::rvalue::lower_extract_field`] to construct and read back
/// the per-variant payload.
pub(crate) fn sum_variant_payload_type<'ctx>(
    context: &'ctx Context,
    adt: &AdtDef,
    variant_index: usize,
    program: &MirProgram,
    arch: Arch,
) -> Option<StructType<'ctx>> {
    let variant = adt.variants.get(variant_index)?;
    let mut field_tys: Vec<BasicTypeEnum<'ctx>> = Vec::with_capacity(variant.fields.len());
    for field in &variant.fields {
        field_tys.push(inkwell_basic_type(context, &field.ty, program, arch)?);
    }
    let packed = matches!(adt.layout.policy, LayoutPolicy::Packed);
    Some(context.struct_type(&field_tys, packed))
}

/// Byte size of a variant's payload struct. Used to pick the largest
/// variant when building a sum ADT's payload slot.
pub(crate) fn compute_variant_size(
    variant: &edda_mir::VariantDef,
    packed: bool,
    program: &MirProgram,
    arch: Arch,
) -> u32 {
    variant_layout(variant, packed, program, arch).0
}

/// Byte size of `ty` in the LLVM layout the backend emits — alignment
/// padding included, matching what LLVM computes for the same
/// [`inkwell_basic_type`] struct. Drives win64 ABI classification and
/// sum max-variant selection.
pub(crate) fn compute_type_size(ty: &MirType, program: &MirProgram, arch: Arch) -> u32 {
    type_layout(ty, program, arch).0
}

/// Natural alignment in bytes of `ty` in the LLVM layout the backend
/// emits, mirroring [`compute_type_size`]'s size counterpart. Used to
/// lay out a sequence of typed values (e.g. a `scope(exec)` spawn
/// body's packed argument buffer) at correctly-aligned offsets.
pub(crate) fn compute_type_align(ty: &MirType, program: &MirProgram, arch: Arch) -> u32 {
    type_layout(ty, program, arch).1
}

/// Conservative alignment of a sum/aggregate stack slot built by the
/// byte-preserving construction paths (`build_variant_in_alloca`,
/// `build_variant_value`, `extract`). The max-variant payload blob is
/// laid out in `i64`-sized chunks, so the slot is never more than
/// 8-byte aligned even when a variant field is wider.
pub(crate) const AGGREGATE_ACCESS_ALIGN: u32 = 8;

/// Alignment to attach to a load/store of `ty` through a pointer into
/// a conservatively-aligned sum-payload slot: `ty`'s natural alignment
/// capped at [`AGGREGATE_ACCESS_ALIGN`].
pub(crate) fn sum_slot_access_align(ty: &MirType, program: &MirProgram, arch: Arch) -> u32 {
    type_layout(ty, program, arch).1.min(AGGREGATE_ACCESS_ALIGN)
}

/// LLVM ABI `(size, align)` in bytes of `ty`, mirroring the struct
/// layout [`inkwell_basic_type`] produces. The single source of truth
/// for [`compute_type_size`] / [`compute_variant_size`].
fn type_layout(ty: &MirType, program: &MirProgram, arch: Arch) -> (u32, u32) {
    let ptr_bytes = pointer_width(arch) / 8;
    match &ty.kind {
        // `{ ptr, usize }` fat pointers — String, slices, and fn-values.
        MirTypeKind::Prim(MirPrim::Str) | MirTypeKind::Slice(_) | MirTypeKind::FnPtr(_) => {
            (2 * ptr_bytes, ptr_bytes)
        }
        // A single opaque pointer.
        MirTypeKind::Capability(_) => (ptr_bytes, ptr_bytes),
        MirTypeKind::Prim(prim) => (
            size_of_prim(*prim, arch).unwrap_or(0),
            align_of_prim(*prim, arch).unwrap_or(1),
        ),
        MirTypeKind::Tuple(elems) => {
            struct_layout(elems.iter().map(|e| type_layout(e, program, arch)), false)
        }
        MirTypeKind::Adt(adt_id) => adt_layout(*adt_id, program, arch),
        _ => (0, 1),
    }
}

/// LLVM ABI `(size, align)` of an ADT, dispatching on Product vs Sum.
fn adt_layout(adt_id: edda_mir::AdtId, program: &MirProgram, arch: Arch) -> (u32, u32) {
    let Some(adt) = program.adts.get(adt_id) else {
        return (0, 1);
    };
    let packed = matches!(adt.layout.policy, LayoutPolicy::Packed);
    match adt.kind {
        AdtKind::Product => match adt.variants.first() {
            Some(variant) => variant_layout(variant, packed, program, arch),
            None => (0, 1),
        },
        AdtKind::Sum => {
            let tag = adt
                .tag_width
                .map(|p| {
                    (
                        size_of_prim(p, arch).unwrap_or(0),
                        align_of_prim(p, arch).unwrap_or(1),
                    )
                })
                .unwrap_or((0, 1));
            let payload = sum_payload_layout(adt, packed, program, arch);
            struct_layout([tag, payload].into_iter(), packed)
        }
    }
}

/// `(size, align)` of a sum's payload slot: the largest variant's
/// payload struct.
fn sum_payload_layout(adt: &AdtDef, packed: bool, program: &MirProgram, arch: Arch) -> (u32, u32) {
    let mut best = (0u32, 1u32);
    for (i, variant) in adt.variants.iter().enumerate() {
        let layout = variant_layout(variant, packed, program, arch);
        if i == 0 || layout.0 > best.0 {
            best = layout;
        }
    }
    best
}

/// `(size, align)` of a variant's payload struct.
fn variant_layout(
    variant: &edda_mir::VariantDef,
    packed: bool,
    program: &MirProgram,
    arch: Arch,
) -> (u32, u32) {
    struct_layout(
        variant.fields.iter().map(|f| type_layout(&f.ty, program, arch)),
        packed,
    )
}

/// C-style `(size, align)` of a struct over `fields` (each a
/// `(size, align)` pair).
fn struct_layout(fields: impl Iterator<Item = (u32, u32)>, packed: bool) -> (u32, u32) {
    let mut offset: u32 = 0;
    let mut align: u32 = 1;
    for (field_size, field_align) in fields {
        let effective_align = if packed { 1 } else { field_align };
        offset = round_up_to(offset, effective_align).saturating_add(field_size);
        align = align.max(effective_align);
    }
    (round_up_to(offset, align), align)
}

/// Round `n` up to the next multiple of `align`.
fn round_up_to(n: u32, align: u32) -> u32 {
    if align <= 1 {
        return n;
    }
    n.saturating_add(align - 1) / align * align
}
