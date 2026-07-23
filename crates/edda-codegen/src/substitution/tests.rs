//! Integration tests for [`super::substitute_spec_body`].
//!
//! Each test builds a small `Spec` AST by hand, binds a
//! [`SubstitutionMap`] (or supplies the empty one), runs the walker,
//! and inspects the rewritten `Block`'s AST shape directly. The tests
//! deliberately do not round-trip through the pretty-printer — that
//! wire is separate — they assert on AST equality.

use edda_intern::{Interner, Symbol};
use edda_span::Span;
use edda_syntax::IntBase;
use edda_syntax::ast::{
    Block, CallArg, Expr, ExprKind, FnBody, FnDecl, GenericKind, GenericParam, Ident, Item, ItemKind,
    Literal, Pat, PatKind, Path, Spec, Stmt, StmtKind, StructLitField, StructPatField, Type,
    TypeKind, UnOp, VariantPatPayload, Visibility,
};
use smol_str::SmolStr;

use crate::argument::{Argument, ArgumentTuple, PrimitiveValue};
use crate::substitution::{SubstitutionMap, substitute_spec_body};

// -------- AST builders --------

fn ident(interner: &Interner, name: &str) -> Ident {
    Ident {
        name: interner.intern(name),
        span: Span::DUMMY,
    }
}

fn path(interner: &Interner, segments: &[&str]) -> Path {
    Path {
        segments: segments.iter().map(|s| ident(interner, s)).collect(),
        span: Span::DUMMY,
    }
}

fn type_path(interner: &Interner, segments: &[&str]) -> Type {
    Type {
        span: Span::DUMMY,
        kind: TypeKind::Path(path(interner, segments)),
    }
}

fn expr_path(interner: &Interner, segments: &[&str]) -> Expr {
    Expr {
        span: Span::DUMMY,
        kind: ExprKind::Path(path(interner, segments)),
    }
}

fn empty_block() -> Block {
    Block {
        span: Span::DUMMY,
        stmts: vec![],
        trailing: None,
    }
}

fn block_trailing(expr: Expr) -> Block {
    Block {
        span: Span::DUMMY,
        stmts: vec![],
        trailing: Some(Box::new(expr)),
    }
}

//   by using a single carrier `function` whose body is the block; the
//   walker substitutes through the function body, so callers can keep
//   the existing block-based assertions
fn spec_with_generics(interner: &Interner, generics: Vec<GenericParam>, body: Block) -> Spec {
    let carrier = Item {
        span: Span::DUMMY,
        doc: vec![],
        attributes: vec![],
        kind: ItemKind::Function(Box::new(FnDecl {
            span: Span::DUMMY,
            stability: None,
            visibility: Visibility::Module,
            refinement_stable: false,
            name: ident(interner, "__carrier"),
            outbound_generics: vec![],
            generics: vec![],
            params: vec![],
            return_ty: None,
            return_mode: edda_syntax::ast::ReturnMode::ByValue,
            effects: None,
            refinements: vec![],
            body: FnBody::Block(body),
        })),
    };
    Spec {
        span: Span::DUMMY,
        stability: None,
        visibility: Visibility::Module,
        name: ident(interner, "Subject"),
        generics,
        where_clauses: vec![],
        body: vec![carrier],
    }
}

fn extract_carrier_block(items: Vec<Item>) -> Block {
    let item = items.into_iter().next().expect("single carrier item");
    let ItemKind::Function(fd) = item.kind else {
        panic!("expected single carrier function in spec body");
    };
    match fd.body {
        FnBody::Block(b) => b,
        FnBody::Extern { .. } => panic!("carrier function expected to be source-bodied"),
    }
}

fn subst_block(
    spec: &Spec,
    subst: &SubstitutionMap,
    interner: &Interner,
) -> Block {
    extract_carrier_block(substitute_spec_body(spec, subst, interner))
}

fn type_generic(interner: &Interner, name: &str) -> GenericParam {
    GenericParam {
        span: Span::DUMMY,
        kind: GenericKind::Type,
        name: ident(interner, name),
        ty: None,
        admits: vec![],
    }
}

fn comptime_generic(interner: &Interner, name: &str) -> GenericParam {
    GenericParam {
        span: Span::DUMMY,
        kind: GenericKind::Comptime,
        name: ident(interner, name),
        ty: None,
        admits: vec![],
    }
}

