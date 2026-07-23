//! In-memory document overlay — editor-authoritative source text.
//!
//! This module introduces a path-keyed store of [`OpenDocument`]s that
//! shadows the resolved-source-graph's view of any file the editor has
//! opened. While an overlay exists, queries about that file (currently
//! [`crate::query::parsed_ast_for_file`]) see the overlay's AST, not
//! the on-disk-derived AST. On `didClose` the overlay is dropped and
//! the on-disk copy resumes being the source of truth on next access
//! (`docs/tooling/lsp-mapping.md` §3).
//!
//! The public surface ([`DocumentVersion`], [`DocumentParseResult`])
//! is transport-agnostic — plain Rust types, no LSP / MCP shapes leak
//! through. The LSP server promotes its `i32` `version` field into
//! [`DocumentVersion`] before reaching in.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use edda_diag::Diagnostic;
use edda_syntax::ast::File as AstFile;

/// Editor-supplied monotonic version stamp on a document overlay.
///
/// Versions are strictly increasing per path: every successful
/// [`crate::Daemon::apply_change`] requires a `version` strictly
/// greater than the document's stored version. The LSP wrapper
/// promotes the protocol's `i32` `textDocument.version` field into
/// this type (LSP guarantees non-negative versions in practice).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DocumentVersion(pub u64);

/// Result of an [`crate::Daemon::open_document`] or
/// [`crate::Daemon::apply_change`] call.
///
/// The fresh AST is intentionally *not* carried in this struct —
/// callers route through [`crate::query::parsed_ast_for_file`] to get
/// an `Arc<File>` that's pinned in the daemon and shares the same
/// overlay storage. Parse diagnostics, by contrast, are returned
/// directly here: they are not stored in
/// [`crate::state::ProjectState::diagnostics_by_file`] because the
/// document lifecycle is editor-authoritative, not project-cascade
/// authoritative.
#[derive(Clone, Debug)]
pub struct DocumentParseResult {
    /// The version that produced this parse (echoes the caller's input).
    pub version: DocumentVersion,
    /// Parse diagnostics for the overlay's text. May be empty.
    pub diagnostics: Vec<Diagnostic>,
}

/// One open document overlay: editor text plus its parse output.
///
/// This struct is keyed by **path**, not [`edda_span::FileId`].
/// Each re-parse registers a fresh `FileId` against the driver's
/// [`edda_span::SourceMap`] (the source map is append-only — see the
/// crate-level invariant on rapid-re-edit `FileId` leakage). The
/// `FileId` is intentionally absent from this struct because the
/// editor's mental model is path-driven, and storing a stale `FileId`
/// here would let callers race the next re-parse.
pub(crate) struct OpenDocument {
    pub(crate) path: PathBuf,
    pub(crate) version: DocumentVersion,
    pub(crate) text: String,
    pub(crate) ast: Arc<AstFile>,
    pub(crate) parse_diagnostics: Vec<Diagnostic>,
}

/// Path-keyed table of open documents.
///
/// Lives on [`crate::state::ProjectState`] under the same
/// `RwLock<Option<_>>` as the rest of the project state (the
/// `daemon-state-guard` pattern).
pub(crate) struct DocumentStore {
    overlays: HashMap<PathBuf, OpenDocument>,
}

impl DocumentStore {
    /// Construct an empty store.
    pub(crate) fn new() -> Self {
        Self {
            overlays: HashMap::new(),
        }
    }

    /// `true` when no documents are open.
    #[allow(dead_code)] // reserved for diagnostics / observability
    pub(crate) fn is_empty(&self) -> bool {
        self.overlays.is_empty()
    }

    /// Borrow the open document for `key` if one exists.
    pub(crate) fn get(&self, key: &Path) -> Option<&OpenDocument> {
        self.overlays.get(key)
    }

    /// Whether `key` has an open overlay.
    pub(crate) fn contains(&self, key: &Path) -> bool {
        self.overlays.contains_key(key)
    }

    /// Insert a new document. Caller is responsible for verifying the
    /// path is not already present (the daemon surfaces
    /// `DocumentAlreadyOpen` before calling this).
    pub(crate) fn insert(&mut self, doc: OpenDocument) {
        self.overlays.insert(doc.path.clone(), doc);
    }

    /// Mutably borrow the open document for `key` if one exists.
    pub(crate) fn get_mut(&mut self, key: &Path) -> Option<&mut OpenDocument> {
        self.overlays.get_mut(key)
    }

    /// Drop the overlay for `key`. Returns whether an entry was
    /// actually removed; the daemon's `close_document` is idempotent
    /// and ignores this signal.
    pub(crate) fn remove(&mut self, key: &Path) -> bool {
        self.overlays.remove(key).is_some()
    }
}

impl Default for DocumentStore {
    fn default() -> Self {
        Self::new()
    }
}
