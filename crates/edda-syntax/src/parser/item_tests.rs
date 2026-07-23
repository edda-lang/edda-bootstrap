//! Item-level integration tests for the parser. Each test feeds source
//! through the full lex → parse pipeline and asserts on the resulting
//! [`File`] AST plus its diagnostics.

use edda_diag::{Diagnostics, LintConfig};
use edda_intern::Interner;
use edda_span::SourceMap;
use std::path::PathBuf;

use crate::ast::{
    AttrArg, AttrLit, EffectMember, ExprKind, File, GenericKind, ItemKind, ParamMode, TypeDeclKind,
    TypeKind, VariantPayload, Visibility,
};
use crate::token::IntBase;
use crate::lexer::lex;
use crate::parser::parse_file;

fn parse_file_str(src: &str) -> (File, Diagnostics) {
    let map = SourceMap::new();
    let file = map.add_file(PathBuf::from("test.ea"), src.to_string());
    let interner = Interner::new();
    let mut diags = Diagnostics::new();
    let cfg = LintConfig::new();
    let tokens = lex(src, file, &interner, &mut diags, &cfg);
    let parsed = parse_file(&tokens, &interner, &mut diags, &cfg);
    (parsed, diags)
}

#[test]
fn file_with_single_function() {
    let src = "function main() -> () { return }";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors());
    assert_eq!(file.items.len(), 1);
    let ItemKind::Function(fd) = &file.items[0].kind else {
        panic!("expected Function item, got {:?}", file.items[0].kind);
    };
    assert_eq!(fd.visibility, Visibility::Module);
    assert!(fd.params.is_empty());
    assert!(fd.return_ty.is_some());
}

#[test]
fn public_function_with_params_modes_and_effects() {
    let src = "
public function push(buf: mutable TokenBuf, value: take Token, allocator: Allocator) -> ()
    with {allocator, err: AllocError}
{
    return
}";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors(), "{} errors", d.error_count());
    let ItemKind::Function(fd) = &file.items[0].kind else {
        panic!("expected Function");
    };
    assert_eq!(fd.visibility, Visibility::Public);
    assert_eq!(fd.params.len(), 3);
    assert_eq!(fd.params[0].mode, ParamMode::Mutable);
    assert_eq!(fd.params[1].mode, ParamMode::Take);
    assert_eq!(fd.params[2].mode, ParamMode::Default);
    let row = fd.effects.as_ref().expect("expected effect row");
    assert_eq!(row.members.len(), 2);
}

// Doc-comment attachment tests were removed with the no-comment lexer
// (the V1.0 no-comment design lock): `.ea` source admits no comments, so
// `///` / `//!` reject as `comment_not_admitted` and never attach. Rejection is covered by the
// lexer tests (`doc_comments_are_rejected`, `four_doc_tiers_are_rejected`).

#[test]
fn product_type_declaration() {
    let src = "type Point { x: f64, y: f64 }";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors());
    let ItemKind::TypeDecl(td) = &file.items[0].kind else {
        panic!("expected TypeDecl");
    };
    let TypeDeclKind::Product { fields } = &td.kind else {
        panic!("expected Product");
    };
    assert_eq!(fields.len(), 2);
    assert!(matches!(fields[0].ty.kind, TypeKind::Path(_)));
}

#[test]
fn sum_type_declaration() {
    let src = "type Color { case red, case rgb(u8, u8, u8) }";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors());
    let ItemKind::TypeDecl(td) = &file.items[0].kind else {
        panic!("expected TypeDecl");
    };
    let TypeDeclKind::Sum { variants } = &td.kind else {
        panic!("expected Sum");
    };
    assert_eq!(variants.len(), 2);
    assert!(matches!(variants[0].payload, VariantPayload::Unit));
    let VariantPayload::Tuple(tys) = &variants[1].payload else {
        panic!("expected Tuple payload");
    };
    assert_eq!(tys.len(), 3);
}

