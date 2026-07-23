use super::*;
use crate::argument::{EffectRowArgument, PrimitiveValue, UserValue};
use edda_span::Span;
use edda_syntax::ast::{Ident, Path, Type};

fn ident(interner: &Interner, name: &str) -> Ident {
    Ident {
        name: interner.intern(name),
        span: Span::DUMMY,
    }
}

fn gp(interner: &Interner, name: &str, kind: GenericKind) -> GenericParam {
    GenericParam {
        span: Span::DUMMY,
        kind,
        name: ident(interner, name),
        ty: None,
        admits: vec![],
    }
}

fn gp_typed(interner: &Interner, name: &str, kind: GenericKind, ty: Type) -> GenericParam {
    GenericParam {
        span: Span::DUMMY,
        kind,
        name: ident(interner, name),
        ty: Some(ty),
        admits: vec![],
    }
}

fn ty_meta() -> Type {
    Type { span: Span::DUMMY, kind: TypeKind::Meta }
}

fn ty_path(interner: &Interner, name: &str) -> Type {
    Type {
        span: Span::DUMMY,
        kind: TypeKind::Path(Path {
            segments: vec![ident(interner, name)],
            span: Span::DUMMY,
        }),
    }
}

#[test]
fn empty_binds_zero_generics_to_empty_tuple() {
    let interner = Interner::new();
    let m = SubstitutionMap::bind("std.Foo", &[], &ArgumentTuple::empty(), &interner).unwrap();
    assert!(m.is_empty());
    assert_eq!(m.len(), 0);
}

#[test]
fn arity_mismatch_rejects_extra_args() {
    let interner = Interner::new();
    let generics = vec![gp(&interner, "T", GenericKind::Type)];
    let args = ArgumentTuple::new(vec![
        Argument::Type(SmolStr::new("i32")),
        Argument::Type(SmolStr::new("u32")),
    ]);
    let err =
        SubstitutionMap::bind("std.Foo", &generics, &args, &interner).expect_err("arity err");
    match err {
        CodegenError::MonomorphArityMismatch {
            expected, found, ..
        } => {
            assert_eq!(expected, 1);
            assert_eq!(found, 2);
        }
        other => panic!("expected MonomorphArityMismatch, got {other:?}"),
    }
}

#[test]
fn arity_mismatch_rejects_missing_args() {
    let interner = Interner::new();
    let generics = vec![
        gp(&interner, "T", GenericKind::Type),
        gp(&interner, "U", GenericKind::Type),
    ];
    let args = ArgumentTuple::new(vec![Argument::Type(SmolStr::new("i32"))]);
    let err = SubstitutionMap::bind("std.Foo", &generics, &args, &interner).unwrap_err();
    assert!(matches!(err, CodegenError::MonomorphArityMismatch { .. }));
}

#[test]
fn type_generic_with_primitive_arg_rejected() {
    let interner = Interner::new();
    let generics = vec![gp(&interner, "T", GenericKind::Type)];
    let args = ArgumentTuple::new(vec![Argument::Primitive(PrimitiveValue::U32(7))]);
    let err = SubstitutionMap::bind("std.Foo", &generics, &args, &interner).unwrap_err();
    match err {
        CodegenError::MonomorphKindMismatch {
            generic_name,
            position,
            generic_kind,
            argument_kind_tag,
            ..
        } => {
            assert_eq!(generic_name.as_str(), "T");
            assert_eq!(position, 0);
            assert_eq!(generic_kind, "type");
            assert_eq!(argument_kind_tag, 0x03);
        }
        other => panic!("expected MonomorphKindMismatch, got {other:?}"),
    }
}

#[test]
fn comptime_numeric_generic_with_type_arg_rejected() {
    // `comptime n: i32` expects a Primitive argument; passing a Type
    // argument is a kind mismatch.
    let interner = Interner::new();
    let generics = vec![gp_typed(
        &interner,
        "n",
        GenericKind::Comptime,
        ty_path(&interner, "i32"),
    )];
    let args = ArgumentTuple::new(vec![Argument::Type(SmolStr::new("i32"))]);
    let err = SubstitutionMap::bind("std.Foo", &generics, &args, &interner).unwrap_err();
    match err {
        CodegenError::MonomorphKindMismatch {
            generic_kind,
            argument_kind_tag,
            ..
        } => {
            assert_eq!(generic_kind, "comptime");
            assert_eq!(argument_kind_tag, 0x01);
        }
        other => panic!("expected MonomorphKindMismatch, got {other:?}"),
    }
}

#[test]
fn comptime_type_generic_accepts_type_arg() {
    // `comptime T: Type` is the canonical stdlib spelling for a
    // type-parameterised spec (`spec std.alloc.Box(comptime T: Type)`).
    // The `Type` meta-type IS a comptime value, so a `Type` argument
    // is the only valid input — accept it.
    let interner = Interner::new();
    let generics = vec![gp_typed(
        &interner,
        "T",
        GenericKind::Comptime,
        ty_meta(),
    )];
    let args = ArgumentTuple::new(vec![Argument::Type(SmolStr::new("parse.Expr"))]);
    let m = SubstitutionMap::bind("std.alloc.Box", &generics, &args, &interner)
        .expect("comptime T: Type with Type arg should bind");
    assert_eq!(m.len(), 1);
}

