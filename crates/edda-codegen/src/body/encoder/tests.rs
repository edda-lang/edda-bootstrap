//! Tests for the AST encoder's type / effect-row / param-mode encoders.

use super::*;
use crate::body::test_support::{dummy_span, fn_param, ident, path, ty, PassThroughResolver};
use edda_intern::Interner;
use edda_syntax::ast::{EffectMember, EffectRow, ParamMode, TypeKind};

#[test]
fn unit_type_is_one_byte() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_type(&ty(TypeKind::Unit));
    assert_eq!(enc.into_bytes(), vec![tags::type_kind::UNIT]);
}

#[test]
fn meta_type_is_one_byte() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_type(&ty(TypeKind::Meta));
    assert_eq!(enc.into_bytes(), vec![tags::type_kind::META]);
}

#[test]
fn path_type_writes_qualified_name() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let p = path(&interner, &["std", "option", "Option"]);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_type(&ty(TypeKind::Path(p)));

    let mut expected = vec![tags::type_kind::PATH];
    expected.extend_from_slice(&("std.option.Option".len() as u32).to_le_bytes());
    expected.extend_from_slice(b"std.option.Option");
    assert_eq!(enc.into_bytes(), expected);
}

#[test]
fn tuple_type_preserves_order() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc1 = Encoder::new(&interner, &resolver);
    let mut enc2 = Encoder::new(&interner, &resolver);
    enc1.write_type(&ty(TypeKind::Tuple(vec![
        ty(TypeKind::Unit),
        ty(TypeKind::Meta),
    ])));
    enc2.write_type(&ty(TypeKind::Tuple(vec![
        ty(TypeKind::Meta),
        ty(TypeKind::Unit),
    ])));
    assert_ne!(enc1.into_bytes(), enc2.into_bytes());
}

#[test]
fn slice_nests_inner_type() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_type(&ty(TypeKind::Slice(Box::new(ty(TypeKind::Unit)))));
    assert_eq!(
        enc.into_bytes(),
        vec![tags::type_kind::SLICE, tags::type_kind::UNIT],
    );
}

#[test]
fn function_type_encodes_params_ret_and_effects_absent() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_type(&ty(TypeKind::Function {
        params: vec![
            fn_param(ty(TypeKind::Unit)),
            fn_param(ty(TypeKind::Meta)),
        ],
        ret: Box::new(ty(TypeKind::Unit)),
        effects: None,
    }));
    let bytes = enc.into_bytes();
    assert_eq!(bytes[0], tags::type_kind::FUNCTION);
    assert_eq!(&bytes[1..5], &2u32.to_le_bytes());
    assert_eq!(bytes[5], tags::type_kind::UNIT);
    assert_eq!(bytes[6], tags::type_kind::META);
    assert_eq!(bytes[7], tags::type_kind::UNIT); // return type
    assert_eq!(bytes[8], 0x00); // effects: None
    assert_eq!(bytes.len(), 9);
}

#[test]
fn function_type_with_effects_writes_presence_flag() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    let row = EffectRow {
        span: dummy_span(),
        members: vec![],
    };
    enc.write_type(&ty(TypeKind::Function {
        params: vec![],
        ret: Box::new(ty(TypeKind::Unit)),
        effects: Some(row),
    }));
    let bytes = enc.into_bytes();
    // FUNCTION tag, 0 params, UNIT return, 0x01 effects-present, 0 members.
    assert_eq!(bytes[0], tags::type_kind::FUNCTION);
    assert_eq!(&bytes[1..5], &0u32.to_le_bytes());
    assert_eq!(bytes[5], tags::type_kind::UNIT);
    assert_eq!(bytes[6], 0x01);
    assert_eq!(&bytes[7..11], &0u32.to_le_bytes());
    assert_eq!(bytes.len(), 11);
}

#[test]
fn comptime_wraps_inner() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_type(&ty(TypeKind::Comptime(Box::new(ty(TypeKind::Unit)))));
    assert_eq!(
        enc.into_bytes(),
        vec![tags::type_kind::COMPTIME, tags::type_kind::UNIT],
    );
}

