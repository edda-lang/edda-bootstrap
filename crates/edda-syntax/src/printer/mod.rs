//! Pretty-printer for the Edda AST.
//!
//! Implements the round-trip rule from `docs/tooling/structural-edits.md`:
//! **parse → print → parse must produce the same AST.**
//!
//! The output is not optimised for human readability. The printer aims
//! squarely at correctness: every compound subexpression is wrapped in
//! parentheses, blocks span multiple indented lines, and statement
//! terminators are explicit. A future "format" mode can elide redundant
//! parens; round-trip safety is the contract this module signs for.

use edda_intern::Interner;

use crate::ast::File;

mod attr;
mod expr;
mod item;
mod pat;
mod stmt;
#[cfg(test)]
mod tests;
mod ty;

pub use item::print_file;

const INDENT: &str = "    ";

/// Buffered pretty-printer over an [`Interner`]. Construct with
/// [`Printer::new`], call one of the public `print_*` entry points, then
/// retrieve the source text via [`Printer::finish`].
pub struct Printer<'a> {
    out: String,
    interner: &'a Interner,
    indent: u32,
}

impl<'a> Printer<'a> {
    /// Construct an empty printer. The interner must match the one the
    /// AST was lexed with so that `Symbol` lookups produce the original
    /// text.
    pub fn new(interner: &'a Interner) -> Self {
        Printer {
            out: String::new(),
            interner,
            indent: 0,
        }
    }

    /// Consume the printer and return the emitted source.
    pub fn finish(self) -> String {
        self.out
    }

    /// Emit a complete file: file-head doc-comments (tier-tagged), then items.
    pub fn print_file(&mut self, file: &File) {
        for doc in &file.doc {
            self.print_doc_line(doc);
        }
        for (i, item) in file.items.iter().enumerate() {
            if i > 0 {
                self.write_newline();
            }
            self.print_item(item);
            self.write_newline();
        }
    }

    /// Emit one doc-comment line with its tier marker and a trailing newline.
    pub(crate) fn print_doc_line(&mut self, line: &crate::ast::DocLine) {
        let prefix = match line.tier {
            crate::ast::DocTier::High => "!!! ",
            crate::ast::DocTier::Medium => "/!! ",
            crate::ast::DocTier::Low => "//! ",
            crate::ast::DocTier::Legacy => "/// ",
        };
        self.write(prefix);
        self.write_resolved(line.body);
        self.write_newline();
    }

    pub(crate) fn write(&mut self, s: &str) {
        self.out.push_str(s);
    }

    pub(crate) fn write_resolved(&mut self, sym: edda_intern::Symbol) {
        let s = self.interner.try_resolve(sym).unwrap_or("");
        self.out.push_str(s);
    }

    pub(crate) fn write_newline(&mut self) {
        self.out.push('\n');
        for _ in 0..self.indent {
            self.out.push_str(INDENT);
        }
    }

    pub(crate) fn with_indent<F: FnOnce(&mut Self)>(&mut self, f: F) {
        self.indent += 1;
        f(self);
        self.indent -= 1;
    }

    pub(crate) fn comma_separated<T, F: FnMut(&mut Self, &T)>(&mut self, items: &[T], mut f: F) {
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.write(", ");
            }
            f(self, item);
        }
    }
}