fn segment_names(interner: &Interner, p: &Path) -> Vec<String> {
    p.segments
        .iter()
        .map(|s| interner.resolve(s.name).to_string())
        .collect()
}

// -------- tests --------

#[test]
fn empty_subst_returns_structurally_equal_block() {
    let interner = Interner::new();
    let body = Block {
        span: Span::DUMMY,
        stmts: vec![Stmt { attributes: Vec::new(),
            span: Span::DUMMY,
            kind: StmtKind::Expr(Expr {
                span: Span::DUMMY,
                kind: ExprKind::Literal(Literal::Int {
                    value: 7,
                    base: IntBase::Dec,
                }),
            }),
        }],
        trailing: None,
    };
    let spec = spec_with_generics(&interner, vec![], body.clone());
    let out = subst_block(&spec, &SubstitutionMap::empty(), &interner);
    assert_eq!(out, body);
}

#[test]
fn type_generic_in_type_path_rewrites_to_qname_segments() {
    let interner = Interner::new();
    // body trailing expr: a Let with type annotation `T` — wrapped in
    // a trailing block so we have a value-producing Block.
    let let_stmt = Stmt { attributes: Vec::new(),
        span: Span::DUMMY,
        kind: StmtKind::Let {
            mutability: edda_syntax::ast::BindingMode::Immutable,
            pat: Pat {
                span: Span::DUMMY,
                kind: PatKind::Binding(ident(&interner, "x")),
            },
            ty: Some(type_path(&interner, &["T"])),
            init: None,
        },
    };
    let body = Block {
        span: Span::DUMMY,
        stmts: vec![let_stmt],
        trailing: None,
    };
    let spec = spec_with_generics(&interner, vec![type_generic(&interner, "T")], body);
    let args = ArgumentTuple::new(vec![Argument::Type(SmolStr::new("std.option.Option"))]);
    let subst =
        SubstitutionMap::bind("std.Foo", &spec.generics, &args, &interner).unwrap();
    let out = subst_block(&spec, &subst, &interner);
    let StmtKind::Let { ty: Some(rewritten), .. } = &out.stmts[0].kind else {
        panic!("expected Let with type annotation");
    };
    let TypeKind::Path(p) = &rewritten.kind else {
        panic!("expected Type::Path after substitution");
    };
    assert_eq!(
        segment_names(&interner, p),
        vec!["std".to_string(), "option".to_string(), "Option".to_string()],
    );
}

#[test]
fn type_generic_in_multi_segment_path_preserves_tail() {
    let interner = Interner::new();
    // body: `let x: T.Inner` — `T.Inner` rewrites to `std.option.Option.Inner`.
    let let_stmt = Stmt { attributes: Vec::new(),
        span: Span::DUMMY,
        kind: StmtKind::Let {
            mutability: edda_syntax::ast::BindingMode::Immutable,
            pat: Pat {
                span: Span::DUMMY,
                kind: PatKind::Binding(ident(&interner, "x")),
            },
            ty: Some(type_path(&interner, &["T", "Inner"])),
            init: None,
        },
    };
    let body = Block {
        span: Span::DUMMY,
        stmts: vec![let_stmt],
        trailing: None,
    };
    let spec = spec_with_generics(&interner, vec![type_generic(&interner, "T")], body);
    let args = ArgumentTuple::new(vec![Argument::Type(SmolStr::new("std.option.Option"))]);
    let subst =
        SubstitutionMap::bind("std.Foo", &spec.generics, &args, &interner).unwrap();
    let out = subst_block(&spec, &subst, &interner);
    let StmtKind::Let { ty: Some(rewritten), .. } = &out.stmts[0].kind else {
        panic!("expected Let with type annotation");
    };
    let TypeKind::Path(p) = &rewritten.kind else {
        panic!("expected Type::Path after substitution");
    };
    assert_eq!(
        segment_names(&interner, p),
        vec![
            "std".to_string(),
            "option".to_string(),
            "Option".to_string(),
            "Inner".to_string(),
        ],
    );
}

