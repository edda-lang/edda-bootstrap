//! Per-session LSP state: daemon handle, negotiated encoding, document
//! versions.
//!
//! The state object is created once after `initialize` succeeds and
//! shared between every request / notification handler. It owns the
//! [`edda_daemon::Daemon`] and the post-negotiation [`PositionEncoding`].
//! Document text is *not* mirrored here — the daemon's overlay store is
//! the single source of truth — but we keep a small `HashMap` of the
//! client-supplied versions so `didChange` can validate monotonicity
//! before the daemon does.

use std::collections::HashMap;
use std::path::PathBuf;

use parking_lot::Mutex;

use edda_daemon::{Daemon, DocumentVersion};

use crate::position::PositionEncoding;

/// Session-wide state shared across LSP request handlers.
pub struct LspState {
    /// Underlying daemon. Holds the open project, source map, document
    /// overlays, and the cascade outputs.
    daemon: Daemon,
    /// Negotiated `positionEncoding`. Fixed for the session.
    encoding: PositionEncoding,
    /// Last-seen document version per open path. Used to short-circuit
    /// duplicate `didChange` notifications.
    versions: Mutex<HashMap<PathBuf, DocumentVersion>>,
    /// Per-document cached text for fast position conversion. The
    /// daemon owns the canonical text (inside its `SourceMap`); this
    /// cache is a write-through copy so handlers don't need to acquire
    /// the daemon's read lock for every position-to-byte translation.
    text_cache: Mutex<HashMap<PathBuf, String>>,
}

impl LspState {
    /// Construct a state wrapping a fresh daemon and the negotiated
    /// encoding.
    pub fn new(daemon: Daemon, encoding: PositionEncoding) -> Self {
        Self {
            daemon,
            encoding,
            versions: Mutex::new(HashMap::new()),
            text_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Borrow the daemon for read or write access.
    pub fn daemon(&self) -> &Daemon {
        &self.daemon
    }

    /// The session's negotiated position encoding.
    pub fn encoding(&self) -> PositionEncoding {
        self.encoding
    }

    /// Read the last-recorded version for `path`.
    pub fn version(&self, path: &std::path::Path) -> Option<DocumentVersion> {
        self.versions.lock().get(path).copied()
    }

    /// Record a new authoritative version for `path`. Replaces any prior
    /// entry.
    pub fn record_version(&self, path: PathBuf, version: DocumentVersion) {
        self.versions.lock().insert(path, version);
    }

    /// Forget the recorded version for `path` (called from `didClose`).
    pub fn forget_version(&self, path: &std::path::Path) {
        self.versions.lock().remove(path);
    }

    /// Cache the current text for `path` (called after open / change).
    pub fn cache_text(&self, path: PathBuf, text: String) {
        self.text_cache.lock().insert(path, text);
    }

    /// Drop the cached text for `path` (called on `didClose`).
    pub fn drop_text(&self, path: &std::path::Path) {
        self.text_cache.lock().remove(path);
    }

    /// Read the cached text for `path`, returning a clone so the lock
    /// isn't held across the caller's body.
    pub fn cached_text(&self, path: &std::path::Path) -> Option<String> {
        self.text_cache.lock().get(path).cloned()
    }
}
