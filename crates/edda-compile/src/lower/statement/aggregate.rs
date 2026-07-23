//! Byte-preserving aggregate-copy helpers for the `Assign` walker.
//!
//! These predicates and the `build_aggregate_in_place` constructor back
//! the memcpy paths in [`super::lower_statement`] that keep cross-variant
//! padding bytes which LLVM's typed load/store would otherwise drop.

use edda_mir::{Idx, MirPrim, MirType, MirTypeKind, Operand};
use inkwell::types::BasicType;

use crate::error::CompileError;

use super::super::place::lower_place_pointer;
use super::super::ty::inkwell_basic_type;
use super::super::LowerCtx;

/// Conservative alignment used for the `build_memcpy` paths that
/// preserve cross-variant bytes on aggregate copies (see the
/// MakeVariant + aggregate-Use paths in [`super::lower_statement`]).
pub(super) const AGGREGATE_COPY_ALIGN: u32 = 8;

/// Mirror of the `Operand::Unit` / no-alloca-local pair handled by
/// `lower_operand`: returns `true` when reading this operand would yield
/// `Ok(None)`.
pub(super) fn operand_yields_no_value(operand: &Operand, cx: &LowerCtx<'_, '_>) -> bool {
    match operand {
        Operand::Unit => true,
        Operand::Copy(place) | Operand::Move(place) => {
            cx.locals[place.local.index()].is_none()
        }
        Operand::Const(_) => false,
    }
}

/// When `operand` reads an aggregate-typed local (whose `{ tag, payload }`
/// shape may have cross-variant padding that LLVM's typed
/// load/store would otherwise drop), return a pointer to the
/// source memory so the `Assign` caller can `memcpy` instead.
fn aggregate_source_ptr<'ctx>(
    operand: &Operand,
    cx: &LowerCtx<'ctx, '_>,
) -> Option<inkwell::values::PointerValue<'ctx>> {
    let place = match operand {
        Operand::Copy(p) | Operand::Move(p) => p,
        _ => return None,
    };
    if !place.projection.is_empty() {
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

/// Aggregate-source extraction that handles both bare and projected
/// `Copy`/`Move` operands. Returns `Ok(Some(ptr))` when the operand
/// has a stable in-memory source the caller can memcpy from, and
/// `Ok(None)` for non-Place operands (Const / Unit) which the caller
/// should resolve through the SSA path. Errors propagate from
/// `lower_place_pointer` (the GEP-walker) when a projection cannot
/// be lowered.
pub(super) fn aggregate_use_source_ptr<'ctx>(
    operand: &Operand,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<Option<inkwell::values::PointerValue<'ctx>>, CompileError> {
    let place = match operand {
        Operand::Copy(p) | Operand::Move(p) => p,
        _ => return Ok(None),
    };
    if place.projection.is_empty() {
        return Ok(cx.locals[place.local.index()]);
    }
    Ok(Some(lower_place_pointer(place, cx)?))
}

/// Whether `ty` represents an in-memory aggregate whose typed
/// load/store would decompose into per-field operations and risk
/// dropping bytes for smaller-variant sum payloads. Tuples / products
/// of fixed scalar fields also qualify because they may *contain* a
/// sum field whose actual variant is smaller than its max-variant
/// slot; the safe rule is to memcpy any aggregate-typed transfer.
pub(super) fn rvalue_ty_is_aggregate(ty: &MirType) -> bool {
    matches!(
        &ty.kind,
        MirTypeKind::Prim(MirPrim::Str)
            | MirTypeKind::Slice(_)
            | MirTypeKind::Tuple(_)
            | MirTypeKind::Adt(_)
            // Fat function value `{ code, env }`
            | MirTypeKind::FnPtr(_)
    )
}

/// Resolve the subject of an `ExtractField` to a `(pointer, MirType)`
/// pair we can GEP into for a byte-faithful memcpy. The MIR type is
/// the outer aggregate's type so the caller can rebuild the LLVM
/// struct type for the GEP. Handles only projection-free Place
/// operands today — projected subjects fall back to the SSA path
/// (sound for non-sum tuples; lossy for nested sums in a follow-up
/// slice).
pub(super) fn aggregate_extract_subject_ptr<'ctx>(
    subject: &Operand,
    cx: &LowerCtx<'ctx, '_>,
) -> Option<(inkwell::values::PointerValue<'ctx>, MirType)> {
    let place = match subject {
        Operand::Copy(p) | Operand::Move(p) => p,
        _ => return None,
    };
    if !place.projection.is_empty() {
        return None;
    }
    let decl = cx.body.locals.get(place.local)?;
    let is_struct_aggregate = matches!(
        &decl.ty.kind,
        MirTypeKind::Tuple(_) | MirTypeKind::Adt(_)
    );
    if !is_struct_aggregate {
        return None;
    }
    let ptr = cx.locals[place.local.index()]?;
    Some((ptr, decl.ty.clone()))
}