#[test]
fn type_generic_inside_tuple_and_slice_recurses() {
    let interner = Interner::new();
    // type: ([T], T)
    let inner = Type {
        span: Span::DUMMY,
        kind: TypeKind::Tuple(vec![
            Type {
                span: Span::DUMMY,
                kind: TypeKind::Slice(Box::new(type_path(&interner, &["T"]))),
            },
            type_path(&interner, &["T"]),
        ]),
    };
    let let_stmt = Stmt { attributes: Vec::new(),
        span: Span::DUMMY,
        kind: StmtKind::Let {
            mutability: edda_syntax::ast::BindingMode::Immutable,
            pat: Pat {
                span: Span::DUMMY,
                kind: PatKind::Binding(ident(&interner, "x")),
            },
            ty: Some(inner),
            init: None,
        },
    };
    let body = Block {
        span: Span::DUMMY,
        stmts: vec![let_stmt],
        trailing: None,
    };
    let spec = spec_with_generics(&interner, vec![type_generic(&interner, "T")], body);
    let args = ArgumentTuple::new(vec![Argument::Type(SmolStr::new("i32"))]);
    let subst =
        SubstitutionMap::bind("std.Foo", &spec.generics, &args, &interner).unwrap();
    let out = subst_block(&spec, &subst, &interner);
    let StmtKind::Let { ty: Some(t), .. } = &out.stmts[0].kind else {
        panic!("missing let type");
    };
    let TypeKind::Tuple(elems) = &t.kind else {
        panic!("expected tuple");
    };
    let TypeKind::Slice(inner) = &elems[0].kind else {
        panic!("expected slice in elem 0");
    };
    let TypeKind::Path(slice_inner) = &inner.kind else {
        panic!("expected path inside slice");
    };
    let TypeKind::Path(elem1) = &elems[1].kind else {
        panic!("expected path in elem 1");
    };
    assert_eq!(segment_names(&interner, slice_inner), vec!["i32".to_string()]);
    assert_eq!(segment_names(&interner, elem1), vec!["i32".to_string()]);
}

#[test]
fn comptime_primitive_in_expr_path_becomes_literal() {
    let interner = Interner::new();
    // body trailing: just `n` — should become `42`.
    let body = block_trailing(expr_path(&interner, &["n"]));
    let spec = spec_with_generics(&interner, vec![comptime_generic(&interner, "n")], body);
    let args = ArgumentTuple::new(vec![Argument::Primitive(PrimitiveValue::U32(42))]);
    let subst =
        SubstitutionMap::bind("std.Foo", &spec.generics, &args, &interner).unwrap();
    let out = subst_block(&spec, &subst, &interner);
    let trailing = out.trailing.expect("trailing");
    let ExprKind::Literal(Literal::Int { value, base }) = trailing.kind else {
        panic!("expected Literal::Int after substitution, got {:?}", trailing.kind);
    };
    assert_eq!(value, 42);
    assert_eq!(base, IntBase::Dec);
}

#[test]
fn comptime_bool_in_expr_path_becomes_bool_literal() {
    let interner = Interner::new();
    let body = block_trailing(expr_path(&interner, &["flag"]));
    let spec =
        spec_with_generics(&interner, vec![comptime_generic(&interner, "flag")], body);
    let args = ArgumentTuple::new(vec![Argument::Primitive(PrimitiveValue::Bool(true))]);
    let subst =
        SubstitutionMap::bind("std.Foo", &spec.generics, &args, &interner).unwrap();
    let out = subst_block(&spec, &subst, &interner);
    let trailing = out.trailing.expect("trailing");
    assert!(matches!(
        trailing.kind,
        ExprKind::Literal(Literal::Bool(true))
    ));
}

#[test]
fn comptime_negative_signed_int_wraps_in_unary_neg() {
    let interner = Interner::new();
    let body = block_trailing(expr_path(&interner, &["n"]));
    let spec = spec_with_generics(&interner, vec![comptime_generic(&interner, "n")], body);
    let args = ArgumentTuple::new(vec![Argument::Primitive(PrimitiveValue::I32(-7))]);
    let subst =
        SubstitutionMap::bind("std.Foo", &spec.generics, &args, &interner).unwrap();
    let out = subst_block(&spec, &subst, &interner);
    let trailing = out.trailing.expect("trailing");
    let ExprKind::Unary { op, expr } = trailing.kind else {
        panic!("expected Unary Neg, got {:?}", trailing.kind);
    };
    assert_eq!(op, UnOp::Neg);
    let ExprKind::Literal(Literal::Int { value, .. }) = expr.kind else {
        panic!("expected inner Literal::Int");
    };
    assert_eq!(value, 7);
}