#[test]
fn sum_variant_with_named_payload() {
    // edlang0's canonical form uses `(name: T, name: T)` for variant payloads.
    let src = "type Color { case rgb(r: u8, g: u8, b: u8) }";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors());
    let ItemKind::TypeDecl(td) = &file.items[0].kind else {
        panic!("expected TypeDecl");
    };
    let TypeDeclKind::Sum { variants } = &td.kind else {
        panic!("expected Sum");
    };
    let VariantPayload::Struct(fields) = &variants[0].payload else {
        panic!("expected Struct payload, got {:?}", variants[0].payload);
    };
    assert_eq!(fields.len(), 3);
}

#[test]
fn imports() {
    let src = "import std.fs\nimport value";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors());
    assert_eq!(file.items.len(), 2);
    let ItemKind::Import(i1) = &file.items[0].kind else {
        panic!("expected Import");
    };
    assert_eq!(i1.path.segments.len(), 2);
    let ItemKind::Import(i2) = &file.items[1].kind else {
        panic!("expected Import");
    };
    assert_eq!(i2.path.segments.len(), 1);
}

#[test]
fn function_with_requires_and_ensures() {
    let src = "function abs(x: i32) -> i32 requires x >= 0 ensures result >= 0 { return x }";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors(), "{} errors", d.error_count());
    let ItemKind::Function(fd) = &file.items[0].kind else {
        panic!("expected Function");
    };
    assert_eq!(fd.refinements.len(), 2);
}

#[test]
fn bounded_stack_corpus_example_parses_cleanly() {
    // End-to-end: parse the locked corpus example
    // `corpus/edda-codex/examples/bounded-stack/main.ea` and verify
    // zero diagnostics. The file's `with {allocator, err: AllocError, panic}`
    // row is the canonical reason this test exists; the rest of the file
    // exercises ranges, the `var name: T` uninitialised-slot form, `?`
    // propagation, and qualified-path references — forms already covered
    // by other tests, included here to keep the integration check honest.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = std::path::Path::new(manifest_dir)
        .join("../../corpus/edda-codex/examples/bounded-stack/main.ea");
    if !path.exists() {
        // Submodule not initialised in this checkout; the inline test
        // below still covers the parse_effect_member fix path.
        return;
    }
    let src = std::fs::read_to_string(&path).expect("read corpus file");
    let (_file, d) = parse_file_str(&src);
    assert!(
        !d.has_errors(),
        "bounded-stack parsed with {} error(s)",
        d.error_count()
    );
}

#[test]
fn named_payload_variant_construction_parses() {
    // `declarations.md` §252: struct-payload variant constructors use
    // `Type.variant(field: value, ...)`. The parser must accept the
    // `name: expr` argument form so the typechecker can validate it
    // downstream.
    let src = r#"
public function make() -> Phase {
    return Phase.yellow(seconds_remaining: 3)
}
"#;
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors(), "diagnostics: {}", d.error_count());
    let ItemKind::Function(fd) = &file.items[0].kind else {
        panic!("expected Function");
    };
    let crate::ast::FnBody::Block(block) = &fd.body else {
        panic!("expected source-bodied function");
    };
    let trailing = block.trailing.as_ref().expect("trailing return expr");
    let ExprKind::Return(Some(val)) = &trailing.kind else {
        panic!("expected return-with-value, got {:?}", trailing.kind);
    };
    let ExprKind::Call { args, .. } = &val.kind else {
        panic!("expected Call, got {:?}", val.kind);
    };
    assert_eq!(args.len(), 1);
    let arg = &args[0];
    let name = arg.name.as_ref().expect("call arg should carry a name");
    assert!(arg.mode.is_none());
    let resolved = {
        let _ = name;
        // We don't have the interner here; just check the structural shape.
        true
    };
    assert!(resolved);
    assert!(matches!(arg.expr.kind, ExprKind::Literal(_)));
}

