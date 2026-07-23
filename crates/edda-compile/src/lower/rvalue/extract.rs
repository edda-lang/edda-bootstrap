//! Field and tag extraction (`ExtractField`, `ExtractTag`).
//!
//! `ExtractField` has two paths: a no-variant tuple/record path that
//! reads a field via `extractvalue`, and a variant-bearing sum path
//! that round-trips the sum value through an alloca to reinterpret
//! the max-variant-payload slot as the chosen variant's payload
//! struct. `ExtractTag` is a single `extractvalue` at index 0 of the
//! `{ tag, payload }` sum struct.

use edda_mir::{AdtKind, FieldIdx, Idx, MirType, MirTypeKind, Operand, VariantIdx};
use inkwell::values::{BasicValue, BasicValueEnum};

use crate::error::CompileError;

use super::super::operand::lower_operand;
use super::super::place::{lower_place_pointer, place_leaf_ty};
use super::super::ty::{inkwell_basic_type, sum_slot_access_align, sum_variant_payload_type};
use super::super::LowerCtx;

/// Lower `RvalueKind::ExtractField`. With `variant: None`, the
/// subject is a tuple or record and we read the field directly via
/// `extractvalue`. With `variant: Some(v)`, the subject is a
/// sum-typed value (`{ tag, max-variant-payload }`): we alloca a
/// scratch slot, store the subject there, GEP into the payload slot,
/// and re-load it typed at the *variant's own* payload struct — then
/// `extractvalue` the field. LLVM accepts the typed load reading
/// only the first `sizeof(variant_payload)` bytes of the
/// max-variant-payload slot; trailing bytes (only present for
/// variants smaller than the max) are unread.
pub(super) fn lower_extract_field<'ctx>(
    subject: &Operand,
    variant: Option<VariantIdx>,
    field: FieldIdx,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    match variant {
        None => lower_extract_field_aggregate(subject, field, cx),
        Some(v) => lower_extract_field_variant(subject, v, field, cx),
    }
}

/// Tuple / record subject: `extractvalue <struct>, <idx>`.
fn lower_extract_field_aggregate<'ctx>(
    subject: &Operand,
    field: FieldIdx,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    let subject_val = lower_operand(subject, cx)?.ok_or_else(|| {
        CompileError::UnsupportedMirShape {
            shape: "extract-field-unit-subject",
            detail: format!("body {:?} ExtractField subject is Unit", cx.body_name),
        }
    })?;
    let struct_val = subject_val.into_struct_value();
    let extracted = cx
        .builder
        .build_extract_value(struct_val, field.index() as u32, "ext")
        .expect("build_extract_value with an in-range index must succeed; MIR validation guarantees the range");
    Ok(extracted)
}

