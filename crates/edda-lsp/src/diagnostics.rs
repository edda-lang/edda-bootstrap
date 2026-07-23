//! Conversion from [`edda_diag::Diagnostic`] to LSP [`lsp_types::Diagnostic`].
//!
//! The mapping per `docs/tooling/lsp-mapping.md` (spec intent):
//!
//! - [`edda_diag::DiagnosticClass`] name → LSP `code` (string)
//! - [`edda_diag::Severity`] → LSP `DiagnosticSeverity`
//! - primary [`edda_diag::Label`].span → LSP `range`
//! - primary [`edda_diag::Label`].message (if any) → appended to the
//!   diagnostic's header `message`
//! - secondary labels → LSP `relatedInformation` entries
//! - notes → joined with newlines and appended to the message

use std::path::Path;

use edda_diag::{Diagnostic as EddaDiagnostic, Severity};
use edda_span::SourceMap;
use lsp_types::{
    DiagnosticRelatedInformation, DiagnosticSeverity, Location, NumberOrString, Uri,
};

use crate::position::{PositionEncoding, span_to_range};
use crate::uri::path_to_uri;

/// Project `edda-diag` severity onto the LSP severity enum.
pub fn severity_to_lsp(severity: Severity) -> DiagnosticSeverity {
    match severity {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warn => DiagnosticSeverity::WARNING,
        Severity::Info => DiagnosticSeverity::INFORMATION,
    }
}

/// Convert an Edda diagnostic to an LSP diagnostic.
///
/// `primary_uri` is the URI of the file the LSP request is keyed against;
/// it is reused as the `relatedInformation.location.uri` for any secondary
/// label whose span sits in the same file.
pub fn diagnostic_to_lsp(
    diag: &EddaDiagnostic,
    source_map: &SourceMap,
    primary_uri: &Uri,
    encoding: PositionEncoding,
) -> lsp_types::Diagnostic {
    let range = span_to_range(source_map, diag.primary.span, encoding);
    let mut message = compose_message(diag);
    append_notes(&mut message, &diag.notes);
    let related = build_related(diag, source_map, primary_uri, encoding);
    lsp_types::Diagnostic {
        range,
        severity: Some(severity_to_lsp(diag.severity)),
        code: Some(NumberOrString::String(diag.class.name().to_string())),
        code_description: None,
        source: Some("edda".to_string()),
        message,
        related_information: if related.is_empty() { None } else { Some(related) },
        tags: None,
        data: None,
    }
}

/// Build the LSP `message` field from the diagnostic's header and the
/// primary label's inline note (if any).
fn compose_message(diag: &EddaDiagnostic) -> String {
    match diag.primary.message.as_ref() {
        Some(inline) if !inline.is_empty() => format!("{}: {}", diag.message, inline),
        _ => diag.message.clone(),
    }
}

/// Append `notes` to `message` with one `note:` line per entry.
fn append_notes(message: &mut String, notes: &[String]) {
    // Bounded loop: one iteration per note.
    for note in notes {
        message.push_str("\n  note: ");
        message.push_str(note);
    }
}

/// Build the LSP `relatedInformation` array from the diagnostic's
/// secondary labels.
fn build_related(
    diag: &EddaDiagnostic,
    source_map: &SourceMap,
    primary_uri: &Uri,
    encoding: PositionEncoding,
) -> Vec<DiagnosticRelatedInformation> {
    let mut out = Vec::with_capacity(diag.secondary.len());
    // Bounded loop: one iteration per secondary label.
    for label in &diag.secondary {
        if label.span.is_dummy() {
            continue;
        }
        let range = span_to_range(source_map, label.span, encoding);
        let uri = if label.span.file == diag.primary.span.file {
            primary_uri.clone()
        } else {
            match secondary_uri(source_map, label.span.file) {
                Some(u) => u,
                None => primary_uri.clone(),
            }
        };
        let location = Location { uri, range };
        let message = label
            .message
            .clone()
            .unwrap_or_else(|| String::from("related location"));
        out.push(DiagnosticRelatedInformation { location, message });
    }
    out
}

/// Resolve a [`edda_span::FileId`] to a `file:` URI via the source map.
fn secondary_uri(source_map: &SourceMap, file: edda_span::FileId) -> Option<Uri> {
    let name = source_map.file_name(file);
    let path: &Path = name.as_ref();
    path_to_uri(path).ok()
}
