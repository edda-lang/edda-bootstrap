//! Tests for the item-level encoders.

use super::*;
use crate::body::tags;
use crate::body::test_support::{expr, ident, ty, PassThroughResolver};
use edda_intern::Interner;
use edda_syntax::ast::{
    Block, ExprKind, GenericKind, GenericParam, Literal, ParamMode, RefinementClause,
    RefinementKind, TypeDecl, TypeDeclKind, TypeField, TypeKind, Variant, VariantPayload,
    Visibility,
};

fn empty_block() -> Block {
    Block {
        span: edda_span::Span::DUMMY,
        stmts: vec![],
        trailing: None,
    }
}

fn empty_fn_body() -> FnBody {
    FnBody::Block(empty_block())
}

#[test]
fn visibility_tags() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_visibility(Visibility::Module);
    enc.write_visibility(Visibility::Public);
    assert_eq!(
        enc.into_bytes(),
        vec![tags::visibility::MODULE, tags::visibility::PUBLIC],
    );
}

#[test]
fn generic_kind_tags() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_generic_kind(GenericKind::Type);
    enc.write_generic_kind(GenericKind::Comptime);
    assert_eq!(
        enc.into_bytes(),
        vec![tags::generic_kind::TYPE, tags::generic_kind::COMPTIME],
    );
}

#[test]
fn generic_param_writes_kind_then_name() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_generic_param(&GenericParam {
        span: edda_span::Span::DUMMY,
        kind: GenericKind::Comptime,
        name: ident(&interner, "n"),
        ty: None,
        admits: vec![],
    });
    let bytes = enc.into_bytes();
    assert_eq!(bytes[0], tags::generic_kind::COMPTIME);
    assert_eq!(&bytes[1..5], &1u32.to_le_bytes());
    assert_eq!(bytes[5], b'n');
}

#[test]
fn param_writes_mode_name_type() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_param(&edda_syntax::ast::Param {
        span: edda_span::Span::DUMMY,
        name: ident(&interner, "x"),
        mode: ParamMode::Mutable,
        ty: ty(TypeKind::Unit),
    });
    let bytes = enc.into_bytes();
    assert_eq!(bytes[0], tags::param_mode::INOUT);
    assert_eq!(&bytes[1..5], &1u32.to_le_bytes());
    assert_eq!(bytes[5], b'x');
    assert_eq!(bytes[6], tags::type_kind::UNIT);
}

#[test]
fn type_field_with_no_refinement() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_type_field(&TypeField {
        span: edda_span::Span::DUMMY,
        name: ident(&interner, "x"),
        ty: ty(TypeKind::Unit),
        refinement: None,
    });
    let bytes = enc.into_bytes();
    // name "x"(5) + UNIT(1) + None(1) = 7
    assert_eq!(bytes.len(), 7);
    assert_eq!(bytes[6], tags::option_flag::NONE);
}

#[test]
fn type_field_with_refinement_writes_predicate() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_type_field(&TypeField {
        span: edda_span::Span::DUMMY,
        name: ident(&interner, "len"),
        ty: ty(TypeKind::Unit),
        refinement: Some(expr(ExprKind::Literal(Literal::Bool(true)))),
    });
    let bytes = enc.into_bytes();
    // ident "len"(4+3) + UNIT(1) + Some(1) + LITERAL(1) + BOOL(1) + true(1) = 12
    assert_eq!(bytes.len(), 4 + 3 + 1 + 1 + 1 + 1 + 1);
    assert_eq!(bytes[8], tags::option_flag::SOME);
    assert_eq!(bytes[9], tags::expr_kind::LITERAL);
    assert_eq!(bytes[10], tags::literal::BOOL);
    assert_eq!(bytes[11], 0x01);
}

#[test]
fn variant_payload_unit_is_one_byte() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_variant_payload(&VariantPayload::Unit);
    assert_eq!(enc.into_bytes(), vec![tags::variant_payload::UNIT]);
}

#[test]
fn variant_payload_tuple_writes_count_and_types() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_variant_payload(&VariantPayload::Tuple(vec![
        ty(TypeKind::Unit),
        ty(TypeKind::Meta),
    ]));
    let bytes = enc.into_bytes();
    assert_eq!(bytes[0], tags::variant_payload::TUPLE);
    assert_eq!(&bytes[1..5], &2u32.to_le_bytes());
    assert_eq!(bytes[5], tags::type_kind::UNIT);
    assert_eq!(bytes[6], tags::type_kind::META);
}

