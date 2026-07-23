//! Tuple and record makers (`MakeTuple`, `MakeRecord`) plus the
//! shared `build_aggregate` helper.
//!
//! Both makers produce an SSA struct value via chained `insertvalue`
//! over `undef` — there is no alloca / store / load round-trip
//! (which would fight LLVM's `mem2reg`). `build_aggregate` is shared
//! with [`super::sum::build_variant_value`] for the per-variant
//! payload struct.

use edda_mir::{AdtId, AdtKind, Operand, Rvalue};
use inkwell::values::BasicValueEnum;

use crate::error::CompileError;

use super::super::operand::lower_operand;
use super::super::ty::inkwell_basic_type;
use super::super::LowerCtx;

/// Lower `RvalueKind::MakeTuple` by chaining `insertvalue` from
/// `undef` of the tuple's struct type. Producing an SSA value
/// directly avoids the alloca/store/load triple that would otherwise
/// fight LLVM's `mem2reg`.
pub(super) fn lower_make_tuple<'ctx>(
    rvalue: &Rvalue,
    elems: &[Operand],
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    let tuple_ty = inkwell_basic_type(cx.context, &rvalue.ty, cx.program, cx.arch).ok_or_else(
        || CompileError::UnsupportedMirShape {
            shape: "make-tuple-non-lowerable-type",
            detail: format!(
                "body {:?} builds a tuple of type {:?} whose elements are not all lowerable",
                cx.body_name, rvalue.ty.kind
            ),
        },
    )?;
    build_aggregate(tuple_ty.into_struct_type(), elems, cx, "tup", "tuple")
}

/// Lower `RvalueKind::MakeRecord` for a product ADT.
pub(super) fn lower_make_record<'ctx>(
    rvalue: &Rvalue,
    adt: AdtId,
    fields: &[Operand],
    cx: &LowerCtx<'ctx, '_>,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    // Sanity-check the ADT shape — the typechecker should have already
    // ensured this, but a wrong-shape arrival here would produce a
    // miscompiled struct, so guard explicitly.
    let adt_def = cx
        .program
        .adts
        .get(adt)
        .expect("MakeRecord references an AdtId from the same program");
    if !matches!(adt_def.kind, AdtKind::Product) {
        return Err(CompileError::UnsupportedMirShape {
            shape: "make-record-on-sum",
            detail: format!(
                "body {:?} uses MakeRecord on sum ADT {:?}; MakeVariant is the sum path",
                cx.body_name, adt_def.name
            ),
        });
    }
    let record_ty = inkwell_basic_type(cx.context, &rvalue.ty, cx.program, cx.arch).ok_or_else(
        || CompileError::UnsupportedMirShape {
            shape: "make-record-non-lowerable-type",
            detail: format!(
                "body {:?} builds a record of type {:?} whose field types are not all lowerable",
                cx.body_name, rvalue.ty.kind
            ),
        },
    )?;
    build_aggregate(record_ty.into_struct_type(), fields, cx, "rec", "record")
}

