//! Wire form of an [`edda_diag::Diagnostic`] for transport on the
//! MCP wire.
//!
//! Per `mcp-protocol.md` §2's diagnostic shape: `class`, `severity`,
//! `message`, `position`, `target`, `suggestions`. The Edda-side
//! [`edda_diag::Diagnostic`] carries spans (a `FileId` plus byte
//! offsets); the wire form materialises those to a file path plus a
//! 1-based line/column position via [`edda_span::SourceMap`].

use serde::{Deserialize, Serialize};

use edda_diag::{Diagnostic, Severity};
use edda_span::{LineCol, SourceMap, Span};

/// Wire form of a single source position.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WirePosition {
    /// File path, as rendered by [`SourceMap::file_name`].
    pub file: String,
    /// 1-based line number.
    pub line: u32,
    /// 1-based column number, in bytes from line start (LSP-compatible).
    pub col: u32,
    /// 0-based byte offset of the span start within the file. Always
    /// present so AI clients have a canonical, locale-free anchor.
    pub byte_lo: u32,
    /// 0-based byte offset of the span end (exclusive).
    pub byte_hi: u32,
}

/// Wire form of an [`edda_diag::Diagnostic`].
///
/// The renderer (`edda-driver` / `edda-cli`) owns the human-readable
/// surface format from `build-system.md` §8; this is the structured
/// JSON form an LLM or IDE client consumes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WireDiagnostic {
    /// Canonical class name from `edda-diag` (`parse_error`,
    /// `typecheck_error`, etc.).
    pub class: String,
    /// `info` | `warn` | `error`.
    pub severity: String,
    /// Header line (the message the renderer prints at the top of the
    /// diagnostic).
    pub message: String,
    /// Primary label's position. Absent when the diagnostic was emitted
    /// against [`edda_span::Span::DUMMY`] (no source location).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<WirePosition>,
    /// Optional inline message on the primary label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_message: Option<String>,
    /// Secondary labels, in push order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secondary: Vec<WireLabel>,
    /// Free-form footer notes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Wire form of a secondary label.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WireLabel {
    /// Position of the label, or `None` if the label's span is dummy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<WirePosition>,
    /// Inline message attached to the label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Project a [`Span`] to its wire form.
///
/// Returns `None` for dummy spans (no file id was issued). Callers
/// should preserve the diagnostic but omit `position` from the wire
/// payload.
pub fn position_for_span(map: &SourceMap, span: Span) -> Option<WirePosition> {
    if span.is_dummy() {
        return None;
    }
    let LineCol { line, col } = map.byte_to_linecol(span.file, span.lo);
    let file_path = map.file_name(span.file).display().to_string();
    Some(WirePosition {
        file: file_path,
        line,
        col,
        byte_lo: span.lo.to_u32(),
        byte_hi: span.hi.to_u32(),
    })
}

/// Render a [`Severity`] to its lowercase wire name.
pub const fn severity_name(s: Severity) -> &'static str {
    match s {
        Severity::Info => "info",
        Severity::Warn => "warn",
        Severity::Error => "error",
    }
}

/// Project an [`edda_diag::Diagnostic`] to its wire form.
pub fn to_wire(map: &SourceMap, diag: &Diagnostic) -> WireDiagnostic {
    let position = position_for_span(map, diag.primary.span);
    let secondary = diag
        .secondary
        .iter()
        .map(|label| WireLabel {
            position: position_for_span(map, label.span),
            message: label.message.clone(),
        })
        .collect();
    WireDiagnostic {
        class: diag.class.name().to_string(),
        severity: severity_name(diag.severity).to_string(),
        message: diag.message.clone(),
        position,
        primary_message: diag.primary.message.clone(),
        secondary,
        notes: diag.notes.clone(),
    }
}

/// Project every diagnostic in `diagnostics` to its wire form, in
/// push order.
pub fn to_wire_many(map: &SourceMap, diagnostics: &[Diagnostic]) -> Vec<WireDiagnostic> {
    diagnostics.iter().map(|d| to_wire(map, d)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use edda_diag::{Diagnostic, DiagnosticClass, Severity};
    use edda_span::{BytePos, SourceMap, Span};
    use std::path::PathBuf;

    #[test]
    fn dummy_span_drops_position() {
        let d = Diagnostic::new(
            DiagnosticClass::ParseError,
            Severity::Error,
            Span::DUMMY,
            "x",
        );
        let map = SourceMap::new();
        let w = to_wire(&map, &d);
        assert!(w.position.is_none());
        assert_eq!(w.class, "parse_error");
        assert_eq!(w.severity, "error");
    }

    #[test]
    fn real_span_renders_one_based_linecol() {
        let map = SourceMap::new();
        let file = map.add_file(PathBuf::from("a.ea"), "let\n  x = 1".to_string());
        let span = Span::new(file, BytePos(4), BytePos(7));
        let d = Diagnostic::new(
            DiagnosticClass::TypecheckError,
            Severity::Warn,
            span,
            "wrong",
        );
        let w = to_wire(&map, &d);
        let pos = w.position.expect("position present for real span");
        assert_eq!(pos.line, 2);
        assert_eq!(pos.col, 1);
        assert_eq!(pos.byte_lo, 4);
        assert_eq!(pos.byte_hi, 7);
        assert!(pos.file.ends_with("a.ea"));
        assert_eq!(w.severity, "warn");
    }
}
