//! Target-aware size + alignment for [`MirType`] values.
//!
//! Replicates the layout logic that lives in `edda-compile::mir_prim`
//! (which only handles [`MirPrim`]) and extends it to compound types
//! (ADTs, tuples, slices) so the alloc-family call rewrite can thread
//! `size_of(T)` / `align_of(T)` as `u64` constants without depending on
//! the LLVM backend.
//!
//! Used exclusively by [`super::call`]'s alloc-family rewrite — the
//! result feeds into [`crate::ConstValue::Uint`] operands prepended at
//! the call site. Pointer-width-bearing primitives (`HeapPtr`, `Usize`,
//! `Isize`) take their width from
//! [`super::LoweringContext::pointer_width_bytes`].
//!
//! Layout rules:
//! - **Primitive**: target-aware size = align (pointer-width for
//!   `HeapPtr` / `Usize` / `Isize`, fixed for everything else).
//! - **Tuple** / **Product** ADT: standard C-style struct layout —
//!   align up to each field's alignment, add the field's size, repeat;
//!   final size rounds up to the struct's max-field alignment.
//! - **Sum** ADT: tag bytes (per [`AdtDef::tag_width`]) followed by the
//!   single largest-by-size variant payload — BOTH the payload size and
//!   its alignment come from that one representative variant, matching
//!   `edda-compile`'s `{ tag, <repr-variant-struct> }` LLVM layout. The
//!   payload offset and final size align to the union of (tag alignment,
//!   representative-variant alignment). Maxing the payload alignment
//!   independently of the size winner diverged from the LLVM struct and
//!   mis-sized `Box`/`Vec`-stored sums.
//! - **Slice** `[T]`: fat pointer = `2 * pointer_width`, align =
//!   `pointer_width`. The element type does not affect the slice value
//!   itself (only the heap region the slice points at).
//! - **Unit** / **Never**: `(0, 1)` — zero-sized with byte alignment.
//! - **Capability** / **FnPtr**: opaque pointer-width value.
//!
//! `MirPrim::Str` sizes the same way `Slice` does (`2 *
//! pointer_width`, align `pointer_width`) — `str` is just a slice with
//! a UTF-8 invariant. The runtime-side `EdStr` confirms this layout
//! (`{ ptr, len }`). Diverges from `edda-compile::mir_prim::size_of_prim`
//! which returns `None` for `Str` to signal "the LLVM emitter
//! constructs the fat pointer separately"; the alloc-family rewrite
//! needs the in-memory size, not the LLVM-emit hint.

use crate::adt::{AdtDef, AdtKind};
use crate::program::MirProgram;
use crate::ty::{MirPrim, MirType, MirTypeKind};

/// Compute `(size, align)` in bytes for `ty` against a target whose
/// pointer width is `pointer_width_bytes`.
pub(super) fn compute_size_align(
    ty: &MirType,
    pointer_width_bytes: u32,
    program: &MirProgram,
) -> Option<(u64, u64)> {
    match &ty.kind {
        MirTypeKind::Prim(prim) => prim_size_align(*prim, pointer_width_bytes),
        MirTypeKind::Unit | MirTypeKind::Never => Some((0, 1)),
        MirTypeKind::Slice(_) => {
            let pw = pointer_width_bytes as u64;
            Some((2 * pw, pw))
        }
        MirTypeKind::Capability(_) => {
            let pw = pointer_width_bytes as u64;
            Some((pw, pw))
        }
        // Unified fat function value `{ code: ptr, env: usize }` —
        // two pointer-width words,
        // pointer-aligned.
        MirTypeKind::FnPtr(_) => {
            let pw = pointer_width_bytes as u64;
            Some((2 * pw, pw))
        }
        MirTypeKind::Tuple(elems) => product_layout(elems, pointer_width_bytes, program),
        MirTypeKind::Adt(adt_id) => {
            let adt = program.adts.get(*adt_id)?;
            adt_size_align(adt, pointer_width_bytes, program)
        }
    }
}