/// Build an aggregate value by chaining `insertvalue` from `undef`.
/// Shared between [`lower_make_tuple`], [`lower_make_record`], and
/// [`super::sum::build_variant_value`].
pub(super) fn build_aggregate<'ctx>(
    struct_ty: inkwell::types::StructType<'ctx>,
    elems: &[Operand],
    cx: &LowerCtx<'ctx, '_>,
    name: &str,
    kind_label: &'static str,
) -> Result<BasicValueEnum<'ctx>, CompileError> {
    let mut acc: BasicValueEnum<'ctx> = struct_ty.get_undef().into();
    for (i, elem) in elems.iter().enumerate() {
        let val = lower_operand(elem, cx)?.ok_or_else(|| CompileError::UnsupportedMirShape {
            shape: "aggregate-unit-element",
            detail: format!(
                "body {:?} {kind_label} element #{i} is Unit; {kind_label}s never carry unit fields",
                cx.body_name
            ),
        })?;
        let inserted = cx
            .builder
            .build_insert_value(acc.into_struct_value(), val, i as u32, name)
            .expect("build_insert_value with a valid in-range index must succeed");
        acc = inserted.into_struct_value().into();
    }
    Ok(acc)
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

    /// Build `fn make(a: i32, b: i64) -> (i32, i64) { (a, b) }`.
    /// MakeTuple lowers as chained `insertvalue` over `undef`.
    #[test]
    fn make_tuple_emits_insertvalue_chain() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();
        let name = interner.intern("make_pair");

        let pair_ty = MirType::new(edda_mir::MirTypeKind::Tuple(vec![
            MirType::prim(MirPrim::I32),
            MirType::prim(MirPrim::I64),
        ]));
        let mut bb = BodyBuilder::new(name, Span::DUMMY, pair_ty.clone());
        let ret_local = bb.return_slot(pair_ty.clone(), Span::DUMMY);
        let a = bb.param(ParamMode::Let, MirType::prim(MirPrim::I32), Span::DUMMY);
        let b = bb.param(ParamMode::Let, MirType::prim(MirPrim::I64), Span::DUMMY);
        let mut block = bb.block();
        let block_id = block.id();
        block.assign(
            Span::DUMMY,
            Place::local(ret_local),
            Rvalue {
                span: Span::DUMMY,
                kind: RvalueKind::MakeTuple {
                    elems: vec![
                        Operand::Copy(Place::local(a)),
                        Operand::Copy(Place::local(b)),
                    ],
                },
                ty: pair_ty.clone(),
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
            .expect("MakeTuple must lower");
        let ir = module.print_to_string().to_string();
        // Two fields ⇒ two insertvalues over the undef seed.
        assert_eq!(
            ir.matches("insertvalue").count(),
            2,
            "expected two insertvalue instructions, got: {ir}"
        );
        // Function returns the anonymous struct.
        assert!(
            ir.contains("define { i32, i64 } @make_pair"),
            "expected struct return type in signature: {ir}"
        );
    }

    /// Helper: build a product ADT `Point { x: i32, y: i32 }` in a
    /// fresh program. Returns `(program, adt_id, point_ty)`.
    fn build_point_program(
        interner: &Interner,
    ) -> (edda_mir::MirProgram, edda_mir::AdtId, MirType) {
        use edda_mir::{AdtDef, AdtKind, FieldDef, LayoutInfo, VariantDef};
        let mut pb = edda_mir::ProgramBuilder::new();
        let point_name = interner.intern("Point");
        let x_name = interner.intern("x");
        let y_name = interner.intern("y");
        let variant = VariantDef {
            name: point_name,
            span: Span::DUMMY,
            fields: vec![
                FieldDef {
                    name: x_name,
                    span: Span::DUMMY,
                    ty: MirType::prim(MirPrim::I32),
                },
                FieldDef {
                    name: y_name,
                    span: Span::DUMMY,
                    ty: MirType::prim(MirPrim::I32),
                },
            ],
            discriminant: None,
        };
        let adt_id = pb.push_adt(AdtDef {
            name: point_name,
            span: Span::DUMMY,
            kind: AdtKind::Product,
            variants: vec![variant],
            layout: LayoutInfo::natural(),
            tag_width: None,
        });
        let ty = MirType::new(edda_mir::MirTypeKind::Adt(adt_id));
        (pb.finish(), adt_id, ty)
    }

    /// `fn mk(x: i32, y: i32) -> Point { Point { x, y } }` via
    /// MakeRecord — checks chained insertvalue + struct return.
    #[test]
    fn make_record_emits_insertvalue_chain() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();
        let (program, adt_id, point_ty) = build_point_program(&interner);

        let name = interner.intern("mk");
        let mut bb = BodyBuilder::new(name, Span::DUMMY, point_ty.clone());
        let ret_local = bb.return_slot(point_ty.clone(), Span::DUMMY);
        let x = bb.param(ParamMode::Let, MirType::prim(MirPrim::I32), Span::DUMMY);
        let y = bb.param(ParamMode::Let, MirType::prim(MirPrim::I32), Span::DUMMY);
        let mut block = bb.block();
        let block_id = block.id();
        block.assign(
            Span::DUMMY,
            Place::local(ret_local),
            Rvalue {
                span: Span::DUMMY,
                kind: RvalueKind::MakeRecord {
                    adt: adt_id,
                    fields: vec![
                        Operand::Copy(Place::local(x)),
                        Operand::Copy(Place::local(y)),
                    ],
                },
                ty: point_ty,
            },
        );
        block.return_(Span::DUMMY, Operand::Copy(Place::local(ret_local)));
        bb.set_entry(block_id);
        let body = bb.finish();

        let function = emitter
            .declare_function(&module, &body, &program, &interner, target.triple().arch())
            .unwrap();
        emitter
            .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
            .expect("MakeRecord must lower");
        let ir = module.print_to_string().to_string();
        assert_eq!(
            ir.matches("insertvalue").count(),
            2,
            "expected two insertvalue instructions: {ir}"
        );
        assert!(
            ir.contains("define { i32, i32 } @mk(i32"),
            "expected struct return + two-i32 params: {ir}"
        );
    }

    /// ExtractField on a record subject reads via extractvalue — same
    /// code path as tuples.
    #[test]
    fn extract_field_on_record_emits_extractvalue() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();
        let (program, _adt_id, point_ty) = build_point_program(&interner);

        let name = interner.intern("get_x");
        let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
        let ret_local = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
        let p = bb.param(ParamMode::Let, point_ty, Span::DUMMY);
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
        let function = emitter
            .declare_function(&module, &body, &program, &interner, target.triple().arch())
            .unwrap();
        emitter
            .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
            .expect("ExtractField on record must lower");
        let ir = module.print_to_string().to_string();
        assert!(ir.contains("extractvalue"), "missing extractvalue: {ir}");
    }
}