#[test]
fn comptime_string_in_expr_path_becomes_str_literal() {
    let interner = Interner::new();
    let body = block_trailing(expr_path(&interner, &["s"]));
    let spec = spec_with_generics(&interner, vec![comptime_generic(&interner, "s")], body);
    let args = ArgumentTuple::new(vec![Argument::Primitive(PrimitiveValue::String(
        SmolStr::new("hello"),
    ))]);
    let subst =
        SubstitutionMap::bind("std.Foo", &spec.generics, &args, &interner).unwrap();
    let out = subst_block(&spec, &subst, &interner);
    let trailing = out.trailing.expect("trailing");
    let ExprKind::Literal(Literal::Str(sym)) = trailing.kind else {
        panic!("expected Literal::Str");
    };
    assert_eq!(interner.resolve(sym), "hello");
}

#[test]
fn type_generic_in_expr_path_rewrites_to_path_expr() {
    let interner = Interner::new();
    // body: `T` as an expression — e.g. passed to `size_of(T)`. We use
    // just `T` as the trailing expression. After substitution it
    // should be an `ExprKind::Path` with the qname's segments.
    let body = block_trailing(expr_path(&interner, &["T"]));
    let spec = spec_with_generics(&interner, vec![type_generic(&interner, "T")], body);
    let args = ArgumentTuple::new(vec![Argument::Type(SmolStr::new("std.fmt.Display"))]);
    let subst =
        SubstitutionMap::bind("std.Foo", &spec.generics, &args, &interner).unwrap();
    let out = subst_block(&spec, &subst, &interner);
    let trailing = out.trailing.expect("trailing");
    let ExprKind::Path(p) = trailing.kind else {
        panic!("expected Path expr");
    };
    assert_eq!(
        segment_names(&interner, &p),
        vec!["std".to_string(), "fmt".to_string(), "Display".to_string()],
    );
}

#[test]
fn unbound_expr_path_passes_through_unchanged() {
    let interner = Interner::new();
    let body = block_trailing(expr_path(&interner, &["unbound"]));
    let spec = spec_with_generics(&interner, vec![type_generic(&interner, "T")], body.clone());
    let args = ArgumentTuple::new(vec![Argument::Type(SmolStr::new("i32"))]);
    let subst =
        SubstitutionMap::bind("std.Foo", &spec.generics, &args, &interner).unwrap();
    let out = subst_block(&spec, &subst, &interner);
    assert_eq!(out, body);
}

#[test]
fn multi_segment_expr_path_with_comptime_head_is_not_substituted() {
    let interner = Interner::new();
    // `n.field` — multi-segment, head is a Comptime generic. The walker
    // leaves this alone because a comptime *value* doesn't admit
    // module-style projection at the Path level (field access is a
    // separate AST node).
    let body = block_trailing(expr_path(&interner, &["n", "field"]));
    let spec = spec_with_generics(&interner, vec![comptime_generic(&interner, "n")], body.clone());
    let args = ArgumentTuple::new(vec![Argument::Primitive(PrimitiveValue::U32(5))]);
    let subst =
        SubstitutionMap::bind("std.Foo", &spec.generics, &args, &interner).unwrap();
    let out = subst_block(&spec, &subst, &interner);
    assert_eq!(out, body);
}

#[test]
fn struct_lit_path_head_rewrites_with_type_generic() {
    let interner = Interner::new();
    // body: `T { x: 1 }` — StructLit. Path head `T` rewrites.
    let lit = Expr {
        span: Span::DUMMY,
        kind: ExprKind::StructLit {
            path: path(&interner, &["T"]),
            fields: vec![StructLitField {
                span: Span::DUMMY,
                name: ident(&interner, "x"),
                mode: None,
                value: Expr {
                    span: Span::DUMMY,
                    kind: ExprKind::Literal(Literal::Int {
                        value: 1,
                        base: IntBase::Dec,
                    }),
                },
            }],
        },
    };
    let body = block_trailing(lit);
    let spec = spec_with_generics(&interner, vec![type_generic(&interner, "T")], body);
    let args = ArgumentTuple::new(vec![Argument::Type(SmolStr::new("foo.Point"))]);
    let subst =
        SubstitutionMap::bind("std.Foo", &spec.generics, &args, &interner).unwrap();
    let out = subst_block(&spec, &subst, &interner);
    let trailing = out.trailing.expect("trailing");
    let ExprKind::StructLit { path: p, fields } = &trailing.kind else {
        panic!("expected StructLit");
    };
    assert_eq!(
        segment_names(&interner, p),
        vec!["foo".to_string(), "Point".to_string()],
    );
    // Field value (1) unchanged.
    assert!(matches!(
        fields[0].value.kind,
        ExprKind::Literal(Literal::Int { value: 1, .. })
    ));
}

