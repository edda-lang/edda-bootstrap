//! The [`Daemon`] entry point and its open / close lifecycle.
//!
//! The daemon currently hosts at most one [`crate::state::ProjectState`] at a time
//! behind a [`parking_lot::RwLock`]. Multi-project hosting (a
//! `HashMap<ProjectId, ProjectState>`) is deferred to a future version
//! that introduces `ProjectId`.
//!
//! The document-overlay lifecycle ([`Daemon::open_document`],
//! [`Daemon::apply_change`], [`Daemon::close_document`]) is also hosted here — see
//! [`crate::document`] for the data model.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use edda_diag::Diagnostics;
use edda_driver::{
    BuildOptions, Command, Driver, ExitCode, Outcome, Reporter, RunPlan, Stage, run_cascade,
};
use edda_syntax::ast::File as AstFile;
use parking_lot::RwLock;

use crate::document::{DocumentParseResult, DocumentVersion, OpenDocument};
use crate::error::DaemonError;
use crate::reporter::DaemonReporter;
use crate::state::{normalize_path, ProjectState};

/// Long-lived compiler-as-service handle.
///
/// One daemon hosts at most one open project at a time. Open
/// the project with [`Daemon::open_project`]; close it with
/// [`Daemon::close_project`]. Reads (project root, diagnostics, AST)
/// take the inner read lock; writes (open / close) take the write lock.
///
/// The handle is `Send + Sync`: a single daemon can be shared across
/// threads. The daemon is sync only — no `tokio`, no `async`. Background
/// work and subscription channels land in a later version through
/// [`std::thread`] and `crossbeam_channel`.
pub struct Daemon {
    state: RwLock<Option<ProjectState>>,
}

impl Daemon {
    /// Construct an empty daemon. No project is open until
    /// [`Daemon::open_project`] is called.
    pub fn new() -> Self {
        Self {
            state: RwLock::new(None),
        }
    }

    /// Open a project: initialise an [`edda_driver::Driver`] for the
    /// manifest in `options`, run the parse + import-resolve prefix of
    /// the cascade, and stash the resulting state.
    ///
    /// # Errors
    ///
    /// - [`DaemonError::ProjectAlreadyOpen`] — a project is already
    ///   open on this daemon.
    /// - [`DaemonError::DriverInit`] — the underlying
    ///   [`edda_driver::Driver::initialize`] failed (missing or
    ///   unparseable manifest, missing `src/`, unspecified or invalid
    ///   target).
    /// - [`DaemonError::CascadeFailed`] — the cascade reported
    ///   [`ExitCode::BuildError`] or [`ExitCode::SystemError`]; the
    ///   accumulated diagnostics are carried in the variant.
    pub fn open_project(&self, options: BuildOptions) -> Result<(), DaemonError> {
        if self.state.read().is_some() {
            return Err(DaemonError::ProjectAlreadyOpen);
        }

        let project = build_project_state(options)?;

        let mut guard = self.state.write();
        // Re-check under the write lock to close a TOCTOU window
        // between the read-only is_some check above and acquiring the
        // write lock.
        if guard.is_some() {
            return Err(DaemonError::ProjectAlreadyOpen);
        }
        *guard = Some(project);
        Ok(())
    }

    /// Close the open project, dropping the [`edda_driver::Driver`]
    /// and all cached state. Idempotent: closing an already-empty
    /// daemon is a no-op.
    pub fn close_project(&self) {
        *self.state.write() = None;
    }

    /// The package root the open project was loaded from, or `None` if
    /// no project is currently open.
    pub fn project_root(&self) -> Option<PathBuf> {
        self.state
            .read()
            .as_ref()
            .map(|state| state.project_root().to_path_buf())
    }

    /// Internal: take a read lock on the project state for queries.
    pub(crate) fn with_state<R>(
        &self,
        f: impl FnOnce(&ProjectState) -> R,
    ) -> Result<R, DaemonError> {
        let guard = self.state.read();
        let state = guard.as_ref().ok_or(DaemonError::NoProjectOpen)?;
        Ok(f(state))
    }

    /// Borrow the daemon's source map for the duration of a closure.
    ///
    /// The source map is shared across every parse / typecheck / codegen
    /// job; transport layers (LSP / MCP) need read access to convert
    /// [`edda_span::Span`] values into wire-format ranges. The closure
    /// runs under the daemon's read lock — keep its body short and
    /// allocation-free where possible.
    ///
    /// # Errors
    ///
    /// - [`DaemonError::NoProjectOpen`] — no project is loaded.
    pub fn with_source_map<R>(
        &self,
        f: impl FnOnce(&edda_span::SourceMap) -> R,
    ) -> Result<R, DaemonError> {
        self.with_state(|state| f(&state.driver.source_map))
    }