#[test]
fn variant_payload_struct_uses_type_fields() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_variant_payload(&VariantPayload::Struct(vec![TypeField {
        span: edda_span::Span::DUMMY,
        name: ident(&interner, "x"),
        ty: ty(TypeKind::Unit),
        refinement: None,
    }]));
    let bytes = enc.into_bytes();
    assert_eq!(bytes[0], tags::variant_payload::STRUCT);
    assert_eq!(&bytes[1..5], &1u32.to_le_bytes());
}

#[test]
fn refinement_kind_tags() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_refinement_kind(RefinementKind::Where);
    enc.write_refinement_kind(RefinementKind::Requires);
    enc.write_refinement_kind(RefinementKind::Ensures);
    assert_eq!(
        enc.into_bytes(),
        vec![
            tags::refinement_kind::WHERE,
            tags::refinement_kind::REQUIRES,
            tags::refinement_kind::ENSURES,
        ],
    );
}

#[test]
fn refinement_clause_writes_kind_then_predicate() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_refinement_clause(&RefinementClause {
        span: edda_span::Span::DUMMY,
        kind: RefinementKind::Requires,
        pred: expr(ExprKind::Literal(Literal::Bool(true))),
    });
    let bytes = enc.into_bytes();
    assert_eq!(bytes[0], tags::refinement_kind::REQUIRES);
    assert_eq!(bytes[1], tags::expr_kind::LITERAL);
    assert_eq!(bytes[2], tags::literal::BOOL);
    assert_eq!(bytes[3], 0x01);
}

#[test]
fn type_decl_product_no_generics() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_type_decl(&TypeDecl {
        span: edda_span::Span::DUMMY,
        stability: None,
        visibility: Visibility::Public,
        linearity: None,
        name: ident(&interner, "Point"),
        generics: vec![],
        kind: TypeDeclKind::Product {
            fields: vec![TypeField {
                span: edda_span::Span::DUMMY,
                name: ident(&interner, "x"),
                ty: ty(TypeKind::Unit),
                refinement: None,
            }],
        },
    });
    let bytes = enc.into_bytes();
    // visibility PUBLIC(1) + name "Point"(4+5) + 0 generics(4) +
    // PRODUCT(1) + 1 field count(4) + field "x"(4+1) + UNIT(1) +
    // None(1) = 26
    assert_eq!(bytes[0], tags::visibility::PUBLIC);
    assert_eq!(&bytes[1..5], &("Point".len() as u32).to_le_bytes());
    assert_eq!(&bytes[10..14], &0u32.to_le_bytes()); // generics count
    assert_eq!(bytes[14], tags::type_decl_kind::PRODUCT);
    assert_eq!(&bytes[15..19], &1u32.to_le_bytes());
}

#[test]
fn type_decl_sum_with_variants() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_type_decl(&TypeDecl {
        span: edda_span::Span::DUMMY,
        stability: None,
        visibility: Visibility::Module,
        linearity: None,
        name: ident(&interner, "Color"),
        generics: vec![],
        kind: TypeDeclKind::Sum {
            variants: vec![
                Variant {
                    span: edda_span::Span::DUMMY,
                    name: ident(&interner, "red"),
                    payload: VariantPayload::Unit,
                },
                Variant {
                    span: edda_span::Span::DUMMY,
                    name: ident(&interner, "rgb"),
                    payload: VariantPayload::Tuple(vec![
                        ty(TypeKind::Unit),
                        ty(TypeKind::Unit),
                        ty(TypeKind::Unit),
                    ]),
                },
            ],
        },
    });
    let bytes = enc.into_bytes();
    assert_eq!(bytes[0], tags::visibility::MODULE);
    // After visibility + name "Color"(4+5) + 0 generics = 14
    assert_eq!(bytes[14], tags::type_decl_kind::SUM);
    assert_eq!(&bytes[15..19], &2u32.to_le_bytes()); // 2 variants
}