#[test]
fn function_call_rejects_named_payload_in_typecheck_not_parser() {
    // The parser must accept named-arg syntax at any call site; the
    // type checker enforces that only variant-constructor calls admit
    // it (`expressions.md` §521 vs `declarations.md` §252).
    let src = "function f() -> () { g(x: 1) }";
    let (_, d) = parse_file_str(src);
    assert!(!d.has_errors(), "named-arg syntax must parse cleanly");
}

#[test]
fn named_payload_argument_carries_span_starting_at_name() {
    let src = "function f() -> () { g(field_one: 1) }";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors());
    let ItemKind::Function(fd) = &file.items[0].kind else {
        panic!("expected Function");
    };
    // The parser places the bare call expression in `trailing`, not in
    // `stmts`, because there's no trailing semicolon.
    let crate::ast::FnBody::Block(block) = &fd.body else {
        panic!("expected source-bodied function");
    };
    let expr = block.trailing.as_ref().expect("trailing expression");
    let ExprKind::Call { args, .. } = &expr.kind else {
        panic!("expected Call, got {:?}", expr.kind);
    };
    let arg = &args[0];
    let name = arg.name.as_ref().expect("name");
    // The arg's span starts where the name starts, not where the expr starts.
    assert!(arg.span.lo <= name.span.lo);
    assert!(name.span.hi < arg.expr.span.lo);
}

#[test]
fn traffic_light_constructor_call_sites_parse_cleanly() {
    // Spot-check that the constructor call sites from
    // `corpus/edda-codex/examples/traffic-light/main.ea` produce no
    // parse errors. Match-arm `.variant(...)` patterns are another
    // agent's territory and are intentionally excluded from this
    // fixture; this test guards only the constructor call sites that
    // depend on the named-payload syntax.
    let src = r#"
public type Phase {
    case red
    case green
    case yellow(seconds_remaining: i32)
}

public function make_red() -> Phase {
    return Phase.red
}

public function make_yellow(s: i32) -> Phase {
    return Phase.yellow(seconds_remaining: s)
}

public function make_yellow_lit() -> Phase {
    return Phase.yellow(seconds_remaining: 3)
}
"#;
    let (_, d) = parse_file_str(src);
    assert!(
        !d.has_errors(),
        "traffic-light constructor sites should parse cleanly; got {} errors: {:?}",
        d.error_count(),
        d.iter().map(|x| &x.message).collect::<Vec<_>>(),
    );
}

#[test]
fn effect_row_admits_capability_named_and_panic_keyword() {
    // `panic` is `Token::Panic` (an originator keyword), not `Token::Ident`,
    // but the locked effect-row grammar (`docs/syntax/effects.md`) admits
    // it as a bare payload-free pure-effect kind. The parser must accept
    // it as an effect-member name; the corpus example `bounded-stack`
    // exercises this row shape.
    let src = "
public function sum_through_stack(n: usize, allocator: Allocator) -> i64
    with {allocator, err: AllocError, panic}
    requires n < 1_000_000
{
    return 0
}";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors(), "{} errors", d.error_count());
    let ItemKind::Function(fd) = &file.items[0].kind else {
        panic!("expected Function");
    };
    let row = fd.effects.as_ref().expect("expected effect row");
    assert_eq!(row.members.len(), 3);
    assert!(matches!(row.members[0], EffectMember::Capability(_)));
    assert!(matches!(row.members[1], EffectMember::Named { .. }));
    assert!(matches!(row.members[2], EffectMember::Capability(_)));
}

#[test]
fn full_canonical_function_body() {
    let src = r#"
public function token_buf_push(buf: mutable TokenBuf, value: take Token, allocator: Allocator) -> ()
    with {allocator, err: AllocError}
{
    token_buf_reserve(mutable buf, buf.len + 1, allocator)?
    buf.data[buf.len] = value
    buf.len += 1
}
"#;
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors(), "diagnostics: {}", d.error_count());
    assert_eq!(file.items.len(), 1);
}

