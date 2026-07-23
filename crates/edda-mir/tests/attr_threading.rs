//! Tests for B2 slice-3 attribute threading.
//!
//! Verifies the contract from the slice-3 plan:
//! - [`edda_mir::Body::export_symbol`] / [`edda_mir::Body::abi`] default to `None`.
//! - [`edda_mir::BodyBuilder::set_export_symbol`] /
//!   [`edda_mir::BodyBuilder::set_abi`] write through to the
//!   finished body.
//! - The MIR pretty-printer surfaces `export "..."` and `abi <tag>`
//!   annotations on the body header when set, and suppresses them
//!   when at the default.
//! - The MIR pretty-printer surfaces `repr=K`, `align=N`, and
//!   `layout=P` annotations on an ADT header when its
//!   [`edda_mir::LayoutInfo`] carries overrides, and suppresses
//!   the prefix entirely when every field is at the natural default.

use edda_intern::Interner;
use edda_mir::{
    AbiTag, AdtDef, AdtKind, AlignBytes, Body, BodyBuilder, LayoutInfo, LayoutPolicy, MirProgram,
    MirType, MirTypeKind, PrettyPrinter, ReprKind, VariantDef,
};
use edda_span::Span;

fn unit_ty() -> MirType {
    MirType::new(MirTypeKind::Unit)
}

#[test]
fn body_export_and_abi_default_to_none() {
    let interner = Interner::new();
    let name = interner.intern("f");
    let body = Body::new(name, Span::DUMMY, unit_ty());
    assert!(body.export_symbol.is_none());
    assert!(body.abi.is_none());
}

#[test]
fn body_builder_set_export_symbol_writes_through() {
    let interner = Interner::new();
    let f = interner.intern("f");
    let target = interner.intern("edda_cb");

    let mut builder = BodyBuilder::new(f, Span::DUMMY, unit_ty());
    let _ = builder.return_slot(unit_ty(), Span::DUMMY);
    builder.set_export_symbol(Some(target));
    let body = builder.finish();
    assert_eq!(body.export_symbol, Some(target));
}

#[test]
fn body_builder_set_abi_writes_through() {
    let interner = Interner::new();
    let f = interner.intern("f");
    let sysv64 = interner.intern("sysv64");

    let mut builder = BodyBuilder::new(f, Span::DUMMY, unit_ty());
    let _ = builder.return_slot(unit_ty(), Span::DUMMY);
    builder.set_abi(Some(AbiTag::Named(sysv64)));
    let body = builder.finish();
    match body.abi {
        Some(AbiTag::Named(s)) => assert_eq!(interner.resolve(s), "sysv64"),
        other => panic!("expected Named(\"sysv64\"), got {:?}", other),
    }
}

#[test]
fn body_builder_setters_round_trip_none() {
    let interner = Interner::new();
    let f = interner.intern("f");
    let mut builder = BodyBuilder::new(f, Span::DUMMY, unit_ty());
    let _ = builder.return_slot(unit_ty(), Span::DUMMY);
    builder.set_export_symbol(None);
    builder.set_abi(None);
    let body = builder.finish();
    assert!(body.export_symbol.is_none());
    assert!(body.abi.is_none());
}

#[test]
fn pretty_body_header_renders_export_and_abi_when_set() {
    let interner = Interner::new();
    let mut program = MirProgram::new();
    let f = interner.intern("callback");
    let target = interner.intern("edda_callback");
    let sysv64 = interner.intern("sysv64");

    let mut builder = BodyBuilder::new(f, Span::DUMMY, unit_ty());
    let _ = builder.return_slot(unit_ty(), Span::DUMMY);
    builder.set_export_symbol(Some(target));
    builder.set_abi(Some(AbiTag::Named(sysv64)));
    let body = builder.finish();
    program.bodies.push(body);

    let mut p = PrettyPrinter::new(&interner);
    p.print_program(&program);
    let out = p.finish();
    assert!(
        out.contains("export \"edda_callback\""),
        "export annotation missing in:\n{}",
        out
    );
    assert!(
        out.contains("abi \"sysv64\""),
        "abi annotation missing in:\n{}",
        out
    );
}

#[test]
fn pretty_body_header_omits_annotations_when_unset() {
    let interner = Interner::new();
    let mut program = MirProgram::new();
    let f = interner.intern("plain");
    let mut builder = BodyBuilder::new(f, Span::DUMMY, unit_ty());
    let _ = builder.return_slot(unit_ty(), Span::DUMMY);
    let body = builder.finish();
    program.bodies.push(body);

    let mut p = PrettyPrinter::new(&interner);
    p.print_program(&program);
    let out = p.finish();
    assert!(
        !out.contains(" export "),
        "unexpected export annotation in:\n{}",
        out
    );
    assert!(
        !out.contains(" abi "),
        "unexpected abi annotation in:\n{}",
        out
    );
}

fn singleton_product_adt(interner: &Interner, layout: LayoutInfo) -> AdtDef {
    let name = interner.intern("Foo");
    let variant = VariantDef {
        name,
        span: Span::DUMMY,
        fields: vec![],
        discriminant: None,
    };
    AdtDef {
        name,
        span: Span::DUMMY,
        kind: AdtKind::Product,
        variants: vec![variant],
        layout,
        tag_width: None,
    }
}

#[test]
fn pretty_adt_header_renders_layout_overrides() {
    let interner = Interner::new();
    let layout = LayoutInfo {
        policy: LayoutPolicy::Packed,
        repr: ReprKind::C,
        abi: AbiTag::Edda,
        align: AlignBytes::new(16),
    };
    let adt = singleton_product_adt(&interner, layout);
    let mut program = MirProgram::new();
    program.adts.push(adt);

    let mut p = PrettyPrinter::new(&interner);
    p.print_program(&program);
    let out = p.finish();
    assert!(out.contains("repr=C"), "repr=C missing in:\n{}", out);
    assert!(
        out.contains("layout=packed"),
        "layout=packed missing in:\n{}",
        out
    );
    assert!(out.contains("align=16"), "align=16 missing in:\n{}", out);
}

#[test]
fn pretty_adt_header_omits_annotations_when_natural() {
    let interner = Interner::new();
    let adt = singleton_product_adt(&interner, LayoutInfo::natural());
    let mut program = MirProgram::new();
    program.adts.push(adt);

    let mut p = PrettyPrinter::new(&interner);
    p.print_program(&program);
    let out = p.finish();
    assert!(
        !out.contains("repr="),
        "natural layout should not emit repr=:\n{}",
        out
    );
    assert!(
        !out.contains("layout="),
        "natural layout should not emit layout=:\n{}",
        out
    );
    assert!(
        !out.contains("align="),
        "natural layout should not emit align=:\n{}",
        out
    );
}
