//! Round-trip tests: parse → print → parse → print, with the two printed
//! strings required to match exactly. This verifies the locked rule from
//! `docs/tooling/structural-edits.md` without committing to a particular
//! surface format.

use edda_diag::{Diagnostics, LintConfig};
use edda_intern::Interner;
use edda_span::SourceMap;
use std::path::PathBuf;

use crate::lexer::lex;
use crate::parser::{parse_expr, parse_file};
use crate::printer::{print_file, Printer};

fn roundtrip_file(src: &str) -> (String, String) {
    let map = SourceMap::new();
    let file_id = map.add_file(PathBuf::from("test.ea"), src.to_string());
    let interner = Interner::new();
    let mut diags = Diagnostics::new();
    let cfg = LintConfig::new();
    let tokens = lex(src, file_id, &interner, &mut diags, &cfg);
    let ast1 = parse_file(&tokens, &interner, &mut diags, &cfg);
    assert!(!diags.has_errors(), "first parse failed: {} errors", diags.error_count());
    let printed1 = print_file(&ast1, &interner);

    let mut diags2 = Diagnostics::new();
    let file_id2 = map.add_file(PathBuf::from("test2.ea"), printed1.clone());
    let tokens2 = lex(&printed1, file_id2, &interner, &mut diags2, &cfg);
    let ast2 = parse_file(&tokens2, &interner, &mut diags2, &cfg);
    assert!(
        !diags2.has_errors(),
        "second parse failed on printer output:\n{}\nerrors: {}",
        printed1,
        diags2.error_count()
    );
    let printed2 = print_file(&ast2, &interner);
    (printed1, printed2)
}

fn assert_roundtrip(src: &str) -> String {
    let (printed1, printed2) = roundtrip_file(src);
    assert_eq!(
        printed1, printed2,
        "round-trip not idempotent:\n--- first ---\n{}\n--- second ---\n{}",
        printed1, printed2
    );
    printed1
}

#[test]
fn empty_file() {
    let _ = assert_roundtrip("");
}

#[test]
fn single_function() {
    let _ = assert_roundtrip("function main() -> () { return }");
}

#[test]
fn function_with_simple_body() {
    let _ = assert_roundtrip(
        "function add(x: i32, y: i32) -> i32 { x + y }",
    );
}

#[test]
fn function_with_mode_params_and_effects() {
    let _ = assert_roundtrip(
        "public function push(buf: mutable TokenBuf, value: take Token, allocator: Allocator) -> () with {allocator, err: AllocError} { return }",
    );
}

#[test]
fn function_with_requires_ensures() {
    let _ = assert_roundtrip(
        "function abs(x: i32) -> i32 requires x >= 0 ensures result >= 0 { return x }",
    );
}

#[test]
fn function_with_decreases_clause() {
    // The codex shape from `01-syntax.md:339-348` and the worked
    // factorial example in `03-verification.md:269-276`. Round-trip
    // verifies the decreases clause survives parse → print → parse.
    let _ = assert_roundtrip(
        "function factorial(n: i64) -> i64 requires n >= 0 decreases n { return n }",
    );
}

#[test]
fn function_with_tuple_decreases_clause() {
    // Mutual-recursion-friendly lex-product tuple measure per
    // `03-verification.md:316-329`. Tuple decomposition into LIA
    // obligations happens in C6; the syntax round-trips here.
    let _ = assert_roundtrip(
        "function is_even(n: i64) -> bool requires n >= 0 decreases (n, 0) { return true }",
    );
}

#[test]
fn loop_with_decreases_clause() {
    // `loop decreases <expr> { body }` per `01-syntax.md:722-727` and
    // the worked sum example at `03-verification.md:291-301`.
    let _ = assert_roundtrip(
        "function sum_loop(n: i64) -> i64 { var i: i64 = 0\nloop decreases (n - i) { if i >= n { break }\ni = i + 1 } }",
    );
}

#[test]
fn derive_form_single_item() {
    // The locked `derive <items> for <Type>` form per
    // `corpus/edda-codex/language/04-specs-comptime.md` §5.
    let _ = assert_roundtrip("derive eq for Point");
}

#[test]
fn derive_form_multiple_items() {
    let _ = assert_roundtrip("derive eq, hash, debug for Point");
}

#[test]
fn derive_form_properties_for_function() {
    // The `derive properties` form per `03-verification.md:408-416` —
    // materialises an explicit `prop_<fn_name>` property test that
    // `edda test --properties` can address individually.
    let _ = assert_roundtrip("derive properties for gcd_extended");
}

#[test]
fn product_type() {
    let _ = assert_roundtrip("type Point { x: f64, y: f64 }");
}