#[test]
fn refined_encodes_base_then_predicate() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    let pred = edda_syntax::ast::Expr {
        span: dummy_span(),
        kind: edda_syntax::ast::ExprKind::Path(path(&interner, &["x"])),
    };
    enc.write_type(&ty(TypeKind::Refined {
        base: Box::new(ty(TypeKind::Unit)),
        pred,
    }));
    let bytes = enc.into_bytes();
    // REFINED tag, base = UNIT tag, then expr: PATH tag + qname.
    assert_eq!(bytes[0], tags::type_kind::REFINED);
    assert_eq!(bytes[1], tags::type_kind::UNIT);
    assert_eq!(bytes[2], tags::expr_kind::PATH);
    assert_eq!(&bytes[3..7], &("x".len() as u32).to_le_bytes());
    assert_eq!(&bytes[7..], b"x");
}

#[test]
fn param_mode_tags() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_param_mode(ParamMode::Default);
    enc.write_param_mode(ParamMode::Mutable);
    enc.write_param_mode(ParamMode::Take);
    enc.write_param_mode(ParamMode::Init);
    assert_eq!(
        enc.into_bytes(),
        vec![
            tags::param_mode::DEFAULT,
            tags::param_mode::INOUT,
            tags::param_mode::SINK,
            tags::param_mode::SET,
        ],
    );
}

#[test]
fn effect_row_capability_member() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let row = EffectRow {
        span: dummy_span(),
        members: vec![EffectMember::Capability(ident(&interner, "allocator"))],
    };
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_effect_row(&row);
    let bytes = enc.into_bytes();
    // 4-byte count, 1 member tag (CAPABILITY=0x00), 4-byte name length, name bytes.
    assert_eq!(&bytes[0..4], &1u32.to_le_bytes());
    assert_eq!(bytes[4], tags::effect_member::CAPABILITY);
    assert_eq!(&bytes[5..9], &("allocator".len() as u32).to_le_bytes());
    assert_eq!(&bytes[9..], b"allocator");
}

#[test]
fn effect_row_named_member_includes_type() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let row = EffectRow {
        span: dummy_span(),
        members: vec![EffectMember::Named {
            name: ident(&interner, "err"),
            ty: ty(TypeKind::Path(path(&interner, &["IoError"]))),
        }],
    };
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_effect_row(&row);
    let bytes = enc.into_bytes();
    // count(4) tag(1) name_len(4) name(3) path_tag(1) qname_len(4) qname(7).
    assert_eq!(bytes.len(), 4 + 1 + 4 + 3 + 1 + 4 + 7);
    assert_eq!(&bytes[0..4], &1u32.to_le_bytes());
    assert_eq!(bytes[4], tags::effect_member::NAMED);
    assert_eq!(&bytes[5..9], &3u32.to_le_bytes());
    assert_eq!(&bytes[9..12], b"err");
    assert_eq!(bytes[12], tags::type_kind::PATH);
    assert_eq!(&bytes[13..17], &7u32.to_le_bytes());
    assert_eq!(&bytes[17..], b"IoError");
}

#[test]
fn effect_row_spread_resolves_to_qualified() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let row = EffectRow {
        span: dummy_span(),
        members: vec![EffectMember::Spread(path(
            &interner,
            &["module", "ParseEffects"],
        ))],
    };
    let mut enc = Encoder::new(&interner, &resolver);
    enc.write_effect_row(&row);
    let bytes = enc.into_bytes();
    assert_eq!(&bytes[0..4], &1u32.to_le_bytes());
    assert_eq!(bytes[4], tags::effect_member::SPREAD);
    assert_eq!(&bytes[5..9], &("module.ParseEffects".len() as u32).to_le_bytes());
    assert_eq!(&bytes[9..], b"module.ParseEffects");
}

#[test]
fn effect_row_source_order_is_preserved() {
    let interner = Interner::new();
    let resolver = PassThroughResolver::new(&interner);
    let cap_first = EffectRow {
        span: dummy_span(),
        members: vec![
            EffectMember::Capability(ident(&interner, "a")),
            EffectMember::Capability(ident(&interner, "b")),
        ],
    };
    let cap_second = EffectRow {
        span: dummy_span(),
        members: vec![
            EffectMember::Capability(ident(&interner, "b")),
            EffectMember::Capability(ident(&interner, "a")),
        ],
    };
    let mut e1 = Encoder::new(&interner, &resolver);
    let mut e2 = Encoder::new(&interner, &resolver);
    e1.write_effect_row(&cap_first);
    e2.write_effect_row(&cap_second);
    assert_ne!(e1.into_bytes(), e2.into_bytes());
}