    /// Open a document overlay for `path`.
    ///
    /// Registers `text` as the editor-authoritative source for `path`,
    /// re-parses it under the open project's interner / source map,
    /// and stores the resulting [`OpenDocument`] in the project state.
    /// Returns the initial parse diagnostics; the fresh AST is
    /// reachable through [`crate::query::parsed_ast_for_file`] (which
    /// overlay-shadows the on-disk view).
    ///
    /// # Errors
    ///
    /// - [`DaemonError::NoProjectOpen`] — no project is loaded.
    /// - [`DaemonError::DocumentAlreadyOpen`] — `path` already has an
    ///   open overlay; close it first or use
    ///   [`Daemon::apply_change`] to replace its contents.
    pub fn open_document(
        &self,
        path: &Path,
        version: DocumentVersion,
        text: String,
    ) -> Result<DocumentParseResult, DaemonError> {
        let key = normalize_path(path);
        let mut guard = self.state.write();
        let state = guard.as_mut().ok_or(DaemonError::NoProjectOpen)?;
        if state.documents.contains(&key) {
            return Err(DaemonError::DocumentAlreadyOpen { path: key });
        }
        let (ast, diagnostics) = parse_overlay_text(&state.driver, &key, &text);
        let doc = OpenDocument {
            path: key,
            version,
            text,
            ast,
            parse_diagnostics: diagnostics.clone(),
        };
        state.documents.insert(doc);
        Ok(DocumentParseResult {
            version,
            diagnostics,
        })
    }

    /// Replace an open document's text and re-parse.
    ///
    /// This method takes the full post-edit text rather than LSP-style
    /// incremental ranges — the LSP wrapper composes deltas before
    /// calling.
    ///
    /// # Errors
    ///
    /// - [`DaemonError::NoProjectOpen`] — no project is loaded.
    /// - [`DaemonError::DocumentNotOpen`] — `path` has no open
    ///   overlay; [`Daemon::open_document`] first.
    /// - [`DaemonError::DocumentVersionStale`] — `version` is not
    ///   strictly greater than the stored version.
    pub fn apply_change(
        &self,
        path: &Path,
        version: DocumentVersion,
        new_text: String,
    ) -> Result<DocumentParseResult, DaemonError> {
        let key = normalize_path(path);
        let mut guard = self.state.write();
        let state = guard.as_mut().ok_or(DaemonError::NoProjectOpen)?;
        // Borrow check: validate-then-parse-then-mutate so we don't
        // hold a `&mut OpenDocument` across the immutable `&Driver`
        // borrow that the parse path needs.
        let stored_version = match state.documents.get(&key) {
            Some(doc) => doc.version,
            None => return Err(DaemonError::DocumentNotOpen { path: key }),
        };
        if version <= stored_version {
            return Err(DaemonError::DocumentVersionStale {
                path: key,
                stored: stored_version,
                attempted: version,
            });
        }
        let (ast, diagnostics) = parse_overlay_text(&state.driver, &key, &new_text);
        let returned_diags = diagnostics.clone();
        let doc = state.documents.get_mut(&key).expect(
            "overlay present at start of apply_change cannot disappear under exclusive write lock",
        );
        doc.version = version;
        doc.text = new_text;
        doc.ast = ast;
        doc.parse_diagnostics = diagnostics;
        Ok(DocumentParseResult {
            version,
            diagnostics: returned_diags,
        })
    }

    /// Drop the in-memory overlay for `path`.
    ///
    /// Per `lsp-mapping.md` §3, after `close_document` returns the
    /// on-disk copy is the source of truth for subsequent queries
    /// about `path`. The watcher (not yet implemented) and the cascade rerun
    /// (`didSave`, also deferred) are separate concerns.
    ///
    /// # Errors
    ///
    /// - [`DaemonError::NoProjectOpen`] — no project is loaded.
    pub fn close_document(&self, path: &Path) -> Result<(), DaemonError> {
        let key = normalize_path(path);
        let mut guard = self.state.write();
        let state = guard.as_mut().ok_or(DaemonError::NoProjectOpen)?;
        let _removed = state.documents.remove(&key);
        Ok(())
    }
}

impl Default for Daemon {
    fn default() -> Self {
        Self::new()
    }
}

/// Lex + parse `text` against the open project's interner / source
/// map / lint config, producing the `Arc<File>` and parse diagnostics
/// for one document overlay.
fn parse_overlay_text(
    driver: &Driver,
    path: &Path,
    text: &str,
) -> (Arc<AstFile>, Vec<edda_diag::Diagnostic>) {
    let file_id = driver.source_map.add_file(path.to_path_buf(), text.to_string());
    let mut diags = Diagnostics::new();
    let tokens = edda_syntax::lex(
        text,
        file_id,
        &driver.interner,
        &mut diags,
        &driver.lint_cfg,
    );
    let file = edda_syntax::parse_file(&tokens, &driver.interner, &mut diags, &driver.lint_cfg);
    (Arc::new(file), diags.into_vec())
}

/// Initialise the driver, run the import-resolution cascade prefix, and
/// lift the outcome into a [`ProjectState`].
fn build_project_state(options: BuildOptions) -> Result<ProjectState, DaemonError> {
    let reporter: Arc<dyn Reporter> = Arc::new(DaemonReporter);
    let mut driver = Driver::initialize(options, reporter)?;

    let plan = RunPlan {
        command: Command::Check,
        stop_after: Stage::ImportResolve,
        full_materialization: false,
    };
    let outcome = run_cascade(&mut driver, &plan);
    let Outcome {
        diagnostics,
        summary: _,
        exit_code,
    } = outcome;

    match exit_code {
        ExitCode::Success => Ok(ProjectState::new(driver, diagnostics)),
        ExitCode::BuildError | ExitCode::SystemError => Err(DaemonError::CascadeFailed {
            exit_code,
            diagnostics,
        }),
    }
}