#[test]
fn sum_type_unit_variant() {
    let _ = assert_roundtrip("type Color { case red, case green, case blue }");
}

#[test]
fn sum_type_tuple_payload() {
    let _ = assert_roundtrip("type V { case unit, case nums(i32, i32, i32) }");
}

#[test]
fn sum_type_named_payload() {
    let _ = assert_roundtrip("type Color { case rgb(r: u8, g: u8, b: u8) }");
}

#[test]
fn product_fields_emitted_newline_only_no_commas() {
    // `declarations.md` §"Product and sum types": "Fields and variants are
    // newline-separated. No trailing commas, no semicolons." The printer
    // must not emit a `,` between product-type fields.
    let printed = assert_roundtrip("type Point { x: f64, y: f64 }");
    assert!(
        !printed.contains("f64,"),
        "product fields must be newline-separated (no commas) per declarations.md §Product and sum types; got:\n{printed}"
    );
    assert!(
        printed.contains("x: f64\n"),
        "expected `x: f64` on its own line; got:\n{printed}"
    );
    assert!(
        printed.contains("y: f64\n"),
        "expected `y: f64` on its own line; got:\n{printed}"
    );
}

#[test]
fn sum_variants_emitted_newline_only_no_commas() {
    // Same rule for sum-type variants — no comma between `case ...` arms.
    let printed = assert_roundtrip("type Color { case red, case green, case blue }");
    // The variant-name token itself can appear without a trailing comma.
    assert!(
        !printed.contains("red,"),
        "sum variants must be newline-separated (no commas); got:\n{printed}"
    );
    assert!(
        !printed.contains("green,"),
        "sum variants must be newline-separated (no commas); got:\n{printed}"
    );
    assert!(
        printed.contains("case red\n"),
        "expected `case red` on its own line; got:\n{printed}"
    );
    assert!(
        printed.contains("case green\n"),
        "expected `case green` on its own line; got:\n{printed}"
    );
    assert!(
        printed.contains("case blue\n"),
        "expected `case blue` on its own line; got:\n{printed}"
    );
}

#[test]
fn imports() {
    let _ = assert_roundtrip("import std.fs\nimport value");
}

#[test]
fn imports_with_alias() {
    let printed = assert_roundtrip("import std.core.cmp as ccmp\nimport value as v");
    assert!(printed.contains("import std.core.cmp as ccmp"), "missing aliased multi-segment: {printed}");
    assert!(printed.contains("import value as v"), "missing aliased sibling: {printed}");
}

#[test]
fn expression_binary_precedence_preserved() {
    // 1 + 2 * 3 ⟶ Binary(Add, 1, Binary(Mul, 2, 3))
    // Printed conservatively as `1 + (2 * 3)`. Re-parses to same shape.
    let _ = assert_roundtrip("function f() -> i32 { 1 + 2 * 3 }");
}

#[test]
fn unary_and_cast() {
    let _ = assert_roundtrip("function f() -> i32 { (-x) as i32 * 2 }");
}

#[test]
fn method_call_on_path_round_trips_to_methodcall_when_paren() {
    // A path `(buf).len` is Field(Path([buf]), len). The printer must
    // emit parens so this AST shape survives re-parsing.
    let _ = assert_roundtrip("function f() -> () { (buf).len() }");
}

#[test]
fn if_match_loop_for() {
    let _ = assert_roundtrip(
        "function f(n: i32) -> i32 {
            if n > 0 { return n }
            for i in 0..<n { return i }
            loop { break 0 }
        }",
    );
}

#[test]
fn match_with_arms_and_guard() {
    let _ = assert_roundtrip(
        "function classify(v: i32) -> i32 { match v { case let x where x > 0 => 1 case _ => 0 } }",
    );
}

#[test]
fn struct_literal_and_field_access() {
    let _ = assert_roundtrip(
        "function origin() -> Point { Point { x: 0.0, y: 0.0 } }",
    );
}

// Doc-comment round-trip tests were removed with the no-comment lexer
// (the V1.0 no-comment design lock): comments reject as `comment_not_admitted`
// and so cannot round-trip. Rejection is covered by the edda-syntax lexer tests.

#[test]
fn stability_keyword_round_trips() {
    let _ = assert_roundtrip("stable public function f() -> () { return }");
    let _ = assert_roundtrip("unstable public function f() -> () { return }");
}

#[test]
fn stability_on_type_round_trips() {
    let _ = assert_roundtrip("stable public type Point { x: f64, y: f64 }");
    // D-19 — the post-visibility keyword form the stdlib uses.
    let _ = assert_roundtrip("public stable type Point { x: f64, y: f64 }");
    let _ = assert_roundtrip("public unstable type Point { x: f64, y: f64 }");
}

