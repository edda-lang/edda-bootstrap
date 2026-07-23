//! Sum-typed rvalues (`MakeVariant`) and the shared
//! [`build_variant_value`] helper.
//!
//! `MakeVariant` alloca's the outer `{ tag, max-variant-payload }`
//! sum struct, writes the variant's tag at field 0, writes the
//! variant's own payload struct at field 1, then loads the whole
//! sum back as an SSA value (LLVM does not admit a direct bitcast
//! between two structs whose layouts differ). The trailing payload
//! bytes of variants smaller than the max are left undef.
//!
//! [`build_variant_value`] is shared with the
//! [`super::super::terminator::raise`] walker, which packages an
//! error payload into the body's return sum.
//!
//! # Submodule layout
//! - [`make_variant`] — `MakeVariant` dispatch + the shared
//!   [`build_variant_value`] SSA-building helper.
//! - [`in_alloca`] — the byte-preserving [`build_variant_in_alloca`]
//!   construction path used by `Assign` / `Raise` memcpy callers.

mod in_alloca;
mod make_variant;

pub(in crate::lower::rvalue) use make_variant::lower_make_variant;
pub(in crate::lower) use in_alloca::build_variant_in_alloca;
pub(in crate::lower) use make_variant::build_variant_value;

#[cfg(test)]
mod tests {
    use crate::Emitter;
    use edda_intern::Interner;
    use edda_mir::{
        BodyBuilder, Idx, MirPrim, MirType, Operand, ParamMode, Place, Rvalue, RvalueKind,
    };
    use edda_span::Span;

    use crate::lower::test_fixtures::linux_x86_64;

    /// Build a sum ADT analogous to `Option<i32>`:
    ///   discriminant 0 -> `None` (no fields)
    ///   discriminant 1 -> `Some(i32)` (one i32 field)
    /// `tag_width = U8`. The Some variant's payload is the largest,
    /// so it determines the LLVM sum-payload shape.
    fn build_option_program(
        interner: &Interner,
    ) -> (edda_mir::MirProgram, edda_mir::AdtId, MirType) {
        use edda_mir::{AdtDef, AdtKind, FieldDef, LayoutInfo, VariantDef};
        let mut pb = edda_mir::ProgramBuilder::new();
        let opt_name = interner.intern("Option");
        let none_name = interner.intern("None");
        let some_name = interner.intern("Some");
        let payload_name = interner.intern("0");
        let none = VariantDef {
            name: none_name,
            span: Span::DUMMY,
            fields: vec![],
            discriminant: Some(0),
        };
        let some = VariantDef {
            name: some_name,
            span: Span::DUMMY,
            fields: vec![FieldDef {
                name: payload_name,
                span: Span::DUMMY,
                ty: MirType::prim(MirPrim::I32),
            }],
            discriminant: Some(1),
        };
        let adt_id = pb.push_adt(AdtDef {
            name: opt_name,
            span: Span::DUMMY,
            kind: AdtKind::Sum,
            variants: vec![none, some],
            layout: LayoutInfo::natural(),
            tag_width: Some(MirPrim::U8),
        });
        let ty = MirType::new(edda_mir::MirTypeKind::Adt(adt_id));
        (pb.finish(), adt_id, ty)
    }

    /// Helper: build a product ADT `Point { x: i32, y: i32 }` for
    /// the negative test below.
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

    /// `fn mk_some(x: i32) -> Option { Some(x) }` via MakeVariant.
    /// The expected lowering is: build the variant payload as an
    /// SSA struct (one `insertvalue`), then alloca-store-load the
    /// outer `{ i8, { i32 } }` sum.
    #[test]
    fn make_variant_emits_alloca_store_load_sum() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();
        let (program, adt_id, option_ty) = build_option_program(&interner);

