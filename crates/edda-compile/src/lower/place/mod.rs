//! MIR [`Place`] -> inkwell `PointerValue` (read or write).
//!
//! Zero-projection places are always admitted. The
//! `Projection::Field(idx)` walker handles tuple roots and
//! product-ADT roots.
//! `Projection::VariantDowncast(v)` does not GEP its own field —
//! it shifts the current type from the sum to the chosen variant's
//! payload and advances the pointer from the sum's outer struct to
//! the payload slot (field 1 of the `{ tag, payload }` shape). After
//! a `VariantDowncast`, a subsequent `Field(i)` walks the variant's
//! payload struct directly.
//!
//! `Projection::Index` is admitted for slice roots — load the
//! slice's data pointer (field 0), GEP by the index local (scaled by
//! the element's natural size), and produce a leaf pointer to the
//! indexed element. The GEP is non-inbounds; bounds-checking is the
//! typechecker's responsibility, not the LLVM walker's.
//!
//! `Projection::Deref(T)` reads through a `HeapPtr` leaf: it loads the
//! pointer value out of the current slot and treats that pointer as the
//! address of a `T`-typed value. Because `lower_place_read` then loads
//! the leaf via [`inkwell_basic_type`], an *aggregate* `T` (record /
//! sum / slice) is loaded whole — not just a scalar word. This is the
//! MIR-native read-through-pointer for aggregates.

use edda_mir::{Idx, MirType, Place, Projection};
use inkwell::values::{BasicValueEnum, PointerValue};

use crate::error::CompileError;

use super::ty::inkwell_basic_type;
use super::LowerCtx;

mod resolve;
mod walk;

#[cfg(test)]
mod tests;

pub(in crate::lower) use resolve::place_leaf_ty;

use walk::{walk_deref, walk_field, walk_index, walk_variant_downcast};

/// Emit `load <leaf-ty>, ptr <gep-chain>` to read out of a place.
/// Walks the projection chain and loads the leaf value.
pub(crate) fn lower_place_read<'ctx>(
    place: &Place,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    let (leaf_ptr, leaf_ty) = walk_place(place, cx)?;
    let llvm_ty = inkwell_basic_type(cx.context, &leaf_ty, cx.program, cx.arch).ok_or_else(|| {
        CompileError::UnsupportedMirShape {
            shape: "non-lowerable-leaf-type",
            detail: format!(
                "body {:?} reads a Place whose leaf type {:?} has no LLVM lowering",
                cx.body_name, leaf_ty.kind
            ),
        }
    })?;
    let name = format!("v{}", place.local.index());
    let loaded = cx
        .builder
        .build_load(llvm_ty, leaf_ptr, &name)
        .expect("build_load from a positioned block must succeed");
    Ok(loaded)
}

/// Resolve a [`Place`] to the leaf pointer after walking projections.
/// Used by `Assign` lowering: the caller stores the rvalue into this
/// pointer directly.
pub(crate) fn lower_place_pointer<'ctx>(
    place: &Place,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<PointerValue<'ctx>, CompileError> {
    let (leaf_ptr, _leaf_ty) = walk_place(place, cx)?;
    Ok(leaf_ptr)
}

/// Walk a [`Place`]'s projection chain and return the leaf
/// `(pointer, MirType)` pair.
///
/// - `Projection::Field`: GEP into the current struct's field, using
///   the struct type derived from `current_ty`.
/// - `Projection::VariantDowncast(v)`: GEP into the sum's payload
///   slot, then replace `current_ty` with a synthetic
///   `MirTypeKind::Tuple` of the variant's field types — subsequent
///   `Field` projections walk the variant's payload as if it were a
///   tuple. The synthetic shape sidesteps adding a richer "current
///   shape" enum at the cost of dropping the variant's packing
///   policy (deferred per `abi-and-layout.md` §4).
/// - `Projection::Index`: GEPs through the slice's data pointer.
/// - `Projection::Deref(T)`: load the `HeapPtr` value out of the
///   current slot and continue from that pointer with leaf type `T` —
///   the read-through-pointer step.
fn walk_place<'ctx>(
    place: &Place,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<(PointerValue<'ctx>, MirType), CompileError> {
    let (mut ptr, mut current_ty) = place_root(place, cx)?;
    for (step_idx, proj) in place.projection.iter().enumerate() {
        let (next_ptr, next_ty) = match proj {
            Projection::Field(field_idx) => walk_field(ptr, &current_ty, *field_idx, cx, step_idx)?,
            Projection::VariantDowncast(variant_idx) => {
                walk_variant_downcast(ptr, &current_ty, *variant_idx, cx, step_idx)?
            }
            Projection::Index(idx_local) => walk_index(ptr, &current_ty, *idx_local, cx, step_idx)?,
            Projection::Deref(pointee_ty) => walk_deref(ptr, &current_ty, pointee_ty, cx, step_idx)?,
        };
        ptr = next_ptr;
        current_ty = next_ty;
    }
    Ok((ptr, current_ty))
}

/// Resolve a [`Place`]'s root local to `(alloca_ptr, root_ty)`.
fn place_root<'ctx>(
    place: &Place,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<(PointerValue<'ctx>, MirType), CompileError> {
    let local_decl = cx
        .body
        .locals
        .get(place.local)
        .expect("Place::local indexes a real local; MIR validation guarantees this");
    let ptr = cx.locals[place.local.index()].ok_or_else(|| {
        CompileError::UnsupportedMirShape {
            shape: "non-lowerable-local",
            detail: format!(
                "body {:?} touches local#{} of type {:?}; ADT / Slice / \
                 Capability locals are not yet lowerable",
                cx.body_name,
                place.local.index(),
                local_decl.ty.kind
            ),
        }
    })?;
    Ok((ptr, local_decl.ty.clone()))
}