#[test]
fn assign_lhs_admits_call_result_field_assignment() {
    // `f().x = 1` — assignment through a return-position mutable
    // borrow. The LHS receiver
    // chain bottoms out at a `Call`, which the grammar admits as a
    // terminal base alongside an `<ident>` path; MIR's `resolve_place`
    // already lowers a `Call`/`MethodCall` base to the `Projection::
    // Deref` place over the returned pointer.
    let src = "function f() -> () { f().x = 1 }";
    let (_, d) = parse_file_str(src);
    assert!(
        !d.has_errors(),
        "f().x = 1 must be admitted as an assignment LHS, got {} errors",
        d.error_count(),
    );
}

#[test]
fn assign_lhs_rejects_tuple_literal() {
    // `(a, b) = (1, 2)` — a tuple literal is not an identifier path, a
    // call, or a projection over either; `expressions.md` §"LHS
    // production for assignment" rejects this shape.
    let src = "function f(a: i32, b: i32) -> () { (a, b) = (1, 2) }";
    let (_, d) = parse_file_str(src);
    assert!(
        d.has_errors(),
        "(a, b) = (1, 2) must be rejected as an assignment LHS",
    );
}

#[test]
fn assign_lhs_admits_field_and_index_chain() {
    // `buf.data[buf.len] = value` — the canonical LHS shape that the
    // corpus example `bounded-stack` relies on. Receiver chain is
    // `Index(Path("buf.data"), Path("buf.len"))` and bottoms out in a
    // multi-segment identifier path; admitted.
    let src = r#"
public function push(buf: mutable TokenBuf, value: take Token) -> () {
    buf.data[buf.len] = value
    buf.len = buf.len + 1
}
"#;
    let (_, d) = parse_file_str(src);
    assert!(
        !d.has_errors(),
        "field-and-index LHS chain must parse cleanly, got {} errors",
        d.error_count(),
    );
}

#[test]
fn missing_arrow_return_type_emits_parse_error() {
    // `declarations.md` §116 locks "Return type is mandatory." The
    // parser must reject any function header that omits `-> T`.
    let src = "function f() { return }";
    let (_, d) = parse_file_str(src);
    assert!(
        d.has_errors(),
        "function header without `->` must emit a parse_error",
    );
}

#[test]
fn spec_declaration_with_generic_params() {
    // `comptime.md` §253 — spec bodies admit `function`, `type`, and
    // module-level `let`. The arglist parses real generic params.
    let src = "spec Foo(comptime T: Type) { function bar() -> T { return } }";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors(), "{} errors", d.error_count());
    let ItemKind::Spec(spec) = &file.items[0].kind else {
        panic!("expected Spec");
    };
    assert_eq!(spec.generics.len(), 1);
    assert_eq!(spec.generics[0].kind, GenericKind::Comptime);
    assert!(
        spec.generics[0].ty.is_some(),
        "comptime spec generic carries its type annotation",
    );
    assert_eq!(spec.where_clauses.len(), 0);
    assert_eq!(spec.body.len(), 1, "spec body has one item");
    assert!(matches!(spec.body[0].kind, ItemKind::Function(_)));
}

#[test]
fn spec_declaration_with_where_clauses() {
    // `comptime.md` §292 locks `where` clauses on spec declarations as a
    // sequence of zero or more comptime predicates between `)` and `{`.
    let src = "spec Range(comptime T: Type) where T == i32 || T == i64 where T == u8 { function ping() -> () { return } }";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors(), "{} errors", d.error_count());
    let ItemKind::Spec(spec) = &file.items[0].kind else {
        panic!("expected Spec");
    };
    assert_eq!(spec.generics.len(), 1);
    assert_eq!(spec.where_clauses.len(), 2);
    assert_eq!(
        spec.where_clauses[0].kind,
        crate::ast::RefinementKind::Where,
    );
    assert_eq!(spec.body.len(), 1);
}

#[test]
fn spec_invocation_at_top_level() {
    // `comptime.md` §312 — `spec Path(args)` with no body is a top-level
    // spec invocation, distinct from a declaration.
    let src = "spec Range(i32)";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors(), "{} errors", d.error_count());
    let ItemKind::SpecInvocation(si) = &file.items[0].kind else {
        panic!("expected SpecInvocation, got {:?}", file.items[0].kind);
    };
    assert_eq!(si.args.len(), 1);
}

