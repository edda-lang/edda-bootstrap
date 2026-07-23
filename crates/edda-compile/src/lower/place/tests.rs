use crate::{CompileError, Emitter};
use edda_intern::Interner;
use edda_mir::{BodyBuilder, FieldIdx, Idx, LocalId, MirPrim, MirType, Operand, Projection, ProgramBuilder, VariantIdx};
use edda_span::Span;

use crate::lower::test_fixtures::linux_x86_64;

#[test]
fn field_projection_on_primitive_root_is_rejected() {
    let emitter = Emitter::new();
    let target = linux_x86_64();
    let module = emitter.tagged_module("m", &target).unwrap();
    let interner = Interner::new();
    let name = interner.intern("proj_test");
    let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
    let _ret_local = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
    let block = bb.block();
    let block_id = block.id();
    // Synthesize a Place with a Field projection on a *primitive*
    // root — Field is admitted only on tuple (and, in 9b, record)
    // roots.
    let p = edda_mir::Place {
        local: LocalId::RETURN_SLOT,
        projection: vec![Projection::Field(FieldIdx::new(0))],
    };
    block.return_(Span::DUMMY, Operand::Copy(p));
    bb.set_entry(block_id);
    let body = bb.finish();
    let program = ProgramBuilder::new().finish();
    let function = emitter
        .declare_function(&module, &body, &program, &interner, target.triple().arch())
        .unwrap();
    let err = emitter
        .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
        .expect_err("Field projection on a primitive root must be rejected");
    assert!(
        matches!(err, CompileError::UnsupportedMirShape { shape, .. } if shape == "field-projection-non-aggregate"),
        "wrong shape: {err:?}",
    );
}

#[test]
fn index_projection_on_non_slice_is_rejected() {
    let emitter = Emitter::new();
    let target = linux_x86_64();
    let module = emitter.tagged_module("m", &target).unwrap();
    let interner = Interner::new();
    let name = interner.intern("idx_test");
    let pair_ty = MirType::new(edda_mir::MirTypeKind::Tuple(vec![
        MirType::prim(MirPrim::I32),
        MirType::prim(MirPrim::I32),
    ]));
    let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
    let _ = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
    let tup = bb.param(edda_mir::ParamMode::Let, pair_ty, Span::DUMMY);
    let block = bb.block();
    let block_id = block.id();
    let p = edda_mir::Place {
        local: tup,
        projection: vec![Projection::Index(LocalId::RETURN_SLOT)],
    };
    block.return_(Span::DUMMY, Operand::Copy(p));
    bb.set_entry(block_id);
    let body = bb.finish();
    let program = ProgramBuilder::new().finish();
    let function = emitter
        .declare_function(&module, &body, &program, &interner, target.triple().arch())
        .unwrap();
    let err = emitter
        .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
        .expect_err("Index projection on a non-slice root must be rejected");
    assert!(
        matches!(err, CompileError::UnsupportedMirShape { shape, .. } if shape == "index-projection-non-slice"),
        "wrong shape: {err:?}",
    );
}

