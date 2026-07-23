//! MIR [`Statement`] -> inkwell instruction(s).
//!
//! `Assign` lowers via [`super::rvalue::lower_rvalue`] +
//! a `store` through [`super::place::lower_place_pointer`]; the
//! no-op family (`Nop` / `StorageLive` / `StorageDead` / `SetInit`
//! / `Drop`) is ignored at the IR level (drop-glue insertion and
//! `@llvm.lifetime.*` markers are not yet implemented).

use edda_mir::{Idx, MirTypeKind, RvalueKind, Statement, StatementKind};
use inkwell::types::BasicType;

use crate::error::CompileError;

use super::LowerCtx;
use super::place::lower_place_pointer;
use super::rvalue::array::array_element_ptr;
use super::rvalue::{build_variant_in_alloca, lower_rvalue};
use super::ty::inkwell_basic_type;

mod aggregate;

use aggregate::{
    aggregate_extract_subject_ptr, aggregate_use_source_ptr, build_aggregate_in_place,
    operand_is_aggregate, operand_yields_no_value, rvalue_ty_is_aggregate, AGGREGATE_COPY_ALIGN,
};

/// Lower a single statement. `Assign` lowers via [`lower_rvalue`]
/// + a write through [`lower_place_pointer`]; the no-op family is
/// still ignored at the IR level.
pub(crate) fn lower_statement<'ctx>(
    stmt: &Statement,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<(), CompileError> {
    match &stmt.kind {
        StatementKind::Nop
        | StatementKind::StorageLive(_)
        | StatementKind::StorageDead(_)
        | StatementKind::SetInit(_)
        | StatementKind::Drop(_) => Ok(()),
        StatementKind::Assign { place, rvalue } => {
            // Locals with no alloca (Unit / Never) have no LLVM
            // representation — the assignment is a no-op at the IR level.
            if cx.locals[place.local.index()].is_none() {
                return Ok(());
            }
            // `Use` of an operand that yields no LLVM value (Unit or a
            // no-alloca source local) is a no-op store. The MIR emits
            // `Use(Operand::Unit)` as the join-point value of diverging
            // expressions (e.g. a `loop { ... }` whose body always returns
            // via `if i >= bytes.len() { return i }`). Skip the assign
            // rather than failing — the dest slot is never read in those
            // paths.
            if let RvalueKind::Use(operand) = &rvalue.kind
                && operand_yields_no_value(operand, cx)
            {
                return Ok(());
            }
            // Byte-preserving paths: when the rvalue assembles a sum
            // variant whose payload may have cross-variant byte
            // layout (e.g. `Ok(double)` written inside an
            // `{ i8, { i64 } }`-shaped max-variant payload), or when
            // copying an existing aggregate, the SSA round-trip
            // `load <agg> + store <agg>` drops padding bytes. Memcpy
            // preserves the full byte range.
            if let RvalueKind::MakeVariant { adt, variant, fields } = &rvalue.kind {
                let dest = lower_place_pointer(place, cx)?;
                let (src_ptr, outer_ty) =
                    build_variant_in_alloca(&rvalue.ty, *adt, *variant, fields, cx)?;
                let size_val = outer_ty
                    .size_of()
                    .expect("aggregate variant type has a sizeof");
                cx.builder
                    .build_memcpy(
                        dest,
                        AGGREGATE_COPY_ALIGN,
                        src_ptr,
                        AGGREGATE_COPY_ALIGN,
                        size_val,
                    )
                    .expect("build_memcpy of assembled variant into destination");
                return Ok(());
            }
            // ArrayIndex on a slice whose element type is aggregate
            // (sum / product / slice / String): the default
            // `lower_rvalue` path does a typed `load <elem_ty>` which
            // decomposes into per-field loads and silently drops
            // cross-variant padding bytes for any sum-typed sub-field
            // (e.g. a `FunctionDef` inside an `Entry` slot). Route
            // through `array_element_ptr` to recover the GEP pointer
            // and memcpy element bytes verbatim into the destination.
            if let RvalueKind::ArrayIndex { array, idx } = &rvalue.kind
                && rvalue_ty_is_aggregate(&rvalue.ty)
            {
                let (src_ptr, leaf_ty) = array_element_ptr(rvalue, array, idx, cx)?;
                let dest = lower_place_pointer(place, cx)?;
                let size_val = leaf_ty
                    .size_of()
                    .expect("aggregate ArrayIndex element type has a sizeof");
                cx.builder
                    .build_memcpy(
                        dest,
                        AGGREGATE_COPY_ALIGN,
                        src_ptr,
                        AGGREGATE_COPY_ALIGN,
                        size_val,
                    )
                    .expect("build_memcpy of aggregate ArrayIndex result into destination");
                return Ok(());
            }
            if let RvalueKind::Use(operand) = &rvalue.kind
                && rvalue_ty_is_aggregate(&rvalue.ty)
                && let Some(src_ptr) = aggregate_use_source_ptr(operand, cx)?
            {
                let dest = lower_place_pointer(place, cx)?;
                let leaf_ty = inkwell_basic_type(cx.context, &rvalue.ty, cx.program, cx.arch)
                    .expect("aggregate type lowers");
                let size_val = leaf_ty
                    .size_of()
                    .expect("aggregate Use type has a sizeof");
                cx.builder
                    .build_memcpy(
                        dest,
                        AGGREGATE_COPY_ALIGN,
                        src_ptr,
                        AGGREGATE_COPY_ALIGN,
                        size_val,
                    )
                    .expect("build_memcpy of aggregate Use into destination");
                return Ok(());
            }
            // Aggregate-typed `ExtractField` from a tuple / product
            // subject (`variant: None`): the SSA round-trip
            // (`load <outer>; extractvalue, idx`) decomposes the
            // outer struct field-by-field and drops bytes inside any
            // sum-typed field whose actual variant is smaller than
            // the max-variant payload slot. GEP into the subject's
            // memory and memcpy the field bytes verbatim.
            // Byte-preserving paths for aggregates that contain
            // sum-typed fields: build the aggregate directly in the
            // destination, using per-field memcpy for aggregate
            // operands (whose SSA load would otherwise decompose the
            // sum's `{ tag, payload }` struct and drop bytes of a
            // smaller variant). Covers `MakeTuple` and `MakeRecord`
            // — `MakeVariant` is handled above.
            if let RvalueKind::MakeTuple { elems } = &rvalue.kind
                && rvalue_ty_is_aggregate(&rvalue.ty)
                && elems.iter().any(|op| operand_is_aggregate(op, cx))
            {
                build_aggregate_in_place(place, &rvalue.ty, elems, cx)?;
                return Ok(());
            }
            if let RvalueKind::MakeRecord { fields, .. } = &rvalue.kind
                && rvalue_ty_is_aggregate(&rvalue.ty)
                && fields.iter().any(|op| operand_is_aggregate(op, cx))
            {
                build_aggregate_in_place(place, &rvalue.ty, fields, cx)?;
                return Ok(());
            }
            if let RvalueKind::ExtractField {
                subject,
                variant: None,
                field,
            } = &rvalue.kind
                && rvalue_ty_is_aggregate(&rvalue.ty)
                && let Some((subject_ptr, subject_ty)) = aggregate_extract_subject_ptr(subject, cx)
            {
                let dest = lower_place_pointer(place, cx)?;
                let outer_struct_ty =
                    inkwell_basic_type(cx.context, &subject_ty, cx.program, cx.arch)
                        .expect("aggregate subject lowers")
                        .into_struct_type();
                let field_ptr = cx
                    .builder
                    .build_struct_gep(
                        outer_struct_ty,
                        subject_ptr,
                        field.index() as u32,
                        "ex.field.ptr",
                    )
                    .expect("build_struct_gep into aggregate field for memcpy extract");
                let leaf_ty = inkwell_basic_type(cx.context, &rvalue.ty, cx.program, cx.arch)
                    .expect("aggregate field type lowers");
                let size_val = leaf_ty
                    .size_of()
                    .expect("aggregate ExtractField type has a sizeof");
                cx.builder
                    .build_memcpy(
                        dest,
                        AGGREGATE_COPY_ALIGN,
                        field_ptr,
                        AGGREGATE_COPY_ALIGN,
                        size_val,
                    )
                    .expect("build_memcpy of aggregate ExtractField into destination");
                return Ok(());
            }
            // Aggregate-typed `ExtractField` from a sum variant
            // payload (`variant: Some(_)`): the SSA path (GEP payload
            // -> typed load of the variant struct -> extractvalue)
            // still decomposes the loaded variant struct and drops
            // bytes when the *field* is itself a sum with smaller
            // variants. GEP through the payload slot and the variant
            // struct, then memcpy.
            if let RvalueKind::ExtractField {
                subject,
                variant: Some(variant_idx),
                field,
            } = &rvalue.kind
                && rvalue_ty_is_aggregate(&rvalue.ty)
                && let Some((subject_ptr, subject_ty)) = aggregate_extract_subject_ptr(subject, cx)
            {
                if let MirTypeKind::Adt(adt_id) = &subject_ty.kind {
                    let adt = cx.program.adts.get(*adt_id);
                    if let Some(adt) = adt
                        && matches!(adt.kind, edda_mir::AdtKind::Sum)
                    {
                        let dest = lower_place_pointer(place, cx)?;
                        let sum_struct_ty =
                            inkwell_basic_type(cx.context, &subject_ty, cx.program, cx.arch)
                                .expect("sum subject lowers")
                                .into_struct_type();
                        let payload_ptr = cx
                            .builder
                            .build_struct_gep(sum_struct_ty, subject_ptr, 1, "ex.var.pay.ptr")
                            .expect("build_struct_gep into sum payload slot for variant extract");
                        let variant_payload_ty = super::ty::sum_variant_payload_type(
                            cx.context,
                            adt,
                            variant_idx.index(),
                            cx.program,
                            cx.arch,
                        )
                        .expect("variant payload type lowers");
                        let field_ptr = cx
                            .builder
                            .build_struct_gep(
                                variant_payload_ty,
                                payload_ptr,
                                field.index() as u32,
                                "ex.var.field.ptr",
                            )
                            .expect(
                                "build_struct_gep into variant payload field for memcpy extract",
                            );
                        let leaf_ty =
                            inkwell_basic_type(cx.context, &rvalue.ty, cx.program, cx.arch)
                                .expect("aggregate variant-field type lowers");
                        let size_val = leaf_ty
                            .size_of()
                            .expect("aggregate variant field type has a sizeof");
                        cx.builder
                            .build_memcpy(
                                dest,
                                AGGREGATE_COPY_ALIGN,
                                field_ptr,
                                AGGREGATE_COPY_ALIGN,
                                size_val,
                            )
                            .expect("build_memcpy of aggregate variant field into destination");
                        return Ok(());
                    }
                }
            }
            let dest = lower_place_pointer(place, cx)?;
            let value = lower_rvalue(rvalue, cx)?;
            cx.builder
                .build_store(dest, value)
                .expect("build_store at Assign site");
            Ok(())
        }
    }
}
