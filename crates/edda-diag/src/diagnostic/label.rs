//! Labelled-span primitives: the pre-resolved [`ResolvedLocation`], the
//! [`Label`] pointing at a source range, and one [`CounterexampleValue`]
//! row of an SMT counterexample.

use std::path::PathBuf;
use std::sync::Arc;

use edda_span::{LineCol, Span};

/// Pre-resolved location for a [`Label`]. The driver populates this
/// after the cascade by walking diagnostics against the live
/// `SourceMap`, so downstream renderers (CLI, MCP wire) do not need the
/// `SourceMap` to surface file/line/col.
#[derive(Clone, Debug)]
pub struct ResolvedLocation {
    /// Path the [`edda_span::SourceMap`] registered for `span.file`.
    pub file_name: Arc<std::path::Path>,
    /// 1-based start line/column derived via [`edda_span::SourceMap::byte_to_linecol`].
    pub line_col_start: LineCol,
    /// 1-based end line/column derived via [`edda_span::SourceMap::byte_to_linecol`].
    pub line_col_end: LineCol,
}

impl ResolvedLocation {
    /// Construct a resolved location from its three components.
    pub fn new(file_name: impl Into<PathBuf>, line_col_start: LineCol, line_col_end: LineCol) -> Self {
        ResolvedLocation {
            file_name: Arc::from(file_name.into()),
            line_col_start,
            line_col_end,
        }
    }
}

/// A labelled source range. A [`Diagnostic`] has one primary label
/// ([`Diagnostic::primary`]) and zero or more secondary labels.
#[derive(Clone, Debug)]
pub struct Label {
    /// Source range this label points at. May be `Span::DUMMY` for labels
    /// attached to synthetic AST nodes.
    pub span: Span,
    /// Optional inline message rendered next to the label arrow.
    pub message: Option<String>,
    /// Pre-resolved file path + line/col. Populated by the driver
    /// (`edda_driver::render::resolve_labels`) before the [`Diagnostic`]
    /// leaves the cascade so the CLI / MCP wire form does not need a
    /// live `SourceMap` to render file:line:col.
    pub resolved: Option<ResolvedLocation>,
}

impl Label {
    /// Construct a label with no inline message.
    pub fn new(span: Span) -> Self {
        Label { span, message: None, resolved: None }
    }

    /// Construct a label with an inline message.
    pub fn with_message(span: Span, message: impl Into<String>) -> Self {
        Label {
            span,
            message: Some(message.into()),
            resolved: None,
        }
    }

    /// Attach pre-resolved file/line/col. Returns the modified label.
    pub fn with_resolved(mut self, resolved: ResolvedLocation) -> Self {
        self.resolved = Some(resolved);
        self
    }
}

/// One row of a [`Diagnostic::counterexample`]: a binding name paired
/// with a value rendered in Edda surface syntax (`lo = 5`, not the raw
/// SMT model form `(define-fun lo () Int 5)`).
#[derive(Clone, Debug)]
pub struct CounterexampleValue {
    /// Variable / parameter name the value belongs to.
    pub name: String,
    /// The value rendered as an Edda literal.
    pub value: String,
}

impl CounterexampleValue {
    /// Construct a counterexample row.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        CounterexampleValue { name: name.into(), value: value.into() }
    }
}
