//! Completion suggestions for the LSP `textDocument/completion` request.
//!
//! v0.1 returns the locked keyword set (`declarations.md` /
//! `expressions.md` / `effects.md` / `refinements.md` / `comptime.md`)
//! filtered against the identifier prefix the cursor sits on. The
//! structural index (item names, scoped bindings) is not yet exposed by
//! the daemon at this layer; that lands in a follow-up that wires
//! a `completion_at(file, pos)` query through `edda-daemon`.

use lsp_types::{CompletionItem, CompletionItemKind};

/// The locked reserved keyword set, in declaration order grouped
/// by category. Used both for completion offerings and for prefix
/// filtering.
const KEYWORDS: &[&str] = &[
    // Declarations
    "function", "module", "import", "public", "type", "case", "spec",
    // Control flow
    "if", "else", "match", "for", "in", "loop", "break", "continue", "return",
    // Bindings
    "let", "var", "uninit",
    // Type / refinement
    "where", "as", "requires", "ensures", "result",
    // Effects
    "with", "raise", "panic", "scope", "await",
    // Parameter modes
    "mutable", "take", "init",
    // Comptime
    "comptime",
    // Booleans (lowercase literals)
    "true", "false",
];

/// Suggest completions matching the identifier prefix `prefix`.
///
/// When `prefix` is empty (e.g. completion fired without context), every
/// keyword is offered. The list is returned sorted in declaration order;
/// LSP clients re-sort by their own ranking rules.
pub fn keyword_completions(prefix: &str) -> Vec<CompletionItem> {
    let lower = prefix.to_ascii_lowercase();
    let mut out = Vec::with_capacity(KEYWORDS.len());
    // Bounded loop: one iteration per keyword.
    for kw in KEYWORDS {
        if prefix.is_empty() || kw.starts_with(lower.as_str()) {
            out.push(CompletionItem {
                label: (*kw).to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                detail: None,
                documentation: None,
                ..Default::default()
            });
        }
    }
    out
}

/// Extract the identifier prefix immediately before column `col` on
/// `line`. `col` is a byte offset in `line`.
pub fn identifier_prefix(line: &str, col: usize) -> &str {
    let bytes = line.as_bytes();
    let clamped = col.min(bytes.len());
    let mut start = clamped;
    // Bounded loop: at most `clamped` iterations.
    while start > 0 {
        let b = bytes[start - 1];
        if is_ident_byte(b) {
            start -= 1;
        } else {
            break;
        }
    }
    &line[start..clamped]
}

/// Whether `b` is an ASCII identifier byte (`[A-Za-z0-9_]`).
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_prefix_returns_full_list() {
        let items = keyword_completions("");
        assert_eq!(items.len(), KEYWORDS.len());
    }

    #[test]
    fn prefix_filters() {
        let items = keyword_completions("re");
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert!(labels.contains(&"return"));
        assert!(labels.contains(&"requires"));
        assert!(labels.contains(&"result"));
        assert!(!labels.contains(&"function"));
    }

    #[test]
    fn prefix_extract() {
        assert_eq!(identifier_prefix("let foo", 7), "foo");
        assert_eq!(identifier_prefix("let foo", 4), "");
        assert_eq!(identifier_prefix("a.b", 3), "b");
    }
}