        let name = interner.intern("mk_some");
        let mut bb = BodyBuilder::new(name, Span::DUMMY, option_ty.clone());
        let ret_local = bb.return_slot(option_ty.clone(), Span::DUMMY);
        let x = bb.param(ParamMode::Let, MirType::prim(MirPrim::I32), Span::DUMMY);
        let mut block = bb.block();
        let block_id = block.id();
        block.assign(
            Span::DUMMY,
            Place::local(ret_local),
            Rvalue {
                span: Span::DUMMY,
                kind: RvalueKind::MakeVariant {
                    adt: adt_id,
                    variant: edda_mir::VariantIdx::new(1), // Some
                    fields: vec![Operand::Copy(Place::local(x))],
                },
                ty: option_ty.clone(),
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
            .expect("MakeVariant must lower");
        let ir = module.print_to_string().to_string();
        // Sum-shaped alloca for the round-trip.
        assert!(
            ir.contains("alloca { i8, { i32 } }"),
            "expected sum-shape alloca: {ir}"
        );
        // Tag pointer is the offset-0 field of the sum alloca, written via GEP.
        assert!(
            ir.contains("getelementptr inbounds nuw { i8, { i32 } }"),
            "expected GEP into sum-shaped alloca: {ir}"
        );
        // Tag value 1 (Some) stored.
        assert!(ir.contains("store i8 1"), "expected tag=1 store: {ir}");
        // Whole sum loaded back as SSA value.
        assert!(
            ir.contains("load { i8, { i32 } }"),
            "expected sum load: {ir}"
        );
    }

    /// `fn tag(o: Option) -> u8 { extract_tag(o) }` — ExtractTag is
    /// `extractvalue <sum>, 0` and the result has the sum's
    /// tag_width int type.
    #[test]
    fn extract_tag_emits_extractvalue_at_index_zero() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();
        let (program, _adt_id, option_ty) = build_option_program(&interner);

        let name = interner.intern("tag");
        let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::U8));
        let ret_local = bb.return_slot(MirType::prim(MirPrim::U8), Span::DUMMY);
        let o = bb.param(ParamMode::Let, option_ty, Span::DUMMY);
        let mut block = bb.block();
        let block_id = block.id();
        block.assign(
            Span::DUMMY,
            Place::local(ret_local),
            Rvalue {
                span: Span::DUMMY,
                kind: RvalueKind::ExtractTag {
                    subject: Operand::Copy(Place::local(o)),
                },
                ty: MirType::prim(MirPrim::U8),
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
            .expect("ExtractTag must lower");
        let ir = module.print_to_string().to_string();
        // The extract reads index 0 (`tag`) of the sum struct.
        assert!(
            ir.contains("extractvalue { i8, { i32 } } %") && ir.contains(", 0"),
            "expected extractvalue of field 0 on the sum: {ir}"
        );
        // Function returns u8 (the tag width).
        assert!(
            ir.contains("define i8 @tag"),
            "expected i8 return type: {ir}"
        );
    }

    /// `fn unwrap_some(o: Option) -> i32 { extract_field(o, Some, 0) }`
    /// — variant-bearing ExtractField goes through the alloca / store /
    /// GEP-payload / typed-load / extractvalue chain.
    #[test]
    fn extract_field_variant_emits_payload_gep_typed_load() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();
        let (program, _adt_id, option_ty) = build_option_program(&interner);

        let name = interner.intern("unwrap_some");
        let mut bb = BodyBuilder::new(name, Span::DUMMY, MirType::prim(MirPrim::I32));
        let ret_local = bb.return_slot(MirType::prim(MirPrim::I32), Span::DUMMY);
        let o = bb.param(ParamMode::Let, option_ty, Span::DUMMY);
        let mut block = bb.block();
        let block_id = block.id();
        block.assign(
            Span::DUMMY,
            Place::local(ret_local),
            Rvalue {
                span: Span::DUMMY,
                kind: RvalueKind::ExtractField {
                    subject: Operand::Copy(Place::local(o)),
                    variant: Some(edda_mir::VariantIdx::new(1)), // Some
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
            .expect("variant-bearing ExtractField must lower");
        let ir = module.print_to_string().to_string();
        // Round-trip alloca for the sum.
        assert!(
            ir.contains("alloca { i8, { i32 } }"),
            "expected sum-shape scratch alloca: {ir}"
        );
        // GEP into payload (field 1 of `{ i8, { i32 } }`).
        // LLVM 18.1.6 emits `inbounds nuw`; earlier 18.x emits plain
        // `inbounds`. Tolerate both spellings.
        assert!(
            ir.contains("getelementptr inbounds { i8, { i32 } }")
                || ir.contains("getelementptr inbounds nuw { i8, { i32 } }"),
            "expected sum-shape GEP: {ir}"
        );
        // Typed load reads the variant's own payload struct `{ i32 }`,
        // not the max-variant-payload type — for `Option<i32>` they
        // happen to coincide, so check `load { i32 }`.
        assert!(
            ir.contains("load { i32 }"),
            "expected typed load of variant payload: {ir}"
        );
        // The field is extracted from the loaded variant struct.
        assert!(
            ir.contains("extractvalue { i32 }"),
            "expected extractvalue from variant payload: {ir}"
        );
    }

    /// Reject MakeVariant against a product ADT.
    #[test]
    fn make_variant_on_product_is_rejected() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();
        let (program, point_adt, point_ty) = build_point_program(&interner);

        let name = interner.intern("bad");
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
                kind: RvalueKind::MakeVariant {
                    adt: point_adt,
                    variant: edda_mir::VariantIdx::new(0),
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
        let err = emitter
            .lower_body(&module, function, &body, &program, &interner, target.triple().arch())
            .expect_err("MakeVariant on a product ADT must be rejected");
        assert!(
            matches!(err, crate::CompileError::UnsupportedMirShape { shape, .. } if shape == "make-variant-on-product"),
            "wrong shape: {err:?}",
        );
    }

    /// Build a sum whose *largest* variant is not the `u128` one, so
    /// the LLVM max-variant payload blob is laid out in `i64`-chunks
    /// (8-byte aligned) and the `u128` variant's value is blitted into
    /// an 8-aligned slot. Constructing that variant must store the
    /// `i128`/`u128` payload with the slot's conservative 8-byte
    /// alignment — never its natural 16-byte alignment, which would
    /// make LLVM emit an aligned `movaps` to an 8-aligned address and
    /// fault at runtime.
    fn build_wide_or_num_program(
        interner: &Interner,
    ) -> (edda_mir::MirProgram, edda_mir::AdtId, MirType) {
        use edda_mir::{AdtDef, AdtKind, FieldDef, LayoutInfo, VariantDef};
        let mut pb = edda_mir::ProgramBuilder::new();
        let adt_name = interner.intern("WideOrNum");
        let wide_name = interner.intern("wide");
        let num_name = interner.intern("num");
        let f = |n: &str| interner.intern(n);
        // `wide(i64, i64, i64)` — 24 bytes, align 8 → the max variant.
        let wide = VariantDef {
            name: wide_name,
            span: Span::DUMMY,
            fields: vec![
                FieldDef { name: f("0"), span: Span::DUMMY, ty: MirType::prim(MirPrim::I64) },
                FieldDef { name: f("1"), span: Span::DUMMY, ty: MirType::prim(MirPrim::I64) },
                FieldDef { name: f("2"), span: Span::DUMMY, ty: MirType::prim(MirPrim::I64) },
            ],
            discriminant: Some(0),
        };
        // `num(u128)` — 16 bytes, align 16, blitted into the 8-aligned blob.
        let num = VariantDef {
            name: num_name,
            span: Span::DUMMY,
            fields: vec![FieldDef {
                name: f("0"),
                span: Span::DUMMY,
                ty: MirType::prim(MirPrim::U128),
            }],
            discriminant: Some(1),
        };
        let adt_id = pb.push_adt(AdtDef {
            name: adt_name,
            span: Span::DUMMY,
            kind: AdtKind::Sum,
            variants: vec![wide, num],
            layout: LayoutInfo::natural(),
            tag_width: Some(MirPrim::U8),
        });
        let ty = MirType::new(edda_mir::MirTypeKind::Adt(adt_id));
        (pb.finish(), adt_id, ty)
    }

    #[test]
    fn make_variant_u128_payload_into_byte_blob_uses_conservative_alignment() {
        let emitter = Emitter::new();
        let target = linux_x86_64();
        let module = emitter.tagged_module("m", &target).unwrap();
        let interner = Interner::new();
        let (program, adt_id, ty) = build_wide_or_num_program(&interner);

        let name = interner.intern("mk_num");
        let mut bb = BodyBuilder::new(name, Span::DUMMY, ty.clone());
        let ret_local = bb.return_slot(ty.clone(), Span::DUMMY);
        let v = bb.param(ParamMode::Let, MirType::prim(MirPrim::U128), Span::DUMMY);
        let mut block = bb.block();
        let block_id = block.id();
        block.assign(
            Span::DUMMY,
            Place::local(ret_local),
            Rvalue {
                span: Span::DUMMY,
                kind: RvalueKind::MakeVariant {
                    adt: adt_id,
                    variant: edda_mir::VariantIdx::new(1), // num(u128)
                    fields: vec![Operand::Copy(Place::local(v))],
                },
                ty: ty.clone(),
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
            .expect("MakeVariant with a u128 variant must lower");
        let ir = module.print_to_string().to_string();
        // The blob is `i64`-chunked (8-aligned), confirming the u128
        // payload is blitted into an under-aligned slot.
        assert!(
            ir.contains("alloca { i8, { i64, i64, i64 } }"),
            "expected 8-aligned i64-chunk blob alloca: {ir}"
        );
        // Every store/load of the i128 payload through the blob slot
        // must carry the conservative 8-byte alignment, never 16.
        for line in ir.lines() {
            let touches_payload_i128 = (line.contains("store { i128 }")
                || line.contains("load { i128 }")
                || line.contains("store i128")
                || line.contains("load i128"))
                && line.contains("mv.");
            if touches_payload_i128 {
                assert!(
                    line.contains("align 8"),
                    "i128 payload access into the 8-aligned sum blob must be `align 8`, got: {line}"
                );
            }
        }
    }
}