#[test]
fn with_parent_siblings_registers_qualified_renames() {
    // Stdlib `std.alloc.Box`'s body references `AllocError` bare
    // (sibling type-decl in the parent `std.alloc` module). After
    // monomorphisation the body lives in `std.alloc.Box_Expr`, so
    // the bare reference needs to rewrite to `alloc.AllocError`.
    let interner = Interner::new();
    let alloc_error = interner.intern("AllocError");
    let m = SubstitutionMap::empty().with_parent_siblings(
        "alloc",
        &[alloc_error],
        &interner,
    );
    let binding = m
        .lookup(alloc_error)
        .expect("AllocError rename was not registered");
    match &binding.value {
        Argument::Type(qname) => assert_eq!(qname.as_str(), "alloc.AllocError"),
        other => panic!("expected Argument::Type(alloc.AllocError), got {other:?}"),
    }
}

#[test]
fn with_parent_siblings_no_op_for_empty_inputs() {
    let interner = Interner::new();
    let foo = interner.intern("Foo");
    assert!(
        SubstitutionMap::empty()
            .with_parent_siblings("", &[foo], &interner)
            .is_empty(),
    );
    assert!(
        SubstitutionMap::empty()
            .with_parent_siblings("alloc", &[], &interner)
            .is_empty(),
    );
}

#[test]
fn with_sibling_renames_maps_pre_to_post_mangled_name() {
    // `spec std.hashmap.StringMap(comptime V: Type)` body contains
    // `spec std.option.Option(V)`. After binding `V := f64`, the
    // sibling-spec's mangled name shifts from `Option_V` to
    // `Option_f64`; body references to `Option_V.*` must follow.
    use edda_span::Span;
    use edda_syntax::ast::{Ident, Item, ItemKind, Path, SpecInvocation, Expr, ExprKind};

    let interner = Interner::new();
    let generics = vec![gp_typed(
        &interner,
        "V",
        GenericKind::Comptime,
        ty_meta(),
    )];
    let args = ArgumentTuple::new(vec![Argument::Type(SmolStr::new("f64"))]);
    let m = SubstitutionMap::bind("std.hashmap.StringMap", &generics, &args, &interner).unwrap();

    let v_ident = Ident { name: interner.intern("V"), span: Span::DUMMY };
    let option_si = SpecInvocation {
        span: Span::DUMMY,
        path: Path {
            segments: vec![
                Ident { name: interner.intern("std"), span: Span::DUMMY },
                Ident { name: interner.intern("option"), span: Span::DUMMY },
                Ident { name: interner.intern("Option"), span: Span::DUMMY },
            ],
            span: Span::DUMMY,
        },
        args: vec![Expr {
            span: Span::DUMMY,
            kind: ExprKind::Path(Path {
                segments: vec![v_ident],
                span: Span::DUMMY,
            }),
        }],
    };
    let body = vec![Item {
        span: Span::DUMMY,
        doc: Vec::new(),
        attributes: Vec::new(),
        kind: ItemKind::SpecInvocation(Box::new(option_si)),
    }];

    let augmented = m.with_sibling_renames(&body, &interner);
    let option_v_sym = interner.intern("Option_V");
    let binding = augmented
        .lookup(option_v_sym)
        .expect("Option_V rename was not registered");
    match &binding.value {
        Argument::Type(qname) => assert_eq!(qname.as_str(), "Option_f64"),
        other => panic!("expected Argument::Type(Option_f64), got {other:?}"),
    }
}

#[test]
fn effect_row_argument_rejected_as_unsupported() {
    let interner = Interner::new();
    let generics = vec![gp(&interner, "E", GenericKind::Comptime)];
    let args = ArgumentTuple::new(vec![Argument::EffectRow(EffectRowArgument::empty())]);
    let err = SubstitutionMap::bind("std.Foo", &generics, &args, &interner).unwrap_err();
    match err {
        CodegenError::MonomorphUnsupportedArgument {
            argument_kind_tag, ..
        } => {
            assert_eq!(argument_kind_tag, 0x02);
        }
        other => panic!("expected MonomorphUnsupportedArgument, got {other:?}"),
    }
}

#[test]
fn user_defined_argument_rejected_as_unsupported() {
    let interner = Interner::new();
    let generics = vec![gp(&interner, "V", GenericKind::Comptime)];
    let args = ArgumentTuple::new(vec![Argument::UserDefined(UserValue::Record {
        type_qualified: SmolStr::new("foo.Point"),
        fields: Box::new([]),
    })]);
    let err = SubstitutionMap::bind("std.Foo", &generics, &args, &interner).unwrap_err();
    match err {
        CodegenError::MonomorphUnsupportedArgument {
            argument_kind_tag, ..
        } => {
            assert_eq!(argument_kind_tag, 0x04);
        }
        other => panic!("expected MonomorphUnsupportedArgument, got {other:?}"),
    }
}

#[test]
fn successful_bind_carries_positions() {
    let interner = Interner::new();
    let generics = vec![
        gp(&interner, "T", GenericKind::Type),
        gp(&interner, "n", GenericKind::Comptime),
    ];
    let args = ArgumentTuple::new(vec![
        Argument::Type(SmolStr::new("std.option.Option")),
        Argument::Primitive(PrimitiveValue::U32(42)),
    ]);
    let m = SubstitutionMap::bind("std.Foo", &generics, &args, &interner).unwrap();
    assert_eq!(m.len(), 2);
    let t_sym = interner.intern("T");
    let n_sym = interner.intern("n");
    assert!(matches!(m.lookup(t_sym).unwrap().value, Argument::Type(_)));
    assert!(matches!(
        m.lookup(n_sym).unwrap().value,
        Argument::Primitive(_)
    ));
    // Unbound names return None.
    let unknown = interner.intern("U");
    assert!(m.lookup(unknown).is_none());
}