/// `[VariantDowncast(v), Field(i)]` walks the sum's
/// payload slot then the variant's payload struct. The IR must
/// contain two GEPs — one into the outer sum (index 1, the
/// payload slot) and one into the variant's payload (index `i`).
#[test]
fn variant_downcast_then_field_emits_payload_and_variant_gep() {
    use edda_mir::{
        AdtDef, AdtKind, FieldDef, LayoutInfo, MirTypeKind, ParamMode, Rvalue, RvalueKind,
        VariantDef,
    };

    let emitter = Emitter::new();
    let target = linux_x86_64();
    let module = emitter.tagged_module("m", &target).unwrap();
    let interner = Interner::new();

    // Sum: Option<i32> with tag_width U8.
    let mut pb = ProgramBuilder::new();
    let opt_name = interner.intern("Option");
    let none_name = interner.intern("None");
    let some_name = interner.intern("Some");
    let payload_name = interner.intern("0");
    let adt_id = pb.push_adt(AdtDef {
        name: opt_name,
        span: Span::DUMMY,
        kind: AdtKind::Sum,
        variants: vec![
            VariantDef {
                name: none_name,
                span: Span::DUMMY,
                fields: vec![],
                discriminant: Some(0),
            },
            VariantDef {
                name: some_name,
                span: Span::DUMMY,
                fields: vec![FieldDef {
                    name: payload_name,
                    span: Span::DUMMY,
                    ty: MirType::prim(MirPrim::I32),
                }],
                discriminant: Some(1),
            },
        ],
        layout: LayoutInfo::natural(),
        tag_width: Some(MirPrim::U8),
    });
    let option_ty = MirType::new(MirTypeKind::Adt(adt_id));

    let name = interner.intern("downcast_some");
    let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
    let ret_local = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
    let o = bb.param(ParamMode::Let, option_ty, Span::DUMMY);

    // ret = o.[VariantDowncast(Some)].[Field(0)]
    let mut block = bb.block();
    let block_id = block.id();
    block.assign(
        Span::DUMMY,
        edda_mir::Place::local(ret_local),
        Rvalue {
            span: Span::DUMMY,
            kind: RvalueKind::Use(Operand::Copy(edda_mir::Place {
                local: o,
                projection: vec![
                    Projection::VariantDowncast(VariantIdx::new(1)),
                    Projection::Field(FieldIdx::new(0)),
                ],
            })),
            ty: MirType::prim(MirPrim::I32),
        },
    );
    block.return_(Span::DUMMY, Operand::Copy(edda_mir::Place::local(ret_local)));
    bb.set_entry(block_id);
    let body = bb.finish();
    let program = pb.finish();

    let function = emitter
        .declare_function(&module, &body, &program, &interner, target.triple().arch())
        .unwrap();
    emitter
        .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
        .expect("VariantDowncast + Field must lower");
    let ir = module.print_to_string().to_string();
    // LLVM 18.1.6 emits `inbounds nuw`; earlier 18.x emits plain
    // `inbounds`. Tolerate both spellings.
    // First GEP: into the outer sum's payload slot.
    assert!(
        ir.contains("getelementptr inbounds { i8, { i32 } }")
            || ir.contains("getelementptr inbounds nuw { i8, { i32 } }"),
        "expected outer-sum GEP: {ir}"
    );
    // Second GEP: into the variant payload's i32 field.
    assert!(
        ir.contains("getelementptr inbounds { i32 }")
            || ir.contains("getelementptr inbounds nuw { i32 }"),
        "expected variant-payload GEP: {ir}"
    );
}

/// VariantDowncast is admitted on sum ADTs but still rejects
/// it on non-ADT roots (tuples, primitives) with a precise label.
#[test]
fn variant_downcast_on_non_adt_root_is_rejected() {
    let emitter = Emitter::new();
    let target = linux_x86_64();
    let module = emitter.tagged_module("m", &target).unwrap();
    let interner = Interner::new();
    let name = interner.intern("vd_test");
    let pair_ty = MirType::new(edda_mir::MirTypeKind::Tuple(vec![
        MirType::prim(MirPrim::I32),
        MirType::prim(MirPrim::I32),
    ]));
    let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
    let _ = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
    let tup = bb.param(edda_mir::ParamMode::Let, pair_ty, Span::DUMMY);
    let block = bb.block();
    let block_id = block.id();
    let p = edda_mir::Place {
        local: tup,
        projection: vec![Projection::VariantDowncast(VariantIdx::new(0))],
    };
    block.return_(Span::DUMMY, Operand::Copy(p));
    bb.set_entry(block_id);
    let body = bb.finish();
    let program = ProgramBuilder::new().finish();
    let function = emitter
        .declare_function(&module, &body, &program, &interner, target.triple().arch())
        .unwrap();
    let err = emitter
        .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
        .expect_err("VariantDowncast on a tuple must be rejected");
    assert!(
        matches!(err, CompileError::UnsupportedMirShape { shape, .. } if shape == "variant-downcast-non-adt"),
        "wrong shape: {err:?}",
    );
}