/// Per-primitive `(size, align)`. Mirrors the table in
/// `edda-compile::mir_prim::size_of_prim` / `align_of_prim` and extends
/// it with the in-memory size of `Str` — diverging from
/// `size_of_prim` which returns `None` for `Str` because the LLVM
/// emitter assembles the fat pointer separately. At the alloc-family
/// call seam we need to *size* `Str` (a struct field containing a
/// string is two pointers wide), so the entry is filled in here.
fn prim_size_align(prim: MirPrim, pointer_width_bytes: u32) -> Option<(u64, u64)> {
    let (size, align): (u64, u64) = match prim {
        MirPrim::I8 | MirPrim::U8 | MirPrim::Bool => (1, 1),
        MirPrim::I16 | MirPrim::U16 => (2, 2),
        MirPrim::I32 | MirPrim::U32 | MirPrim::F32 | MirPrim::Char => (4, 4),
        MirPrim::I64 | MirPrim::U64 | MirPrim::F64 => (8, 8),
        MirPrim::I128 | MirPrim::U128 => (16, 16),
        MirPrim::Usize | MirPrim::Isize | MirPrim::HeapPtr => {
            (pointer_width_bytes as u64, pointer_width_bytes as u64)
        }
        // `Str` is a fat pointer `(ptr, len)` — same wire shape as
        // `Slice` (see `runtime/src/lib.rs::EdStr`).
        MirPrim::Str => (2 * pointer_width_bytes as u64, pointer_width_bytes as u64),
    };
    Some((size, align))
}

/// Round `offset` up to the next multiple of `align`. Standard
/// `align_up` rule that the C ABI uses for struct field placement.
fn align_to(offset: u64, align: u64) -> u64 {
    debug_assert!(align >= 1 && align.is_power_of_two(), "align must be a non-zero power of two");
    (offset + align - 1) & !(align - 1)
}

fn adt_size_align(
    adt: &AdtDef,
    pointer_width_bytes: u32,
    program: &MirProgram,
) -> Option<(u64, u64)> {
    match adt.kind {
        AdtKind::Product => {
            let variant = adt.variants.first()?;
            let field_tys: Vec<MirType> = variant.fields.iter().map(|f| f.ty.clone()).collect();
            product_layout(field_tys.as_slice(), pointer_width_bytes, program)
        }
        AdtKind::Sum => sum_layout(adt, pointer_width_bytes, program),
    }
}

/// Layout for a product / tuple-like aggregate.
fn product_layout(
    fields: &[MirType],
    pointer_width_bytes: u32,
    program: &MirProgram,
) -> Option<(u64, u64)> {
    let mut offset: u64 = 0;
    let mut struct_align: u64 = 1;
    for field_ty in fields {
        let (fs, fa) = compute_size_align(field_ty, pointer_width_bytes, program)?;
        offset = align_to(offset, fa);
        offset += fs;
        if fa > struct_align {
            struct_align = fa;
        }
    }
    let total = align_to(offset, struct_align);
    Some((total, struct_align))
}

/// Layout for a sum-tagged ADT: `tag` followed by the single
/// largest-by-size variant payload, both size and align taken from
/// that representative variant. Mirrors
/// [`edda_compile::lower::ty`]'s `sum_payload_layout`, so the
/// alloc-family `size_of`/`align_of` constants match the LLVM struct
/// the backend lays out for the same sum.
fn sum_layout(
    adt: &AdtDef,
    pointer_width_bytes: u32,
    program: &MirProgram,
) -> Option<(u64, u64)> {
    let tag = adt.tag_width?;
    let (tag_size, tag_align) = prim_size_align(tag, pointer_width_bytes)?;
    let (payload_size, payload_align) =
        representative_variant_layout(adt, pointer_width_bytes, program)?;
    let struct_align = tag_align.max(payload_align);
    let payload_offset = align_to(tag_size, payload_align);
    let total = align_to(payload_offset + payload_size, struct_align);
    Some((total, struct_align))
}

/// `(size, align)` of a sum's payload slot: the single largest-by-size
/// variant's product layout. Both components come from that one
/// representative variant so the result equals the LLVM payload struct
/// `edda-compile` emits.
fn representative_variant_layout(
    adt: &AdtDef,
    pointer_width_bytes: u32,
    program: &MirProgram,
) -> Option<(u64, u64)> {
    let mut best: (u64, u64) = (0, 1);
    for (i, variant) in adt.variants.iter().enumerate() {
        let field_tys: Vec<MirType> = variant.fields.iter().map(|f| f.ty.clone()).collect();
        let layout = product_layout(field_tys.as_slice(), pointer_width_bytes, program)?;
        if i == 0 || layout.0 > best.0 {
            best = layout;
        }
    }
    Some(best)
}
