//! Server-side LSP capabilities advertised during `initialize`.
//!
//! Centralises the capability set so the request handlers and the
//! lifecycle handshake never drift. Every entry here corresponds to a
//! method the server actually implements; nothing is advertised that
//! routes to the "method not supported" fallback.

use lsp_types::{
    CompletionOptions, OneOf, PositionEncodingKind, SemanticTokensFullOptions,
    SemanticTokensLegend, SemanticTokensOptions, SemanticTokensServerCapabilities,
    ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind,
    TextDocumentSyncOptions, WorkDoneProgressOptions, SaveOptions, TextDocumentSyncSaveOptions,
};

use crate::position::PositionEncoding;
use crate::semtokens::TOKEN_TYPES;

/// Construct the [`ServerCapabilities`] this server advertises.
///
/// `encoding` reflects the result of the `positionEncoding` negotiation —
/// the server tells the client which unit it will use for `Position`
/// fields on every subsequent message.
pub fn server_capabilities(encoding: PositionEncoding) -> ServerCapabilities {
    ServerCapabilities {
        position_encoding: Some(encoding.to_lsp()),
        text_document_sync: Some(text_document_sync()),
        completion_provider: Some(completion_options()),
        hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
        semantic_tokens_provider: Some(semantic_tokens_capability()),
        code_action_provider: Some(lsp_types::CodeActionProviderCapability::Simple(true)),
        ..Default::default()
    }
}

/// `textDocumentSync` capability: open / change / close / save notifications
/// with full-text change mode.
fn text_document_sync() -> TextDocumentSyncCapability {
    TextDocumentSyncCapability::Options(TextDocumentSyncOptions {
        open_close: Some(true),
        change: Some(TextDocumentSyncKind::FULL),
        will_save: None,
        will_save_wait_until: None,
        save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
            include_text: Some(false),
        })),
    })
}

/// `completionProvider` capability: triggered manually or on `.`, no
/// resolve method, no commit characters.
fn completion_options() -> CompletionOptions {
    CompletionOptions {
        resolve_provider: Some(false),
        trigger_characters: Some(vec![".".to_string()]),
        all_commit_characters: None,
        work_done_progress_options: WorkDoneProgressOptions::default(),
        completion_item: None,
    }
}

/// `semanticTokensProvider` capability: full-document tokens only (no
/// range / delta in v0.1).
fn semantic_tokens_capability() -> SemanticTokensServerCapabilities {
    SemanticTokensServerCapabilities::SemanticTokensOptions(SemanticTokensOptions {
        work_done_progress_options: WorkDoneProgressOptions::default(),
        legend: SemanticTokensLegend {
            token_types: TOKEN_TYPES.to_vec(),
            token_modifiers: Vec::new(),
        },
        range: Some(false),
        full: Some(SemanticTokensFullOptions::Bool(true)),
    })
}

/// Pick the position encoding to negotiate with the client.
///
/// The LSP spec says: clients advertise `general.positionEncodings` as an
/// ordered list of supported encodings; the server picks one. We prefer
/// UTF-8 because [`edda_span::BytePos`] is UTF-8 byte-based — no
/// re-encoding work per position. If the client doesn't list UTF-8, we
/// honour the LSP default of UTF-16.
pub fn negotiate_encoding(advertised: Option<&[PositionEncodingKind]>) -> PositionEncoding {
    let Some(list) = advertised else {
        return PositionEncoding::Utf16;
    };
    // Bounded loop: one iteration per advertised encoding.
    for kind in list {
        if *kind == PositionEncodingKind::UTF8 {
            return PositionEncoding::Utf8;
        }
    }
    PositionEncoding::Utf16
}

// Suppress unused import warning when `OneOf` is needed by callers of the
// capability builder later — kept here so adding a `definitionProvider`
// (or similar single-or-options capability) is a one-line edit.
#[allow(dead_code)]
fn _silence_oneof_warning() -> Option<OneOf<bool, ()>> {
    None
}