/// U6: `[Deref(record), Field(0)]`
/// reads an aggregate *through a pointer*. The IR must first `load
/// ptr` (the deref of the `HeapPtr` param), then GEP into the
/// pointed-to record and `load` the field — proving the whole
/// aggregate is reachable behind the pointer, not just a scalar.
#[test]
fn deref_pointer_to_record_loads_aggregate_field() {
    use edda_mir::{
        AdtDef, AdtKind, FieldDef, LayoutInfo, MirTypeKind, ParamMode, Rvalue, RvalueKind,
        VariantDef,
    };

    let emitter = Emitter::new();
    let target = linux_x86_64();
    let module = emitter.tagged_module("m", &target).unwrap();
    let interner = Interner::new();

    // Record: Point { x: i32, y: i32 } — a product ADT.
    let mut pb = ProgramBuilder::new();
    let point_name = interner.intern("Point");
    let ctor_name = interner.intern("Point");
    let x_name = interner.intern("x");
    let y_name = interner.intern("y");
    let adt_id = pb.push_adt(AdtDef {
        name: point_name,
        span: Span::DUMMY,
        kind: AdtKind::Product,
        variants: vec![VariantDef {
            name: ctor_name,
            span: Span::DUMMY,
            fields: vec![
                FieldDef { name: x_name, span: Span::DUMMY, ty: MirType::prim(MirPrim::I32) },
                FieldDef { name: y_name, span: Span::DUMMY, ty: MirType::prim(MirPrim::I32) },
            ],
            discriminant: None,
        }],
        layout: LayoutInfo::natural(),
        tag_width: None,
    });
    let point_ty = MirType::new(MirTypeKind::Adt(adt_id));

    let name = interner.intern("read_x_through_ptr");
    let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
    let ret_local = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
    // Param is a raw heap pointer to a Point.
    let p = bb.param(ParamMode::Let, MirType::prim(MirPrim::HeapPtr), Span::DUMMY);

    // ret = (*p as Point).x  ==  p.[Deref(Point)].[Field(0)]
    let mut block = bb.block();
    let block_id = block.id();
    block.assign(
        Span::DUMMY,
        edda_mir::Place::local(ret_local),
        Rvalue {
            span: Span::DUMMY,
            kind: RvalueKind::Use(Operand::Copy(edda_mir::Place {
                local: p,
                projection: vec![
                    Projection::Deref(point_ty.clone()),
                    Projection::Field(FieldIdx::new(0)),
                ],
            })),
            ty: MirType::prim(MirPrim::I32),
        },
    );
    block.return_(Span::DUMMY, Operand::Copy(edda_mir::Place::local(ret_local)));
    bb.set_entry(block_id);
    let body = bb.finish();
    let program = pb.finish();

    let function = emitter
        .declare_function(&module, &body, &program, &interner, target.triple().arch())
        .unwrap();
    emitter
        .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
        .expect("Deref + Field on a HeapPtr param must lower");
    let ir = module.print_to_string().to_string();
    // The deref step loads the pointer value out of the param slot.
    assert!(ir.contains("load ptr"), "expected deref `load ptr`: {ir}");
    // The field walk GEPs into the pointed-to record `{ i32, i32 }`.
    assert!(
        ir.contains("getelementptr inbounds { i32, i32 }")
            || ir.contains("getelementptr inbounds nuw { i32, i32 }"),
        "expected record-field GEP through the pointer: {ir}"
    );
}

/// U6: a `Deref` whose leaf is not a `HeapPtr` is rejected with a
/// precise shape label — Deref is only valid behind a pointer.
#[test]
fn deref_on_non_pointer_leaf_is_rejected() {
    let emitter = Emitter::new();
    let target = linux_x86_64();
    let module = emitter.tagged_module("m", &target).unwrap();
    let interner = Interner::new();
    let name = interner.intern("bad_deref");
    let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
    let _ = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
    // An i32 param is not a pointer.
    let n = bb.param(edda_mir::ParamMode::Let, MirType::prim(MirPrim::I32), Span::DUMMY);
    let block = bb.block();
    let block_id = block.id();
    let p = edda_mir::Place {
        local: n,
        projection: vec![Projection::Deref(MirType::prim(MirPrim::I32))],
    };
    block.return_(Span::DUMMY, Operand::Copy(p));
    bb.set_entry(block_id);
    let body = bb.finish();
    let program = ProgramBuilder::new().finish();
    let function = emitter
        .declare_function(&module, &body, &program, &interner, target.triple().arch())
        .unwrap();
    let err = emitter
        .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
        .expect_err("Deref on a non-pointer leaf must be rejected");
    assert!(
        matches!(err, CompileError::UnsupportedMirShape { shape, .. } if shape == "deref-projection-non-pointer"),
        "wrong shape: {err:?}",
    );
}