#[test]
fn variant_pattern_path_head_rewrites_with_type_generic() {
    let interner = Interner::new();
    // body: `match v { T.red => 0 }` — variant pattern path head.
    let arm = edda_syntax::ast::MatchArm {
        span: Span::DUMMY,
        pat: Pat {
            span: Span::DUMMY,
            kind: PatKind::Variant {
                path: path(&interner, &["T", "red"]),
                payload: VariantPatPayload::None,
            },
        },
        guard: None,
        body: Expr {
            span: Span::DUMMY,
            kind: ExprKind::Literal(Literal::Int {
                value: 0,
                base: IntBase::Dec,
            }),
        },
    };
    let scrutinee = expr_path(&interner, &["v"]);
    let match_expr = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Match {
            scrutinee: Box::new(scrutinee),
            arms: vec![arm],
        },
    };
    let body = block_trailing(match_expr);
    let spec = spec_with_generics(&interner, vec![type_generic(&interner, "T")], body);
    let args = ArgumentTuple::new(vec![Argument::Type(SmolStr::new("foo.Color"))]);
    let subst =
        SubstitutionMap::bind("std.Foo", &spec.generics, &args, &interner).unwrap();
    let out = subst_block(&spec, &subst, &interner);
    let trailing = out.trailing.expect("trailing");
    let ExprKind::Match { arms, .. } = &trailing.kind else {
        panic!("expected Match");
    };
    let PatKind::Variant { path: p, .. } = &arms[0].pat.kind else {
        panic!("expected Variant pattern");
    };
    assert_eq!(
        segment_names(&interner, p),
        vec!["foo".to_string(), "Color".to_string(), "red".to_string()],
    );
}

#[test]
fn struct_pattern_path_head_rewrites_with_type_generic() {
    let interner = Interner::new();
    // body: `match v { T { x } => x }` — struct pattern path head.
    let arm = edda_syntax::ast::MatchArm {
        span: Span::DUMMY,
        pat: Pat {
            span: Span::DUMMY,
            kind: PatKind::Struct {
                path: path(&interner, &["T"]),
                fields: vec![StructPatField {
                    span: Span::DUMMY,
                    name: ident(&interner, "x"),
                    pat: Pat {
                        span: Span::DUMMY,
                        kind: PatKind::Binding(ident(&interner, "x")),
                    },
                }],
                rest: false,
            },
        },
        guard: None,
        body: expr_path(&interner, &["x"]),
    };
    let match_expr = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Match {
            scrutinee: Box::new(expr_path(&interner, &["v"])),
            arms: vec![arm],
        },
    };
    let body = block_trailing(match_expr);
    let spec = spec_with_generics(&interner, vec![type_generic(&interner, "T")], body);
    let args = ArgumentTuple::new(vec![Argument::Type(SmolStr::new("foo.Point"))]);
    let subst =
        SubstitutionMap::bind("std.Foo", &spec.generics, &args, &interner).unwrap();
    let out = subst_block(&spec, &subst, &interner);
    let trailing = out.trailing.expect("trailing");
    let ExprKind::Match { arms, .. } = &trailing.kind else {
        panic!("expected Match");
    };
    let PatKind::Struct { path: p, .. } = &arms[0].pat.kind else {
        panic!("expected Struct pattern");
    };
    assert_eq!(
        segment_names(&interner, p),
        vec!["foo".to_string(), "Point".to_string()],
    );
}

