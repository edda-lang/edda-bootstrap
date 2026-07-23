//! In-memory project state owned by a [`crate::Daemon`].
//!
//! Currently stores: the long-lived [`edda_driver::Driver`] (interner,
//! source map, manifest, layout, target, resolved package), plus a
//! per-file partition of the cascade's accumulated diagnostics keyed
//! by [`edda_span::FileId`], and the document-overlay store —
//! see [`crate::document`]. Future work grows this struct further (persistent
//! index, watcher state).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use edda_diag::{Diagnostic, Diagnostics};
use edda_driver::Driver;
use edda_span::FileId;
use edda_syntax::ast::File as AstFile;

use crate::document::DocumentStore;

pub(crate) struct ProjectState {
    /// Long-lived driver: holds the source map, interners, manifest,
    /// layout, target, lint config, and the
    /// [`edda_resolve::ResolvedPackage`].
    pub(crate) driver: Driver,
    /// Per-file partition of the cascade's diagnostics. The key is the
    /// `FileId` of the diagnostic's primary label.
    pub(crate) diagnostics_by_file: HashMap<FileId, Vec<Diagnostic>>,
    /// Editor-authoritative overlay store. While a path has an entry
    /// here, queries about that path return the overlay's text + AST
    /// instead of the on-disk-derived view.
    pub(crate) documents: DocumentStore,
}

impl ProjectState {
    /// Build a project state from a driver and the cascade's
    /// accumulated diagnostics. Diagnostics are consumed and
    /// partitioned by their primary-label [`FileId`]; diagnostics with
    /// a [`edda_span::Span::DUMMY`] primary are dropped because there
    /// is no file-keyed query that could ever return them.
    pub(crate) fn new(driver: Driver, diagnostics: Diagnostics) -> Self {
        let by_file = partition_by_file(diagnostics);
        Self {
            driver,
            diagnostics_by_file: by_file,
            documents: DocumentStore::new(),
        }
    }

    /// The package root the driver was initialised against.
    pub(crate) fn project_root(&self) -> &Path {
        &self.driver.package_root
    }

    /// Resolve an on-disk path to the matching [`FileId`] by scanning
    /// the resolved source graph's module entries.
    ///
    /// Returns `None` when the path is not part of the resolved graph
    /// (the file was never imported, or the cascade aborted before
    /// import-resolve registered it).
    pub(crate) fn file_id_for_path(&self, path: &Path) -> Option<FileId> {
        let resolved = self.driver.resolved.as_ref()?;
        let normalized = normalize(path);
        for entry in resolved.graph().modules() {
            if normalize(&entry.file_path) == normalized {
                return Some(entry.file_id);
            }
        }
        None
    }

    /// Resolve an on-disk path to the parsed AST for that file.
    ///
    /// This method consults the document overlay first. If `path` has an
    /// open overlay, that overlay's AST is returned (the editor is
    /// authoritative over its open files per `lsp-mapping.md` §3).
    /// Otherwise the resolved-source-graph entry is returned, falling
    /// back to `None` if the path is not registered there either.
    pub(crate) fn ast_for_path(&self, path: &Path) -> Option<Arc<AstFile>> {
        let normalized = normalize(path);
        if let Some(doc) = self.documents.get(&normalized) {
            return Some(Arc::clone(&doc.ast));
        }
        let resolved = self.driver.resolved.as_ref()?;
        for entry in resolved.graph().modules() {
            if normalize(&entry.file_path) == normalized {
                return Some(Arc::clone(&entry.ast));
            }
        }
        None
    }
}

/// Best-effort canonicalisation, re-exported for sibling modules
/// (e.g. `daemon.rs` building a `DocumentStore` key from a caller's
/// `&Path`).
pub(crate) fn normalize_path(path: &Path) -> PathBuf {
    normalize(path)
}

/// Partition a [`Diagnostics`] take into per-file buckets.
fn partition_by_file(diagnostics: Diagnostics) -> HashMap<FileId, Vec<Diagnostic>> {
    let mut by_file: HashMap<FileId, Vec<Diagnostic>> = HashMap::new();
    for diag in diagnostics.into_vec() {
        if diag.primary.span.is_dummy() {
            continue;
        }
        by_file
            .entry(diag.primary.span.file)
            .or_default()
            .push(diag);
    }
    by_file
}

/// Best-effort canonicalisation. Falls back to the raw path when the
/// filesystem rejects the probe (path doesn't exist, permission denied,
/// etc.) so unknown-file lookups still return a meaningful `None`
/// rather than panicking.
fn normalize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
