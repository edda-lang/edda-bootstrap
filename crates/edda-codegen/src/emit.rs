//! Body-source emission — pretty-print substituted spec items to UTF-8 source bytes.
//!
//! The emitted bytes are what gets stored as [`crate::StageRequest::body_source`]:
//! valid Edda source below the `\ @generated` header that [`crate::CodegenSession::stage`]
//! prepends. The round-trip rule from `docs/tooling/structural-edits.md` is
//! inherited from the `edda-syntax` pretty-printer with no additional work here.

use edda_intern::Interner;
use edda_span::Span;
use edda_syntax::ast::{File, Ident, Item, ItemKind, ModuleDecl, Path};
use edda_syntax::print_file;

/// Render a list of substituted spec-body items to UTF-8 source bytes.
///
/// Wraps `items` in a synthetic [`File`] with a dummy span and no inner
/// doc-comments, then drives the `edda-syntax` pretty-printer. The printer's
/// round-trip invariant guarantees the result is re-parseable without loss.
///
/// When `module_path` is `Some(<dotted>)`, a `module <dotted>` declaration
/// is emitted before `items`; this lets the resolver assign the artifact a
/// stable canonical module path independent of its on-disk cache location
/// (cache-tier sharding by hash prefix otherwise produces unaddressable
/// module paths). When `None`, no override is emitted and the file's
/// module identity falls back to the resolver's path-derived rule.
pub fn emit_items(items: Vec<Item>, interner: &Interner, module_path: Option<&str>) -> Vec<u8> {
    let mut all_items = Vec::with_capacity(items.len() + module_path.is_some() as usize);
    if let Some(path_text) = module_path {
        all_items.push(synthesize_module_item(path_text, interner));
    }
    all_items.extend(items);
    let file = File { span: Span::DUMMY, doc: Vec::new(), items: all_items };
    print_file(&file, interner).into_bytes()
}

/// Construct a synthetic `Item::Module(ModuleDecl)` whose `path` segments
/// are the dot-split `path_text` interned through `interner`.
fn synthesize_module_item(path_text: &str, interner: &Interner) -> Item {
    let segments: Vec<Ident> = path_text
        .split('.')
        .map(|seg| Ident { name: interner.intern(seg), span: Span::DUMMY })
        .collect();
    debug_assert!(!segments.is_empty(), "synthesize_module_item: empty path");
    let path = Path { segments, span: Span::DUMMY };
    Item {
        span: Span::DUMMY,
        doc: Vec::new(),
        attributes: Vec::new(),
        kind: ItemKind::Module(ModuleDecl { span: Span::DUMMY, path }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_module_declaration_when_path_supplied() {
        let interner = Interner::new();
        let bytes = emit_items(Vec::new(), &interner, Some("std.alloc.Box_Expr"));
        let text = std::str::from_utf8(&bytes).expect("emitter produces utf-8");
        assert!(
            text.contains("module std.alloc.Box_Expr"),
            "expected `module std.alloc.Box_Expr` in emitted source; got:\n{text}",
        );
    }

    #[test]
    fn emits_no_module_declaration_when_path_omitted() {
        let interner = Interner::new();
        let bytes = emit_items(Vec::new(), &interner, None);
        let text = std::str::from_utf8(&bytes).expect("emitter produces utf-8");
        assert!(
            !text.contains("module "),
            "expected no `module` keyword in emitted source; got:\n{text}",
        );
    }
}
