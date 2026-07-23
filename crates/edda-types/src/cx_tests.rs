//! Tests for [`TyCx`] and the [`lower_type_decl`] / [`build_ty_cx`] drivers.

use super::*;
use crate::test_support::{Harness, ident_for, ty_path};
use crate::lower::{LowerCx, lower_type_decl};
use crate::prim::Primitive;
use crate::ty::TyInterner;
use edda_resolve::{BindingId, ModuleId};
use edda_span::Span;
use edda_syntax::ast::{self, TypeField, Variant, VariantPayload as AstVariantPayload, Visibility};

fn binding(module: u32, index: u32) -> BindingId {
    BindingId::new(ModuleId::new(module), index)
}

#[test]
fn ty_cx_empty_lookup_misses() {
    let cx = TyCx::new();
    assert!(cx.sig(binding(0, 0)).is_none());
    assert!(cx.type_decl(binding(0, 0)).is_none());
    assert_eq!(cx.sig_count(), 0);
    assert_eq!(cx.type_decl_count(), 0);
}

#[test]
fn ty_cx_insert_and_lookup_roundtrips() {
    let ty_interner = TyInterner::new();
    let mut cx = TyCx::new();
    let id = binding(0, 4);
    let info = TypeDeclInfo {
        span: Span::DUMMY,
        linearity: None,
        kind: TypeDeclShape::Product {
            fields: Box::new([FieldInfo {
                span: Span::DUMMY,
                name: edda_intern::Interner::new().intern("x"),
                ty: ty_interner.prim(Primitive::I32),
                refinement: None,
            }]),
        },
    };
    cx.insert_type_decl(id, info);
    assert_eq!(cx.type_decl_count(), 1);
    let stored = cx.type_decl(id).expect("inserted");
    assert!(matches!(stored.kind, TypeDeclShape::Product { .. }));
}

#[test]
fn type_decl_info_field_lookup() {
    let ty_interner = TyInterner::new();
    let interner = edda_intern::Interner::new();
    let x_sym = interner.intern("x");
    let y_sym = interner.intern("y");
    let info = TypeDeclInfo {
        span: Span::DUMMY,
        linearity: None,
        kind: TypeDeclShape::Product {
            fields: Box::new([
                FieldInfo {
                    span: Span::DUMMY,
                    name: x_sym,
                    ty: ty_interner.prim(Primitive::F64),
                    refinement: None,
                },
                FieldInfo {
                    span: Span::DUMMY,
                    name: y_sym,
                    ty: ty_interner.prim(Primitive::F64),
                    refinement: None,
                },
            ]),
        },
    };
    assert!(info.field(x_sym).is_some());
    assert!(info.field(y_sym).is_some());
    assert!(info.field(interner.intern("z")).is_none());
    assert_eq!(info.fields().len(), 2);
    assert!(info.variants().is_empty());
}

#[test]
fn type_decl_info_variant_lookup() {
    let ty_interner = TyInterner::new();
    let interner = edda_intern::Interner::new();
    let red_sym = interner.intern("red");
    let info = TypeDeclInfo {
        span: Span::DUMMY,
        linearity: None,
        kind: TypeDeclShape::Sum {
            variants: Box::new([VariantInfo {
                span: Span::DUMMY,
                name: red_sym,
                payload: VariantPayloadInfo::Tuple {
                    elems: Box::new([
                        ty_interner.prim(Primitive::U8),
                        ty_interner.prim(Primitive::U8),
                        ty_interner.prim(Primitive::U8),
                    ]),
                },
            }]),
        },
    };
    assert!(info.variant(red_sym).is_some());
    assert!(info.variant(interner.intern("blue")).is_none());
    assert_eq!(info.variants().len(), 1);
    assert!(info.fields().is_empty());
}

#[test]
fn lower_type_decl_product_records_field_types() {
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let decl = ast::TypeDecl {
        span: Span::DUMMY,
        stability: None,
        visibility: Visibility::Module,
        linearity: None,
        name: ident_for(&h.interner, "Point"),
        generics: vec![],
        kind: ast::TypeDeclKind::Product {
            fields: vec![
                TypeField {
                    span: Span::DUMMY,
                    name: ident_for(&h.interner, "x"),
                    ty: ty_path(&h.interner, "f64"),
                    refinement: None,
                },
                TypeField {
                    span: Span::DUMMY,
                    name: ident_for(&h.interner, "y"),
                    ty: ty_path(&h.interner, "f64"),
                    refinement: None,
                },
            ],
        },
    };
    let info = lower_type_decl(&decl, &cx, &mut h.diags, &h.lint_cfg);
    match info.kind {
        TypeDeclShape::Product { fields } => {
            assert_eq!(fields.len(), 2);
            let f64_id = h.ty_interner.prim(Primitive::F64);
            for f in fields.iter() {
                assert_eq!(f.ty, f64_id);
            }
        }
        _ => panic!("expected product"),
    }
    assert!(h.diags.is_empty());
}

#[test]
fn lower_type_decl_sum_records_variants() {
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let decl = ast::TypeDecl {
        span: Span::DUMMY,
        stability: None,
        visibility: Visibility::Public,
        linearity: None,
        name: ident_for(&h.interner, "Color"),
        generics: vec![],
        kind: ast::TypeDeclKind::Sum {
            variants: vec![
                Variant {
                    span: Span::DUMMY,
                    name: ident_for(&h.interner, "red"),
                    payload: AstVariantPayload::Unit,
                },
                Variant {
                    span: Span::DUMMY,
                    name: ident_for(&h.interner, "rgb"),
                    payload: AstVariantPayload::Tuple(vec![
                        ty_path(&h.interner, "u8"),
                        ty_path(&h.interner, "u8"),
                        ty_path(&h.interner, "u8"),
                    ]),
                },
            ],
        },
    };
    let info = lower_type_decl(&decl, &cx, &mut h.diags, &h.lint_cfg);
    match info.kind {
        TypeDeclShape::Sum { variants } => {
            assert_eq!(variants.len(), 2);
            assert!(matches!(variants[0].payload, VariantPayloadInfo::Unit));
            let u8_id = h.ty_interner.prim(Primitive::U8);
            match &variants[1].payload {
                VariantPayloadInfo::Tuple { elems } => {
                    assert_eq!(elems.len(), 3);
                    for t in elems.iter() {
                        assert_eq!(*t, u8_id);
                    }
                }
                _ => panic!("expected tuple payload"),
            }
        }
        _ => panic!("expected sum"),
    }
    assert!(h.diags.is_empty());
}