/// Whether the operand's source value is an aggregate that would lose
/// bytes through a typed load/store round-trip. Used to detect when a
/// `MakeTuple` / `MakeRecord` field needs byte-preserving treatment.
pub(super) fn operand_is_aggregate(operand: &Operand, cx: &LowerCtx<'_, '_>) -> bool {
    let place = match operand {
        Operand::Copy(p) | Operand::Move(p) => p,
        _ => return false,
    };
    if !place.projection.is_empty() {
        return false;
    }
    cx.body
        .locals
        .get(place.local)
        .map(|d| {
            matches!(
                &d.ty.kind,
                MirTypeKind::Prim(MirPrim::Str)
                    | MirTypeKind::Slice(_)
                    | MirTypeKind::Tuple(_)
                    | MirTypeKind::Adt(_)
                    // Fat function value `{ code, env }`
                    | MirTypeKind::FnPtr(_)
            )
        })
        .unwrap_or(false)
}

/// Build an aggregate value (tuple or product/sum ADT) directly in
/// the destination alloca, GEPing per field and using memcpy for
/// aggregate-typed fields (whose SSA load would otherwise decompose
/// the sum's `{ tag, payload }` and drop bytes of a smaller variant).
pub(super) fn build_aggregate_in_place<'ctx>(
    place: &edda_mir::Place,
    outer_ty: &MirType,
    elems: &[Operand],
    cx: &LowerCtx<'ctx, '_>,
) -> Result<(), CompileError> {
    let dest = lower_place_pointer(place, cx)?;
    let outer_struct_ty = inkwell_basic_type(cx.context, outer_ty, cx.program, cx.arch)
        .expect("aggregate destination type lowers")
        .into_struct_type();
    for (idx, elem) in elems.iter().enumerate() {
        let field_ptr = cx
            .builder
            .build_struct_gep(outer_struct_ty, dest, idx as u32, "agg.field.ptr")
            .expect("build_struct_gep into aggregate field for build_aggregate_in_place");
        // Fetch the field's MIR type for sizing/lowering.
        let field_mir_ty = match &outer_ty.kind {
            MirTypeKind::Tuple(elems_ty) => elems_ty.get(idx).cloned(),
            MirTypeKind::Adt(adt_id) => cx
                .program
                .adts
                .get(*adt_id)
                .and_then(|adt| adt.variants.first())
                .and_then(|v| v.fields.get(idx).map(|f| f.ty.clone())),
            _ => None,
        };
        let Some(field_mir_ty) = field_mir_ty else {
            return Err(CompileError::UnsupportedMirShape {
                shape: "build-aggregate-in-place-missing-field-type",
                detail: format!(
                    "body {:?} build_aggregate_in_place could not resolve field type at index {idx} of outer type {:?}",
                    cx.body_name, outer_ty.kind
                ),
            });
        };
        let field_llvm_ty =
            inkwell_basic_type(cx.context, &field_mir_ty, cx.program, cx.arch).ok_or_else(
                || CompileError::UnsupportedMirShape {
                    shape: "build-aggregate-in-place-non-lowerable-field",
                    detail: format!(
                        "body {:?} aggregate field #{idx} of type {:?} does not lower to an LLVM basic type",
                        cx.body_name, field_mir_ty.kind
                    ),
                },
            )?;
        if let Some(src_ptr) = aggregate_source_ptr(elem, cx) {
            let size_val = field_llvm_ty
                .size_of()
                .expect("aggregate field type has a sizeof");
            cx.builder
                .build_memcpy(
                    field_ptr,
                    AGGREGATE_COPY_ALIGN,
                    src_ptr,
                    AGGREGATE_COPY_ALIGN,
                    size_val,
                )
                .expect("build_memcpy of aggregate field into destination GEP");
        } else {
            let value = crate::lower::operand::lower_operand(elem, cx)?.ok_or_else(|| {
                CompileError::UnsupportedMirShape {
                    shape: "build-aggregate-in-place-unit-field",
                    detail: format!(
                        "body {:?} aggregate field #{idx} is Unit; aggregates never carry unit fields",
                        cx.body_name
                    ),
                }
            })?;
            cx.builder
                .build_store(field_ptr, value)
                .expect("build_store of non-aggregate field into destination GEP");
        }
    }
    Ok(())
}
