use super::format::compose_module_path;
use crate::argument::{Argument, ArgumentTuple};
use edda_span::Span;
use edda_syntax::ast::{Expr, ExprKind, Ident, Import, Item, ItemKind, Path, SpecInvocation};
use edda_intern::Interner;
use smol_str::SmolStr;

fn args(items: Vec<Argument>) -> ArgumentTuple {
    ArgumentTuple::new(items)
}

#[test]
fn compose_module_path_appends_disambig_hex_for_single_segment_arg() {
    // Every materialised module gets an
    // 8-hex disambig suffix derived from `(spec_leaf, args)` so two
    // same-leaf invocations whose args differ in fully-qualified form
    // produce distinct module paths. `Vec(String)` produces
    // `std.collections.vec.Vec_String_<8hex>` — the suffix is
    // deterministic and computable via `module_disambig_hex`.
    let args_tuple = args(vec![Argument::Type(SmolStr::new("String"))]);
    let actual = compose_module_path("std.collections.vec.Vec", &args_tuple);
    let hex = crate::mangle::module_disambig_hex("std.collections.vec.Vec", &args_tuple)
        .expect("Type-arg-only invocations always have a disambig hex");
    assert_eq!(actual, format!("std.collections.vec.Vec_String_{hex}"));
    // The hex is exactly 8 lowercase ASCII chars from BLAKE3.
    assert_eq!(hex.len(), 8);
    assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
}

#[test]
fn compose_module_path_disambig_distinguishes_same_leaf_distinct_qnames() {
    // The exact bug case — two derive sites
    // for `eq` whose target type-leaf is `BlockId` but whose qualifying
    // path differs (`backend.hlir.id.block.BlockId` vs
    // `backend.lir.id.block.BlockId`). Under the prior leaf-only
    // composition these collided at `std.core.compare.eq_BlockId`; the
    // disambig suffix forces them apart.
    let hlir = compose_module_path(
        "std.core.compare.eq",
        &args(vec![Argument::Type(SmolStr::new(
            "backend.hlir.id.block.BlockId",
        ))]),
    );
    let lir = compose_module_path(
        "std.core.compare.eq",
        &args(vec![Argument::Type(SmolStr::new(
            "backend.lir.id.block.BlockId",
        ))]),
    );
    assert_ne!(hlir, lir, "distinct qnames must produce distinct module paths");
    assert!(hlir.starts_with("std.core.compare.eq_BlockId_"));
    assert!(lir.starts_with("std.core.compare.eq_BlockId_"));
}

#[test]
fn compose_module_path_disambig_collapses_same_qname() {
    // Two invocations with byte-identical
    // args MUST produce identical module paths so content-addressed
    // dedup still routes them through one cached artifact.
    let a = compose_module_path(
        "std.core.option.Option",
        &args(vec![Argument::Type(SmolStr::new("kind_mod.Token"))]),
    );
    let b = compose_module_path(
        "std.core.option.Option",
        &args(vec![Argument::Type(SmolStr::new("kind_mod.Token"))]),
    );
    assert_eq!(a, b);
    assert!(a.starts_with("std.core.option.Option_Token_"));
}

#[test]
fn compose_module_path_disambig_admits_kind_module_qualified_arg() {
    // Single-segment leading lowercase Edda module names still strip
    // through `type_leaf_mangle` for the mangled short-name component
    // (`Option_Token`, not `Option_kind_mod_Token`) — the new disambig
    // suffix is what distinguishes invocations whose stripped form
    // collides.
    let a = compose_module_path(
        "std.core.option.Option",
        &args(vec![Argument::Type(SmolStr::new("kind_mod.Token"))]),
    );
    let b = compose_module_path(
        "std.core.option.Option",
        &args(vec![Argument::Type(SmolStr::new("probe78.main.Token"))]),
    );
    // Both mangle to `Option_Token` after strip-lowercase; the disambig
    // hex makes them distinct because their qnames differ.
    assert!(a.starts_with("std.core.option.Option_Token_"));
    assert!(b.starts_with("std.core.option.Option_Token_"));
    assert_ne!(a, b);
}