#[test]
fn stable_unstable_usable_as_identifiers() {
    // §3.7 stability modifiers are contextual soft-keywords: outside
    // stability position `stable` / `unstable` are ordinary identifiers —
    // function names, bindings, fields.
    let printed = assert_roundtrip("function stable() -> i32 { return 0 }");
    assert!(printed.contains("function stable("), "function name `stable` lost: {printed}");
    let printed = assert_roundtrip("function f() -> i32 { let unstable: i32 = 0; return unstable }");
    assert!(printed.contains("unstable"), "binding `unstable` lost: {printed}");
    // `stable` as a refinement-stability marker on a function whose name is
    // also `stable` — both positions resolve correctly.
    let _ = assert_roundtrip("stable function stable() -> i32 { return 0 }");
}

#[test]
fn integer_bases_preserved() {
    // Hex, binary, octal bases survive the round-trip via the printer
    // emitting the original base prefix.
    let _ = assert_roundtrip(
        "function f() -> i32 { 0xFF + 0b1010 + 0o755 + 42 }",
    );
}

#[test]
fn try_and_await_postfix() {
    let _ = assert_roundtrip(
        "function f() -> () { (g()).await; (h())? }",
    );
}

#[test]
fn call_site_modes_round_trip() {
    let printed = assert_roundtrip(
        "function caller(buf: mutable Buf, val: take Item, dst: init Out) -> () { push(mutable buf, take val, init dst, val) }",
    );
    // Spot-check that the keywords survive — otherwise round-trip
    // could vacuously hold if the parser silently dropped them.
    assert!(printed.contains("mutable buf"), "missing `mutable buf` in {printed}");
    assert!(printed.contains("take val"), "missing `take val` in {printed}");
    assert!(printed.contains("init dst"), "missing `init dst` in {printed}");
}

#[test]
fn named_payload_call_args_round_trip() {
    // `declarations.md` §252: variant constructors take named payload
    // arguments. The printer must emit `<name>: <expr>` so the form
    // survives parse → print → parse.
    let printed = assert_roundtrip(
        "function f() -> Phase { return Phase.yellow(seconds_remaining: 3) }",
    );
    assert!(
        printed.contains("seconds_remaining:"),
        "named-arg keyword missing from output: {printed}"
    );
}

#[test]
fn scope_binder_form_round_trips() {
    // `effects.md` §"scope(exec) name { ... }" locks the binder form.
    // The binder `group` names the structured-concurrency scope so the
    // body can reference it as `<name>.spawn(...)`.
    let printed = assert_roundtrip(
        "function example() -> () { scope(exec) group { let x = 1 } }",
    );
    assert!(
        printed.contains("scope(exec) group"),
        "scope binder missing from output: {printed}"
    );
}

#[test]
fn spawn_block_bare_form_round_trips() {
    // `effects.md` §"Structured concurrency" — the bare form
    // `<scope>.spawn { body }` admits implicit shareable captures only.
    let printed = assert_roundtrip(
        "function example() -> () { scope(exec) group { let t = group.spawn { compute() } } }",
    );
    assert!(
        printed.contains("group.spawn {"),
        "spawn-block surface missing from output: {printed}"
    );
}

#[test]
fn spawn_block_with_take_arg_round_trips() {
    // The locked argument grammar admits `take <ident> = <expr>`
    // entries, threading single-task capabilities across the spawn
    // boundary by ownership transfer.
    let printed = assert_roundtrip(
        "function example() -> () { scope(exec) group { let t = group.spawn(take alloc = parent.fork()) { work(alloc) } } }",
    );
    assert!(
        printed.contains("group.spawn(take alloc = "),
        "spawn-arg surface missing from output: {printed}"
    );
}

#[test]
fn spawn_block_with_multi_args_round_trips() {
    // Two take-args, the first with an explicit type annotation, the
    // second bare. Exercises the comma separator and the optional
    // `: <Type>` between binder and `=`.
    let printed = assert_roundtrip(
        "function example() -> () { scope(exec) group { let t = group.spawn(take alloc: Allocator = parent.fork(), take sub = exec.child()) { work(alloc, sub) } } }",
    );
    assert!(
        printed.contains("take alloc: Allocator = "),
        "typed spawn-arg surface missing from output: {printed}"
    );
    assert!(
        printed.contains(", take sub = "),
        "second spawn-arg surface missing from output: {printed}"
    );
}

#[test]
fn spawn_method_call_still_routes_to_method_call() {
    // A `<obj>.spawn(arg)` call where the parens contain a non-`take`
    // expression must continue to parse as a regular method call so
    // user types with a `spawn` method are not silently misparsed.
    let printed = assert_roundtrip(
        "function example() -> () { let _ = group.spawn(42) }",
    );
    assert!(
        printed.contains(".spawn(") && printed.contains("42)"),
        "method call form regressed: {printed}"
    );
    assert!(
        !printed.contains("scope(exec)"),
        "unexpected scope wrapping: {printed}"
    );
}