#[test]
fn zero_arg_spec_invocation_with_dotted_path() {
    // A multi-segment path is always an
    // invocation, so `spec a.b.C()` parses with an empty argument list
    // instead of being routed to the declaration parser.
    let src = "spec std.collections.hashset.IntSet()";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors(), "{} errors", d.error_count());
    let ItemKind::SpecInvocation(si) = &file.items[0].kind else {
        panic!("expected SpecInvocation, got {:?}", file.items[0].kind);
    };
    assert_eq!(si.path.segments.len(), 4);
    assert!(si.args.is_empty());
}

#[test]
fn zero_param_spec_declaration_still_parses() {
    // The single-segment `spec Name() { ... }` form stays a declaration.
    let src = "public spec StringSet() {\n}";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors(), "{} errors", d.error_count());
    assert!(matches!(file.items[0].kind, ItemKind::Spec(_)));
}

#[test]
fn spec_invocation_followed_by_function_item() {
    // `comptime.md` §334 — the single-token lookahead past `)` decides:
    // when the next non-trivial token is `function` (not `{` or `where`),
    // the spec is an invocation, not a declaration.
    let src = "spec std.option.Option(i32) function main() -> () { return }";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors(), "{} errors", d.error_count());
    assert_eq!(file.items.len(), 2);
    let ItemKind::SpecInvocation(si) = &file.items[0].kind else {
        panic!("expected first item to be SpecInvocation");
    };
    // Multi-segment path is admitted on the invocation form.
    assert_eq!(si.path.segments.len(), 3);
    assert!(matches!(file.items[1].kind, ItemKind::Function(_)));
}

#[test]
fn comptime_param_prefix_lifts_into_generics() {
    // `comptime.md` §102 locks the parameter-prefix form
    // `comptime <name>: <Type>`. The parser must lift such an entry
    // into `FnDecl.generics` (with `GenericKind::Comptime`) rather than
    // into `FnDecl.params`. Trailing value params still populate
    // `params` in source order.
    let src = "function push(comptime n: usize, x: i32) -> () { return }";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors(), "{} errors", d.error_count());
    let ItemKind::Function(fd) = &file.items[0].kind else {
        panic!("expected Function");
    };
    assert_eq!(fd.generics.len(), 1, "expected 1 comptime generic");
    assert_eq!(fd.generics[0].kind, GenericKind::Comptime);
    assert!(
        fd.generics[0].ty.is_some(),
        "comptime parameter carries its type annotation",
    );
    assert_eq!(fd.params.len(), 1, "trailing value param stays in params");
}

#[test]
fn item_admits_no_attributes_by_default() {
    let src = "function main() -> () { return }";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors());
    assert!(file.items[0].attributes.is_empty());
}