/// Sum-variant subject: round-trip through an alloca to reinterpret
/// the payload slot as the chosen variant's payload struct, then
/// extract the field by index.
fn lower_extract_field_variant<'ctx>(
    subject: &Operand,
    variant_idx: VariantIdx,
    field: FieldIdx,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    let subject_ty = subject_static_ty(subject, cx)?;
    let adt_id = match &subject_ty.kind {
        MirTypeKind::Adt(id) => *id,
        other => {
            return Err(CompileError::UnsupportedMirShape {
                shape: "extract-field-variant-non-adt",
                detail: format!(
                    "body {:?} ExtractField with variant on subject of non-ADT type {other:?}",
                    cx.body_name
                ),
            });
        }
    };
    let adt = cx
        .program
        .adts
        .get(adt_id)
        .expect("AdtId in subject's type was issued by the same program");
    if adt.kind != AdtKind::Sum {
        return Err(CompileError::UnsupportedMirShape {
            shape: "extract-field-variant-on-product",
            detail: format!(
                "body {:?} ExtractField with variant on product ADT {:?}; \
                 variant: Some(_) requires a sum subject",
                cx.body_name, adt.name
            ),
        });
    }
    if variant_idx.index() >= adt.variants.len() {
        return Err(CompileError::UnsupportedMirShape {
            shape: "extract-field-variant-out-of-range",
            detail: format!(
                "body {:?} ExtractField variant index {} but ADT has {} variants",
                cx.body_name,
                variant_idx.index(),
                adt.variants.len()
            ),
        });
    }
    let variant_payload_ty = sum_variant_payload_type(
        cx.context,
        adt,
        variant_idx.index(),
        cx.program,
        cx.arch,
    )
    .ok_or_else(|| CompileError::UnsupportedMirShape {
        shape: "extract-field-variant-non-lowerable",
        detail: format!(
            "body {:?} ExtractField on variant {} of ADT {:?}: variant payload contains a non-lowerable field type",
            cx.body_name,
            variant_idx.index(),
            adt.name
        ),
    })?;
    let sum_ty = inkwell_basic_type(cx.context, &subject_ty, cx.program, cx.arch)
        .ok_or_else(|| CompileError::UnsupportedMirShape {
            shape: "extract-field-variant-non-lowerable-sum",
            detail: format!(
                "body {:?} ExtractField subject ADT {:?} is not lowerable",
                cx.body_name, adt.name
            ),
        })?
        .into_struct_type();

    // Get a pointer to the subject's memory. When the subject is a
    // `Move`/`Copy` of a `Place` (the common case after `?`
    // propagation), use the place's pointer directly — going through
    // an SSA value would decompose the sum's `{ tag, payload }`
    // struct into per-field scalars and drop the padding bytes that
    // a smaller variant's payload may occupy (e.g. `Ok(double)`
    // writes 8 bytes at offset 8 inside a `{ i8, { i64 } }`-shaped
    // payload — the SSA round-trip loses bytes 9-15 of the double).
    // For non-Place operands (`Const`, computed values) we still
    // need a scratch alloca; in that case we copy via memcpy so all
    // bytes (including padding) survive.
    let subject_ptr = match subject {
        Operand::Copy(p) | Operand::Move(p) => lower_place_pointer(p, cx)?,
        Operand::Const(_) | Operand::Unit => {
            let subject_val = lower_operand(subject, cx)?.ok_or_else(|| {
                CompileError::UnsupportedMirShape {
                    shape: "extract-field-unit-subject",
                    detail: format!("body {:?} ExtractField subject is Unit", cx.body_name),
                }
            })?;
            let alloca = cx.build_entry_alloca(sum_ty, "ex.sum");
            cx.builder
                .build_store(alloca, subject_val)
                .expect("build_store of sum value into scratch alloca");
            alloca
        }
    };
    let payload_ptr = cx
        .builder
        .build_struct_gep(sum_ty, subject_ptr, 1, "ex.pay.ptr")
        .expect("build_struct_gep into payload slot (field 1)");
    let payload_load = cx
        .builder
        .build_load(variant_payload_ty, payload_ptr, "ex.var")
        .expect("build_load of variant payload");
    // The payload slot is only conservatively aligned (the sum alloca's
    // `i64`-chunk blob); reading a payload whose natural alignment is
    // wider (an `i128`/`u128` variant field) must not claim its 16-byte
    // load alignment, or LLVM emits an aligned `movaps` from an
    // 8-aligned slot and faults.
    let payload_align = adt.variants[variant_idx.index()]
        .fields
        .iter()
        .map(|f| sum_slot_access_align(&f.ty, cx.program, cx.arch))
        .max()
        .unwrap_or(1);
    payload_load
        .as_instruction_value()
        .expect("a load produces an instruction value")
        .set_alignment(payload_align)
        .expect("set conservative alignment on variant-payload load");
    let variant_struct = payload_load.into_struct_value();
    let extracted = cx
        .builder
        .build_extract_value(variant_struct, field.index() as u32, "ex.field")
        .expect("build_extract_value with an in-range field index must succeed");
    Ok(extracted)
}

/// Lower `RvalueKind::ExtractTag`: `extractvalue <sum>, 0`. The
/// resulting value carries the sum's tag-width int type by
/// construction.
pub(super) fn lower_extract_tag<'ctx>(
    subject: &Operand,
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    let subject_val = lower_operand(subject, cx)?.ok_or_else(|| {
        CompileError::UnsupportedMirShape {
            shape: "extract-tag-unit-subject",
            detail: format!("body {:?} ExtractTag subject is Unit", cx.body_name),
        }
    })?;
    let struct_val = subject_val.into_struct_value();
    let tag = cx
        .builder
        .build_extract_value(struct_val, 0, "tag")
        .expect("build_extract_value at index 0 of a sum struct must succeed");
    Ok(tag)
}