#[test]
fn fn_decl_signature_only() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_fn_decl(&FnDecl {
        span: edda_span::Span::DUMMY,
        stability: None,
        visibility: Visibility::Public,
        refinement_stable: false,
        name: ident(&interner, "add"),
        outbound_generics: vec![],
        generics: vec![],
        params: vec![edda_syntax::ast::Param {
            span: edda_span::Span::DUMMY,
            name: ident(&interner, "x"),
            mode: ParamMode::Default,
            ty: ty(TypeKind::Unit),
        }],
        return_ty: Some(ty(TypeKind::Unit)),
        return_mode: edda_syntax::ast::ReturnMode::ByValue,
        effects: None,
        refinements: vec![],
        body: empty_fn_body(),
    });
    let bytes = enc.into_bytes();
    // visibility PUBLIC
    assert_eq!(bytes[0], tags::visibility::PUBLIC);
    // name "add" = u32(3) + "add"(3)
    assert_eq!(&bytes[1..5], &3u32.to_le_bytes());
    assert_eq!(&bytes[5..8], b"add");
    // 0 generics
    assert_eq!(&bytes[8..12], &0u32.to_le_bytes());
    // 1 param: count(4) + DEFAULT(1) + "x" ident(4+1) + UNIT(1)
    assert_eq!(&bytes[12..16], &1u32.to_le_bytes());
    assert_eq!(bytes[16], tags::param_mode::DEFAULT);
    // Return ty: Some + UNIT, then effects: None, then 0 refinements, then block
    // Locate the return-ty-present byte: 16 + (1 mode + 4 name_len + 1 name + 1 ty) = 23
    assert_eq!(bytes[23], tags::option_flag::SOME);
    assert_eq!(bytes[24], tags::type_kind::UNIT);
    assert_eq!(bytes[25], tags::option_flag::NONE); // effects None
    assert_eq!(&bytes[26..30], &0u32.to_le_bytes()); // 0 refinements
    // fn_body: BLOCK discriminator at byte 30, then block: 0 stmts + trailing None
    assert_eq!(bytes[30], tags::fn_body::BLOCK);
    assert_eq!(&bytes[31..35], &0u32.to_le_bytes());
    assert_eq!(bytes[35], tags::option_flag::NONE);
}

#[test]
fn fn_decl_refinement_changes_hash() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mk = |refinements| FnDecl {
        span: edda_span::Span::DUMMY,
        stability: None,
        visibility: Visibility::Module,
        refinement_stable: false,
        name: ident(&interner, "f"),
        outbound_generics: vec![],
        generics: vec![],
        params: vec![],
        return_ty: None,
        return_mode: edda_syntax::ast::ReturnMode::ByValue,
        effects: None,
        refinements,
        body: empty_fn_body(),
    };
    let mut without = Encoder::new(&interner, &resolver);
    let mut with_req = Encoder::new(&interner, &resolver);
    without.write_fn_decl(&mk(vec![]));
    with_req.write_fn_decl(&mk(vec![RefinementClause {
        span: edda_span::Span::DUMMY,
        kind: RefinementKind::Requires,
        pred: expr(ExprKind::Literal(Literal::Bool(true))),
    }]));
    assert_ne!(without.into_bytes(), with_req.into_bytes());
}

#[test]
fn fn_decl_generic_param_changes_hash() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mk = |generics| FnDecl {
        span: edda_span::Span::DUMMY,
        stability: None,
        visibility: Visibility::Module,
        refinement_stable: false,
        name: ident(&interner, "f"),
        outbound_generics: vec![],
        generics,
        params: vec![],
        return_ty: None,
        return_mode: edda_syntax::ast::ReturnMode::ByValue,
        effects: None,
        refinements: vec![],
        body: empty_fn_body(),
    };
    let mut without = Encoder::new(&interner, &resolver);
    let mut with_t = Encoder::new(&interner, &resolver);
    without.write_fn_decl(&mk(vec![]));
    with_t.write_fn_decl(&mk(vec![GenericParam {
        span: edda_span::Span::DUMMY,
        kind: GenericKind::Type,
        name: ident(&interner, "T"),
        ty: None,
        admits: vec![],
    }]));
    assert_ne!(without.into_bytes(), with_t.into_bytes());
}