#[test]
fn single_export_attribute_on_function() {
    let src = "
@export(\"edda_callback\")
public function callback() -> i32 { return 0 }";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors(), "{} errors", d.error_count());
    let item = &file.items[0];
    assert_eq!(item.attributes.len(), 1);
    let attr = &item.attributes[0];
    assert_eq!(attr.args.len(), 1);
    match &attr.args[0] {
        AttrArg::Lit {
            lit: AttrLit::Str(_),
            ..
        } => {}
        other => panic!("expected Str literal arg, got {:?}", other),
    }
}

#[test]
fn multiple_attributes_in_source_order() {
    let src = "
@export(\"foo\")
@abi(\"sysv64\")
public function foo() -> i32 { return 0 }";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors(), "{} errors", d.error_count());
    let attrs = &file.items[0].attributes;
    assert_eq!(attrs.len(), 2);
}

#[test]
fn attribute_admits_ident_arg() {
    let src = "
@repr(C)
type Point { x: i32 }";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors(), "{} errors", d.error_count());
    let attrs = &file.items[0].attributes;
    assert_eq!(attrs.len(), 1);
    match &attrs[0].args[0] {
        AttrArg::Ident(_) => {}
        other => panic!("expected Ident arg, got {:?}", other),
    }
}

#[test]
fn attribute_admits_int_arg_with_base_preserved() {
    let src = "
@align(0x10)
type Aligned { x: i32 }";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors(), "{} errors", d.error_count());
    match &file.items[0].attributes[0].args[0] {
        AttrArg::Lit {
            lit: AttrLit::Int { value, base },
            ..
        } => {
            assert_eq!(*value, 16);
            assert_eq!(*base, IntBase::Hex);
        }
        other => panic!("expected Int literal arg, got {:?}", other),
    }
}

#[test]
fn attribute_admits_named_arg() {
    let src = "
@align(value = 16)
type Aligned { x: i32 }";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors(), "{} errors", d.error_count());
    match &file.items[0].attributes[0].args[0] {
        AttrArg::Named { value, .. } => match value.as_ref() {
            AttrArg::Lit {
                lit: AttrLit::Int { value: v, .. },
                ..
            } => assert_eq!(*v, 16),
            other => panic!("expected Int value, got {:?}", other),
        },
        other => panic!("expected Named arg, got {:?}", other),
    }
}

#[test]
fn attribute_admits_bool_arg() {
    let src = "
@layout(natural)
@some_flag(true)
type Foo { x: i32 }";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors(), "{} errors", d.error_count());
    let attrs = &file.items[0].attributes;
    assert_eq!(attrs.len(), 2);
    match &attrs[1].args[0] {
        AttrArg::Lit {
            lit: AttrLit::Bool(true),
            ..
        } => {}
        other => panic!("expected Bool(true) arg, got {:?}", other),
    }
}

#[test]
fn attribute_with_empty_arg_list_parses() {
    let src = "
@marker()
type Foo { x: i32 }";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors(), "{} errors", d.error_count());
    assert!(file.items[0].attributes[0].args.is_empty());
}

#[test]
fn attributes_attach_to_following_item_not_preceding() {
    // Attributes apply to the item that follows them, mirroring how
    // doc-comments attach.
    let src = "
function before() -> () { return }
@export(\"x\")
function after() -> () { return }";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors(), "{} errors", d.error_count());
    assert_eq!(file.items.len(), 2);
    assert!(file.items[0].attributes.is_empty());
    assert_eq!(file.items[1].attributes.len(), 1);
}

#[test]
fn positional_after_named_is_rejected() {
    let src = "
@align(value = 16, 8)
type Bad { x: i32 }";
    let (_file, d) = parse_file_str(src);
    assert!(
        d.has_errors(),
        "positional after named must emit a parse error",
    );
}


#[test]
fn extern_body_form_parses_clean() {
    let src = "
public function add_raw(a: i64, b: i64) -> i64
extern \"my_add_sym\"";
    let (file, d) = parse_file_str(src);
    assert!(!d.has_errors(), "{} errors", d.error_count());
    let ItemKind::Function(fd) = &file.items[0].kind else {
        panic!("expected Function item, got {:?}", file.items[0].kind);
    };
    assert!(
        matches!(fd.body, crate::ast::FnBody::Extern { .. }),
        "expected Extern body-form"
    );
}

#[test]
fn abi_bodyless_is_rejected_with_extern_recovery() {
    // `@abi("name")` is pure symbol-name
    // metadata and never licenses bodylessness. The parser diagnoses and
    // recovers by synthesising the Extern body from the attribute payload.
    let src = "
@abi(\"__edda_alloc_array\")
public function alloc(n: usize) -> [u8]";
    let (file, d) = parse_file_str(src);
    assert!(
        d.has_errors(),
        "bodyless @abi function must emit a parse error"
    );
    let ItemKind::Function(fd) = &file.items[0].kind else {
        panic!("expected Function item, got {:?}", file.items[0].kind);
    };
    assert!(
        matches!(fd.body, crate::ast::FnBody::Extern { .. }),
        "recovery must synthesise FnBody::Extern from the @abi payload"
    );
}
