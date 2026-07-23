//! `textDocument/completion` handler.
//!
//! v0.1 returns keyword completions filtered against the identifier
//! prefix immediately preceding the cursor. The identifier prefix is
//! extracted from the cached document text using the negotiated
//! position encoding.

use lsp_types::{CompletionParams, CompletionResponse};

use crate::completion::{identifier_prefix, keyword_completions};
use crate::error::LspError;
use crate::position::position_to_byte;
use crate::state::LspState;
use crate::uri::uri_to_path;

/// Handle `textDocument/completion`. Returns keyword-set completions
/// filtered by the cursor's identifier prefix.
pub fn completion(
    state: &LspState,
    params: CompletionParams,
) -> Result<Option<CompletionResponse>, LspError> {
    let path = uri_to_path(&params.text_document_position.text_document.uri)?;
    let pos = params.text_document_position.position;
    let prefix = match state.cached_text(&path) {
        Some(text) => extract_prefix(&text, pos, state.encoding()),
        None => String::new(),
    };
    let items = keyword_completions(&prefix);
    Ok(Some(CompletionResponse::Array(items)))
}

/// Find the identifier prefix on the line at `pos`.
fn extract_prefix(text: &str, pos: lsp_types::Position, encoding: crate::position::PositionEncoding) -> String {
    let byte_offset = position_to_byte(text, pos, encoding);
    let clamped = byte_offset.min(text.len());
    let line_start = line_start_offset(text, clamped);
    let prefix = identifier_prefix(&text[line_start..clamped], clamped - line_start);
    prefix.to_string()
}

/// Find the byte offset where the line containing `offset` begins.
fn line_start_offset(text: &str, offset: usize) -> usize {
    let bytes = text.as_bytes();
    let mut start = offset;
    // Bounded loop: at most `offset` iterations.
    while start > 0 {
        if bytes[start - 1] == b'\n' {
            break;
        }
        start -= 1;
    }
    start
}