#[test]
fn compose_module_path_path_qualifies_nested_spec_args_under_collision() {
    // `Vec(Vec_String.Vec)` and `Vec(Vec_usize.Vec)` materialise under
    // distinct module paths even before the disambig suffix because the
    // spec-mangled `Vec_String` / `Vec_usize` intermediate is
    // uppercase-initialled and retained by the strip-lowercase rule —
    // and the disambig hex further differentiates them through
    // their distinct qnames.
    let a = compose_module_path(
        "std.collections.vec.Vec",
        &args(vec![Argument::Type(SmolStr::new(
            "std.collections.vec.Vec_String.Vec",
        ))]),
    );
    let b = compose_module_path(
        "std.collections.vec.Vec",
        &args(vec![Argument::Type(SmolStr::new(
            "std.collections.vec.Vec_usize.Vec",
        ))]),
    );
    assert!(a.starts_with("std.collections.vec.Vec_Vec_String_Vec_"));
    assert!(b.starts_with("std.collections.vec.Vec_Vec_usize_Vec_"));
    assert_ne!(a, b);
}

#[test]
fn artifact_body_for_vec_string_has_no_spurious_sibling_spec_import() {
    // A `Vec(String)`
    // materialisation must NOT emit `import std.core.option.Option_String`
    // alongside its `spec std.core.option.Option(String)` directive —
    // the spec line is the canonical materialisation request, and
    // the spurious import (treating the sibling artifact's leaf as a
    // stdlib path that doesn't exist) is what flooded the cache port
    // and runes with `unresolved stdlib import` errors. This test
    // exercises the emitter path that produces the artifact body by
    // composing a minimal substituted item slice (one bona-fide
    // `std.mem.alloc` import + the nested sibling `spec` line) and
    // asserts the only `import` line in the emitted bytes is the
    // bona-fide one — no `import std.core.option.Option_String`,
    // no `import std.<anything>.Option_*` or `_Vec_*`.
    let interner = Interner::new();
    let module_path = compose_module_path(
        "std.collections.vec.Vec",
        &args(vec![Argument::Type(SmolStr::new("String"))]),
    );

    // Bona-fide parent import: `import std.mem.alloc` from vec.ea.
    let alloc_import_item = Item {
        span: Span::DUMMY,
        doc: Vec::new(),
        attributes: Vec::new(),
        kind: ItemKind::Import(Import {
            span: Span::DUMMY,
            path: Path {
                segments: vec![
                    Ident { name: interner.intern("std"), span: Span::DUMMY },
                    Ident { name: interner.intern("mem"), span: Span::DUMMY },
                    Ident { name: interner.intern("alloc"), span: Span::DUMMY },
                ],
                span: Span::DUMMY,
            },
            alias: None,
            selection: None,
        }),
    };

    // The substituted spec body, post-`T := String`, contains a
    // nested `spec std.core.option.Option(String)`. The sibling-spec
    // emission path is the canonical materialisation line —
    // critically, it does NOT involve an `import` directive.
    let option_spec_item = Item {
        span: Span::DUMMY,
        doc: Vec::new(),
        attributes: Vec::new(),
        kind: ItemKind::SpecInvocation(Box::new(SpecInvocation {
            span: Span::DUMMY,
            path: Path {
                segments: vec![
                    Ident { name: interner.intern("std"), span: Span::DUMMY },
                    Ident { name: interner.intern("core"), span: Span::DUMMY },
                    Ident { name: interner.intern("option"), span: Span::DUMMY },
                    Ident { name: interner.intern("Option"), span: Span::DUMMY },
                ],
                span: Span::DUMMY,
            },
            args: vec![Expr {
                span: Span::DUMMY,
                kind: ExprKind::Path(Path {
                    segments: vec![Ident {
                        name: interner.intern("String"),
                        span: Span::DUMMY,
                    }],
                    span: Span::DUMMY,
                }),
            }],
        })),
    };

    let items = vec![alloc_import_item, option_spec_item];
    let bytes = crate::emit::emit_items(items, &interner, Some(&module_path));
    let text = std::str::from_utf8(&bytes).expect("emit_items produces utf-8");

    assert!(
        text.contains("import std.mem.alloc"),
        "bona-fide parent import was filtered out; emitted:\n{text}",
    );
    assert!(
        text.contains("spec std.core.option.Option(String)"),
        "sibling spec materialisation line missing; emitted:\n{text}",
    );
    // The exact spurious-import shape the sibling-spec-import fix reported.
    assert!(
        !text.contains("import std.core.option.Option_String"),
        "spurious sibling-spec import line resurfaced; emitted:\n{text}",
    );
    // Same anti-pattern, less specific match — any
    // `import <stdlib>.<Mangled_Leaf>` line that names a
    // sibling-spec materialisation is a contract violation.
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("import ") {
            assert!(
                !rest.contains("Option_"),
                "import line references a sibling-spec materialisation leaf: `{trimmed}`",
            );
            assert!(
                !rest.contains("Vec_"),
                "import line references a sibling-spec materialisation leaf: `{trimmed}`",
            );
        }
    }
}