/// Resolve an operand's static MIR type without emitting IR.
/// Used by `lower_extract_field_variant` to find the subject's sum
/// ADT before round-tripping the value through an alloca.
fn subject_static_ty(
    operand: &Operand,
    cx: &LowerCtx<'_, '_>,
) -> Result<MirType, CompileError> {
    match operand {
        Operand::Copy(p) | Operand::Move(p) => place_leaf_ty(p, cx),
        Operand::Const(id) => Ok(cx.program.consts[*id].ty.clone()),
        Operand::Unit => Err(CompileError::UnsupportedMirShape {
            shape: "subject-static-ty-on-unit",
            detail: format!(
                "body {:?} has an Operand::Unit where a typed subject is required",
                cx.body_name
            ),
        }),
    }
}

#[cfg(test)]
mod tests {
    use crate::Emitter;
    use edda_intern::Interner;
    use edda_mir::{
        BodyBuilder, Idx, MirPrim, MirType, Operand, ParamMode, Place, ProgramBuilder, Rvalue,
        RvalueKind,
    };
    use edda_span::Span;

    use super::super::super::test_fixtures::linux_x86_64;

    /// Build `fn fst(p: (i32, i64)) -> i32 { p.0 (via ExtractField) }`.
    /// ExtractField on a tuple subject lowers as `extractvalue`.
    #[test]
    fn extract_field_on_tuple_emits_extractvalue() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();
        let name = interner.intern("fst");

        let pair_ty = MirType::new(edda_mir::MirTypeKind::Tuple(vec![
            MirType::prim(MirPrim::I32),
            MirType::prim(MirPrim::I64),
        ]));
        let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
        let ret_local = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
        let p = bb.param(ParamMode::Let, pair_ty, Span::DUMMY);
        let mut block = bb.block();
        let block_id = block.id();
        block.assign(
            Span::DUMMY,
            Place::local(ret_local),
            Rvalue {
                span: Span::DUMMY,
                kind: RvalueKind::ExtractField {
                    subject: Operand::Copy(Place::local(p)),
                    variant: None,
                    field: edda_mir::FieldIdx::new(0),
                },
                ty: MirType::prim(MirPrim::I32),
            },
        );
        block.return_(Span::DUMMY, Operand::Copy(Place::local(ret_local)));
        bb.set_entry(block_id);
        let body = bb.finish();
        let program = ProgramBuilder::new().finish();
        let function = emitter
            .declare_function(&module, &body, &program, &interner, target.triple().arch())
            .unwrap();
        emitter
            .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
            .expect("ExtractField on tuple must lower");
        let ir = module.print_to_string().to_string();
        assert!(
            ir.contains("extractvalue"),
            "missing extractvalue: {ir}"
        );
    }

    /// `ExtractField { variant: Some(_) }` is admitted on sum
    /// ADTs but still rejects it on non-ADT subjects (a tuple, here)
    /// with a precise shape label.
    #[test]
    fn extract_field_with_variant_on_non_adt_is_rejected() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();
        let name = interner.intern("ev");

        let pair_ty = MirType::new(edda_mir::MirTypeKind::Tuple(vec![
            MirType::prim(MirPrim::I32),
            MirType::prim(MirPrim::I64),
        ]));
        let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
        let ret_local = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
        let p = bb.param(ParamMode::Let, pair_ty, Span::DUMMY);
        let mut block = bb.block();
        let block_id = block.id();
        block.assign(
            Span::DUMMY,
            Place::local(ret_local),
            Rvalue {
                span: Span::DUMMY,
                kind: RvalueKind::ExtractField {
                    subject: Operand::Copy(Place::local(p)),
                    variant: Some(edda_mir::VariantIdx::new(0)),
                    field: edda_mir::FieldIdx::new(0),
                },
                ty: MirType::prim(MirPrim::I32),
            },
        );
        block.return_(Span::DUMMY, Operand::Copy(Place::local(ret_local)));
        bb.set_entry(block_id);
        let body = bb.finish();
        let program = ProgramBuilder::new().finish();
        let function = emitter
            .declare_function(&module, &body, &program, &interner, target.triple().arch())
            .unwrap();
        let err = emitter
            .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
            .expect_err("variant-bearing ExtractField on a tuple must be rejected");
        assert!(
            matches!(err, crate::CompileError::UnsupportedMirShape { shape, .. } if shape == "extract-field-variant-non-adt"),
            "wrong shape: {err:?}",
        );
    }
}