#[test]
fn comptime_substitution_recurses_into_call_arguments() {
    let interner = Interner::new();
    // body: `size_of(T, n)` — Call with two Path arguments. T → "i32",
    // n → 42.
    let call = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Call {
            callee: Box::new(expr_path(&interner, &["size_of"])),
            args: vec![
                CallArg::bare(expr_path(&interner, &["T"])),
                CallArg::bare(expr_path(&interner, &["n"])),
            ],
        },
    };
    let body = block_trailing(call);
    let spec = spec_with_generics(
        &interner,
        vec![
            type_generic(&interner, "T"),
            comptime_generic(&interner, "n"),
        ],
        body,
    );
    let args = ArgumentTuple::new(vec![
        Argument::Type(SmolStr::new("i32")),
        Argument::Primitive(PrimitiveValue::U32(42)),
    ]);
    let subst =
        SubstitutionMap::bind("std.Foo", &spec.generics, &args, &interner).unwrap();
    let out = subst_block(&spec, &subst, &interner);
    let trailing = out.trailing.expect("trailing");
    let ExprKind::Call { args, .. } = &trailing.kind else {
        panic!("expected Call");
    };
    let ExprKind::Path(t_path) = &args[0].expr.kind else {
        panic!("expected Path for first arg");
    };
    assert_eq!(segment_names(&interner, t_path), vec!["i32".to_string()]);
    let ExprKind::Literal(Literal::Int { value, .. }) = args[1].expr.kind else {
        panic!("expected Literal::Int for second arg");
    };
    assert_eq!(value, 42);
}

#[test]
fn signed_i64_min_wraps_in_neg_around_unsigned_abs() {
    let interner = Interner::new();
    let body = block_trailing(expr_path(&interner, &["n"]));
    let spec = spec_with_generics(&interner, vec![comptime_generic(&interner, "n")], body);
    let args =
        ArgumentTuple::new(vec![Argument::Primitive(PrimitiveValue::I64(i64::MIN))]);
    let subst =
        SubstitutionMap::bind("std.Foo", &spec.generics, &args, &interner).unwrap();
    let out = subst_block(&spec, &subst, &interner);
    let trailing = out.trailing.expect("trailing");
    let ExprKind::Unary { op, expr } = trailing.kind else {
        panic!("expected Unary");
    };
    assert_eq!(op, UnOp::Neg);
    let ExprKind::Literal(Literal::Int { value, .. }) = expr.kind else {
        panic!("expected inner Literal::Int");
    };
    assert_eq!(value, (i64::MIN as i128).unsigned_abs());
}

#[test]
fn spread_effect_member_is_left_untouched() {
    let interner = Interner::new();
    // body: `let f: function() with { ...T };` — `...T` Spread is left
    // unchanged because bind rejects EffectRow arguments, so
    // even if a Type-kind `T` is bound the walker treats the path's
    // Spread context structurally (the test asserts no rewrite).
    let fn_type = Type {
        span: Span::DUMMY,
        kind: TypeKind::Function {
            params: vec![],
            ret: Box::new(Type {
                span: Span::DUMMY,
                kind: TypeKind::Unit,
            }),
            effects: Some(edda_syntax::ast::EffectRow {
                span: Span::DUMMY,
                members: vec![edda_syntax::ast::EffectMember::Spread(path(
                    &interner,
                    &["T"],
                ))],
            }),
        },
    };
    let let_stmt = Stmt { attributes: Vec::new(),
        span: Span::DUMMY,
        kind: StmtKind::Let {
            mutability: edda_syntax::ast::BindingMode::Immutable,
            pat: Pat {
                span: Span::DUMMY,
                kind: PatKind::Binding(ident(&interner, "f")),
            },
            ty: Some(fn_type),
            init: None,
        },
    };
    let body = Block {
        span: Span::DUMMY,
        stmts: vec![let_stmt],
        trailing: None,
    };
    // A Type-kind generic T bound to a real type — this is a degenerate
    // case (T is a type, not a row), but the walker passes Spread paths
    // through, so the output must contain the original `...T`.
    let spec = spec_with_generics(&interner, vec![type_generic(&interner, "T")], body);
    let args = ArgumentTuple::new(vec![Argument::Type(SmolStr::new("foo.Errs"))]);
    let subst =
        SubstitutionMap::bind("std.Foo", &spec.generics, &args, &interner).unwrap();
    let out = subst_block(&spec, &subst, &interner);
    let StmtKind::Let { ty: Some(t), .. } = &out.stmts[0].kind else {
        panic!("expected let with type");
    };
    let TypeKind::Function { effects: Some(r), .. } = &t.kind else {
        panic!("expected function type with effects");
    };
    let edda_syntax::ast::EffectMember::Spread(p) = &r.members[0] else {
        panic!("expected Spread member");
    };
    // Spread path's head is still `T`, not rewritten.
    let head: Symbol = p.segments[0].name;
    assert_eq!(interner.resolve(head), "T");
}
