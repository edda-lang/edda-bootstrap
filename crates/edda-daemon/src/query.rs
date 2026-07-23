//! Read-only query surface over a [`crate::Daemon`]'s open project.
//!
//! This module ships two queries: per-file diagnostics and the parsed AST
//! for a file. Both take an on-disk `&Path` and return owned clones —
//! the daemon's locked state is released before the caller sees the
//! result. The surface is transport-agnostic; the LSP and MCP crates
//! shape these returns into their wire formats.
//!
//! [`parsed_ast_for_file`] is overlay-aware: if `path` has
//! an open document (see [`crate::Daemon::open_document`]), the
//! overlay's AST is returned in place of the resolved-source-graph's.
//! [`diagnostics_for_file`] is unchanged — document parse diagnostics
//! are returned by `open_document` / `apply_change` directly and do
//! *not* enter [`crate::state::ProjectState::diagnostics_by_file`].

use std::path::Path;
use std::sync::Arc;

use edda_diag::Diagnostic;
use edda_syntax::ast::File as AstFile;

use crate::daemon::Daemon;
use crate::error::DaemonError;

/// Diagnostics whose primary label points at `path`.
///
/// # Errors
///
/// - [`DaemonError::NoProjectOpen`] — no project is currently loaded.
///
/// # Behaviour
///
/// Returns an empty `Vec` when `path` is registered with the resolved
/// source graph but is clean, and also when `path` is unknown to the
/// graph — the LSP routinely queries files outside the package (the
/// canonical example is editing a file in a sibling directory that the
/// manifest does not yet reference).
pub fn diagnostics_for_file(
    daemon: &Daemon,
    path: &Path,
) -> Result<Vec<Diagnostic>, DaemonError> {
    daemon.with_state(|state| match state.file_id_for_path(path) {
        Some(file_id) => state
            .diagnostics_by_file
            .get(&file_id)
            .cloned()
            .unwrap_or_default(),
        None => Vec::new(),
    })
}

/// Parsed AST for the file at `path`.
///
/// # Errors
///
/// - [`DaemonError::NoProjectOpen`] — no project is currently loaded.
///
/// # Behaviour
///
/// Resolution order:
///
/// 1. If `path` has an open document overlay
///    ([`crate::Daemon::open_document`]), the overlay's AST is
///    returned. This is the LSP-authoritative view per
///    `lsp-mapping.md` §3.
/// 2. Otherwise the resolved-source-graph entry for `path` is
///    returned.
/// 3. If `path` is in neither, returns `None` — the LSP routinely
///    queries files outside the package.
pub fn parsed_ast_for_file(
    daemon: &Daemon,
    path: &Path,
) -> Result<Option<Arc<AstFile>>, DaemonError> {
    daemon.with_state(|state| state.ast_for_path(path))
}
