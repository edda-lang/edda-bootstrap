//! Document-synchronization handlers: `didOpen`, `didChange`, `didClose`,
//! `didSave`.
//!
//! Each forwards the editor's view of a file into the daemon's overlay
//! store ([`edda_daemon::Daemon::open_document`] /
//! [`edda_daemon::Daemon::apply_change`] /
//! [`edda_daemon::Daemon::close_document`]). After every successful
//! transition the diagnostics for the affected file are pushed via a
//! `textDocument/publishDiagnostics` notification.

use std::path::Path;

use lsp_server::{Connection, Message, Notification};
use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, PublishDiagnosticsParams, Uri,
    notification::PublishDiagnostics,
};

use edda_daemon::DocumentVersion;

use crate::diagnostics::diagnostic_to_lsp;
use crate::error::LspError;
use crate::state::LspState;
use crate::uri::uri_to_path;

/// Handle `textDocument/didOpen`. Opens an overlay in the daemon and
/// publishes the initial parse diagnostics for the file.
pub fn did_open(
    state: &LspState,
    conn: &Connection,
    params: DidOpenTextDocumentParams,
) -> Result<(), LspError> {
    let path = uri_to_path(&params.text_document.uri)?;
    let version = promote_version(params.text_document.version);
    let text = params.text_document.text;
    let result = state
        .daemon()
        .open_document(&path, version, text.clone())?;
    state.record_version(path.clone(), version);
    state.cache_text(path.clone(), text);
    publish_diagnostics(
        state,
        conn,
        &params.text_document.uri,
        result.diagnostics.as_slice(),
        Some(version.0 as i32),
    );
    Ok(())
}

/// Handle `textDocument/didChange`. Replaces the overlay's text and
/// publishes the fresh parse diagnostics.
pub fn did_change(
    state: &LspState,
    conn: &Connection,
    params: DidChangeTextDocumentParams,
) -> Result<(), LspError> {
    let path = uri_to_path(&params.text_document.uri)?;
    let version = promote_version(params.text_document.version);
    let text = extract_full_text(&params)?;
    let result = state
        .daemon()
        .apply_change(&path, version, text.clone())?;
    state.record_version(path.clone(), version);
    state.cache_text(path.clone(), text);
    publish_diagnostics(
        state,
        conn,
        &params.text_document.uri,
        result.diagnostics.as_slice(),
        Some(version.0 as i32),
    );
    Ok(())
}

/// Handle `textDocument/didClose`. Drops the overlay and clears the
/// editor's diagnostic display for the URI.
pub fn did_close(
    state: &LspState,
    conn: &Connection,
    params: DidCloseTextDocumentParams,
) -> Result<(), LspError> {
    let path = uri_to_path(&params.text_document.uri)?;
    state.daemon().close_document(&path)?;
    state.forget_version(&path);
    state.drop_text(&path);
    publish_diagnostics(state, conn, &params.text_document.uri, &[], None);
    Ok(())
}

/// Handle `textDocument/didSave`. Currently a no-op beyond logging.
pub fn did_save(
    _state: &LspState,
    _conn: &Connection,
    _params: DidSaveTextDocumentParams,
) -> Result<(), LspError> {
    Ok(())
}

/// Extract the full post-edit text from a `didChange` payload.
fn extract_full_text(params: &DidChangeTextDocumentParams) -> Result<String, LspError> {
    let last = params.content_changes.last().ok_or_else(|| {
        LspError::InvalidRequest("didChange has no content_changes entry".to_string())
    })?;
    Ok(last.text.clone())
}

/// Promote a wire `i32` version to the daemon's `DocumentVersion`.
fn promote_version(wire: i32) -> DocumentVersion {
    DocumentVersion(wire.max(0) as u64)
}

/// Push a `textDocument/publishDiagnostics` notification.
fn publish_diagnostics(
    state: &LspState,
    conn: &Connection,
    uri: &Uri,
    diags: &[edda_diag::Diagnostic],
    version: Option<i32>,
) {
    let encoding = state.encoding();
    let conversion = state.daemon().with_source_map(|source_map| {
        let mut out = Vec::with_capacity(diags.len());
        // Bounded loop: one iteration per diagnostic.
        for d in diags {
            out.push(diagnostic_to_lsp(d, source_map, uri, encoding));
        }
        out
    });
    let lsp_diags = match conversion {
        Ok(v) => v,
        Err(_) => return,
    };
    let params = PublishDiagnosticsParams {
        uri: uri.clone(),
        diagnostics: lsp_diags,
        version,
    };
    let notification = Notification::new(
        <PublishDiagnostics as lsp_types::notification::Notification>::METHOD.to_string(),
        params,
    );
    if conn.sender.send(Message::Notification(notification)).is_err() {
        // Channel closed: the connection is shutting down; nothing to do.
    }
}

// Silence unused import: this re-exports the `Path` so per-file helpers
// can take `&Path` without re-importing the path module everywhere.
#[allow(dead_code)]
fn _silence_path_import(p: &Path) -> &Path {
    p
}
