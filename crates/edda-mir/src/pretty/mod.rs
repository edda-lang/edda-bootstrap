//! Human-readable MIR pretty-printer.
//!
//! The output is intended for inspection by compiler authors — it is not a
//! parser input. Round-trip is not a goal at this layer (the surface AST
//! printer in `edda-syntax` owns that contract). Use [`pretty`] as the
//! one-shot entry point; [`PrettyPrinter`] is exposed for callers that need to
//! interleave MIR output with surrounding text.

use edda_intern::Interner;

use crate::program::MirProgram;

mod body;
mod operand;
mod ty;

/// Four-space indentation matches the surface AST printer convention.
const INDENT: &str = "    ";

/// Render `program` as a string using `interner` to resolve [`edda_intern::Symbol`]s.
pub fn pretty(program: &MirProgram, interner: &Interner) -> String {
    let mut p = PrettyPrinter::new(interner);
    p.print_program(program);
    p.finish()
}

/// Buffered pretty-printer over an [`Interner`].
pub struct PrettyPrinter<'a> {
    out: String,
    interner: &'a Interner,
    indent: usize,
}

impl<'a> PrettyPrinter<'a> {
    /// Construct an empty printer.
    pub fn new(interner: &'a Interner) -> Self {
        PrettyPrinter {
            out: String::new(),
            interner,
            indent: 0,
        }
    }

    /// Consume the printer and return the emitted text.
    pub fn finish(self) -> String {
        self.out
    }

    /// Render an entire [`MirProgram`].
    pub fn print_program(&mut self, program: &MirProgram) {
        for (id, adt) in program.adts.iter_enumerated() {
            self.print_adt(id, adt);
            self.write_str("\n");
        }
        for (id, body) in program.bodies.iter_enumerated() {
            self.print_body(id, body, program);
            self.write_str("\n");
        }
        if let Some(entry) = program.entry {
            self.write_str("entry = ");
            self.write_str(&format!("body{}", entry.as_u32()));
            self.write_str(";\n");
        }
    }

    /// Append `s` verbatim.
    pub(crate) fn write_str(&mut self, s: &str) {
        self.out.push_str(s);
    }

    /// Append the leading whitespace for the current indent level.
    pub(crate) fn write_indent(&mut self) {
        for _ in 0..self.indent {
            self.out.push_str(INDENT);
        }
    }

    /// Append `s` followed by a newline.
    pub(crate) fn write_line(&mut self, s: &str) {
        self.write_indent();
        self.out.push_str(s);
        self.out.push('\n');
    }

    /// Run `f` with the indent level temporarily increased by one.
    pub(crate) fn with_indent<F: FnOnce(&mut Self)>(&mut self, f: F) {
        self.indent += 1;
        f(self);
        self.indent -= 1;
    }

    /// Resolve a [`edda_intern::Symbol`] to its source `&str`.
    pub(crate) fn resolve(&self, sym: edda_intern::Symbol) -> &str {
        self.interner.resolve(sym)
    }
}