#[test]
fn scope_binder_free_form_still_admitted() {
    // The legacy binder-free form `scope(exec) { ... }` remains
    // admitted via `Option<Ident> = None` for backward compatibility.
    let printed = assert_roundtrip(
        "function example() -> () { scope(exec) { let x = 1 } }",
    );
    assert!(
        printed.contains("scope(exec) {"),
        "binder-free scope missing from output: {printed}"
    );
    // Sanity: there is no identifier between `scope(exec)` and `{`.
    assert!(
        !printed.contains("scope(exec) group"),
        "unexpected binder in output: {printed}"
    );
}

#[test]
fn spec_declaration_round_trips() {
    // `comptime.md` §253 — spec body is an item list. Round-trip a
    // declaration with a `comptime` generic, a `where` clause, and one
    // function item.
    let printed = assert_roundtrip(
        "public spec Stack(comptime T: Type) where T == i32 { function push() -> () { return } }",
    );
    assert!(printed.contains("public spec Stack"));
    assert!(printed.contains("comptime T: Type"));
    assert!(printed.contains("where"));
}

#[test]
fn spec_invocation_round_trips() {
    // `comptime.md` §312 — invocation has no body; the printer emits
    // `spec <path>(<args>)`.
    let printed = assert_roundtrip("spec std.option.Option(i32)");
    assert!(printed.contains("spec std.option.Option("));
}

#[test]
fn explicit_printer_construction() {
    // Sanity: the `Printer` type is exposed for callers that want to
    // build output incrementally.
    let interner = Interner::new();
    let mut p = Printer::new(&interner);
    p.write("hello");
    assert_eq!(p.finish(), "hello");
}

// The expression-level helper is also publicly callable.
#[test]
fn parse_expr_and_print_round_trips() {
    let src = "1 + 2 * (3 - 4)";
    let map = SourceMap::new();
    let file = map.add_file(PathBuf::from("e.ea"), src.to_string());
    let interner = Interner::new();
    let mut diags = Diagnostics::new();
    let cfg = LintConfig::new();
    let tokens = lex(src, file, &interner, &mut diags, &cfg);
    let e = parse_expr(&tokens, &interner, &mut diags, &cfg);
    let mut p = Printer::new(&interner);
    p.print_expr(&e);
    let printed = p.finish();
    // Parse the printed form back; it should yield an equal printed form.
    let mut diags2 = Diagnostics::new();
    let file2 = map.add_file(PathBuf::from("e2.ea"), printed.clone());
    let tokens2 = lex(&printed, file2, &interner, &mut diags2, &cfg);
    let e2 = parse_expr(&tokens2, &interner, &mut diags2, &cfg);
    let mut p2 = Printer::new(&interner);
    p2.print_expr(&e2);
    assert_eq!(printed, p2.finish());
}

#[test]
fn single_export_attribute_round_trips() {
    let _ = assert_roundtrip(
        "@export(\"edda_callback\")
public function callback() -> i32 { return 0 }",
    );
}

#[test]
fn multiple_attributes_round_trip() {
    let _ = assert_roundtrip(
        "@export(\"foo\")
@abi(\"sysv64\")
public function foo() -> i32 { return 0 }",
    );
}

#[test]
fn attribute_with_ident_arg_round_trips() {
    let _ = assert_roundtrip(
        "@repr(C)
type Point { x: i32 }",
    );
}

#[test]
fn attribute_with_int_arg_dec_round_trips() {
    let _ = assert_roundtrip(
        "@align(8)
type Aligned { x: i32 }",
    );
}

#[test]
fn attribute_with_int_arg_hex_preserves_base() {
    // The user's chosen base must round-trip verbatim — 0x10 stays 0x10.
    let printed = assert_roundtrip(
        "@align(0x10)
type Aligned { x: i32 }",
    );
    assert!(
        printed.contains("0x10"),
        "expected hex base preserved, got:\n{}",
        printed
    );
}

#[test]
fn attribute_with_named_arg_round_trips() {
    let _ = assert_roundtrip(
        "@align(value = 16)
type Aligned { x: i32 }",
    );
}

#[test]
fn attribute_with_bool_arg_round_trips() {
    let _ = assert_roundtrip(
        "@some_flag(true)
type Foo { x: i32 }",
    );
}

#[test]
fn attribute_with_mixed_positional_then_named_round_trips() {
    let _ = assert_roundtrip(
        "@cfg(target, feature = \"x\")
function f() -> () { return }",
    );
}

