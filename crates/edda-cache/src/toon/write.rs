//! TOON writer: builder-style emitter with an optional comment-prefix mode.

use std::fmt::Write as _;

use super::INDENT_STEP;

/// Builder for emitting a TOON document. The writer owns an output buffer
/// and tracks the current indent level. Use the helpers
/// ([`scalar`](Self::scalar), [`block`](Self::block),
/// [`list`](Self::list), etc.) to build up the document.
pub struct Writer {
    out: String,
    indent: usize,
    comment_prefix: Option<&'static str>,
}

impl Writer {
    /// Construct a writer that emits plain TOON.
    pub fn new() -> Self {
        Writer {
            out: String::new(),
            indent: 0,
            comment_prefix: None,
        }
    }

    /// Construct a writer that prefixes every output line with `// ` (used
    /// by `header.rs` to emit artifact headers as Edda line comments so
    /// the generated artifact round-trips through `edda_syntax::parse_file`).
    pub fn commented() -> Self {
        Writer {
            out: String::new(),
            indent: 0,
            comment_prefix: Some("// "),
        }
    }

    /// Append a free-form comment line (`\ <text>`). Allowed only in
    /// plain mode — in commented mode every line is already a comment, so
    /// use [`bare`](Self::bare) instead.
    pub fn comment(&mut self, text: &str) {
        debug_assert!(
            self.comment_prefix.is_none(),
            "Writer::comment in commented mode (use bare)"
        );
        let _ = writeln!(self.out, "\\ {}", text);
    }

    /// Emit a bare line containing just `text` (preceded by indent and,
    /// in commented mode, by the comment prefix). Used by the header
    /// module for the `// @generated` marker, which has no `key: value`
    /// shape.
    pub fn bare(&mut self, text: &str) {
        self.write_indent();
        self.out.push_str(text);
        self.out.push('\n');
    }

    /// Append a blank line separator.
    pub fn blank_line(&mut self) {
        if let Some(prefix) = self.comment_prefix {
            // Header-style blank: emit a bare `\` so the line is still
            // syntactically a comment.
            self.out.push_str(prefix.trim_end());
            self.out.push('\n');
        } else {
            self.out.push('\n');
        }
    }

    /// Emit `key: value` on a single line. `value` is quoted if it
    /// contains characters that would break unquoted parsing.
    pub fn scalar(&mut self, key: &str, value: &str) {
        self.write_indent();
        let _ = write!(self.out, "{}: ", key);
        self.write_scalar_value(value);
        self.out.push('\n');
    }

    /// Emit `key: []`.
    pub fn empty_list(&mut self, key: &str) {
        self.write_indent();
        let _ = writeln!(self.out, "{}: []", key);
    }

    /// Emit `key:` and call `body` with an increased indent. The body is
    /// expected to emit at least one child line (otherwise the resulting
    /// document is not round-trippable as a non-empty block).
    pub fn block<F: FnOnce(&mut Writer)>(&mut self, key: &str, body: F) {
        self.write_indent();
        let _ = writeln!(self.out, "{}:", key);
        self.indent += INDENT_STEP;
        body(self);
        self.indent -= INDENT_STEP;
    }

    /// Emit `key:` annotated with `[N]{f1,f2,...}` and call `body` with an
    /// increased indent. Used by the manifest module for the `artifacts:`
    /// list so the on-disk form matches `migration.md` §4.
    pub fn list_with_schema<F: FnOnce(&mut Writer)>(
        &mut self,
        key: &str,
        field_names: &[&str],
        count: usize,
        body: F,
    ) {
        self.write_indent();
        let _ = write!(self.out, "{}[{}]{{", key, count);
        for (i, name) in field_names.iter().enumerate() {
            if i > 0 {
                self.out.push(',');
            }
            self.out.push_str(name);
        }
        let _ = writeln!(self.out, "}}:");
        self.indent += INDENT_STEP;
        body(self);
        self.indent -= INDENT_STEP;
    }

    /// Emit a `- key: value` list-item row and call `body` to emit any
    /// continuation lines (which appear at `INDENT_STEP` deeper than the
    /// `-` marker). The first-field shape is explicit because YAML-style
    /// list items share the `-` marker's line with their first entry —
    /// matching the spec's example output exactly.
    pub fn list_item<F: FnOnce(&mut Writer)>(
        &mut self,
        first_key: &str,
        first_value: &str,
        body: F,
    ) {
        self.write_indent();
        self.out.push_str("- ");
        self.out.push_str(first_key);
        self.out.push_str(": ");
        self.write_scalar_value(first_value);
        self.out.push('\n');
        self.indent += INDENT_STEP;
        body(self);
        self.indent -= INDENT_STEP;
    }

    /// Emit a bare-scalar list item: `- <value>`. Used for lists of
    /// strings such as the `sources:` and `nested_deps:` blocks in
    /// `manifest.toon`.
    pub fn list_item_scalar(&mut self, value: &str) {
        self.write_indent();
        self.out.push_str("- ");
        self.write_scalar_value(value);
        self.out.push('\n');
    }

    /// Consume the writer and return the accumulated TOON text.
    pub fn finish(self) -> String {
        self.out
    }

    /// Write the current indentation, prefixed by the comment marker if
    /// the writer is in commented mode.
    fn write_indent(&mut self) {
        if let Some(prefix) = self.comment_prefix {
            self.out.push_str(prefix);
        }
        for _ in 0..self.indent {
            self.out.push(' ');
        }
    }

    /// Emit a scalar value, quoting when necessary.
    fn write_scalar_value(&mut self, value: &str) {
        if scalar_needs_quoting(value) {
            self.out.push('"');
            for c in value.chars() {
                if c == '"' || c == '\\' {
                    self.out.push('\\');
                }
                self.out.push(c);
            }
            self.out.push('"');
        } else {
            self.out.push_str(value);
        }
    }
}

impl Default for Writer {
    fn default() -> Self {
        Writer::new()
    }
}

/// Decide whether `s` must be quoted to round-trip through the lexer.
fn scalar_needs_quoting(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    s.chars().any(|c| {
        matches!(c, ':' | ',' | '"' | '\\' | '#' | '[' | ']' | '{' | '}' | '\n' | '\r')
            || c.is_whitespace()
    })
}
