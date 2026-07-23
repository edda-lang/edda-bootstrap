//! Daemon-level error type covering project open / close failures.
//!
//! [`DaemonError`] separates two failure modes — the driver failed to
//! start (system error: missing manifest, missing src/, bad target)
//! versus the cascade ran and the build failed (carries the cascade's
//! diagnostics). Query-time errors (no project open) live here too.

use std::fmt;
use std::path::PathBuf;

use edda_diag::Diagnostics;
use edda_driver::{DriverError, ExitCode};

use crate::document::DocumentVersion;

/// Daemon-surface error. Every public entry point that can fail
/// returns `Result<_, DaemonError>`.
#[derive(Debug)]
pub enum DaemonError {
    /// The underlying [`edda_driver::Driver`] could not be initialised:
    /// missing or unparseable manifest, missing `src/`, unspecified or
    /// invalid target. These are system-level failures (`build-system.md`
    /// §10 exit code 2) and the cascade never ran.
    DriverInit(DriverError),
    /// The cascade ran but reported a non-success exit code. The
    /// driver's accumulated [`Diagnostics`] take is forwarded so callers
    /// can inspect the failure without re-running the build.
    CascadeFailed {
        /// The cascade's `build-system.md` §10 exit code — always
        /// [`ExitCode::BuildError`] or [`ExitCode::SystemError`].
        exit_code: ExitCode,
        /// Every diagnostic the cascade emitted, in push order.
        diagnostics: Diagnostics,
    },
    /// [`crate::Daemon::open_project`] was called while a project is
    /// already loaded. The daemon is single-project; the caller must
    /// [`crate::Daemon::close_project`] before reopening.
    ProjectAlreadyOpen,
    /// A query method was called before [`crate::Daemon::open_project`]
    /// succeeded, or after [`crate::Daemon::close_project`].
    NoProjectOpen,
    /// [`crate::Daemon::open_document`] was called for a path that
    /// already has an open overlay. The caller must
    /// [`crate::Daemon::close_document`] first or
    /// [`crate::Daemon::apply_change`] to replace its contents.
    DocumentAlreadyOpen {
        /// The path the caller tried to (re-)open.
        path: PathBuf,
    },
    /// [`crate::Daemon::apply_change`] was called for a path with no
    /// open overlay. The caller must [`crate::Daemon::open_document`]
    /// first.
    DocumentNotOpen {
        /// The path the caller tried to change.
        path: PathBuf,
    },
    /// [`crate::Daemon::apply_change`] was called with a
    /// non-strictly-increasing version. Editors guarantee monotonic
    /// versions; receiving a stale one means messages arrived out of
    /// order (or were duplicated) and the change is dropped.
    DocumentVersionStale {
        /// The path whose change was rejected.
        path: PathBuf,
        /// The version currently stored in the overlay.
        stored: DocumentVersion,
        /// The version the caller attempted to apply.
        attempted: DocumentVersion,
    },
}

impl fmt::Display for DaemonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DriverInit(err) => write!(f, "driver initialization failed: {err}"),
            Self::CascadeFailed {
                exit_code,
                diagnostics,
            } => write!(
                f,
                "cascade failed with exit code {} ({} diagnostics, {} errors)",
                exit_code.as_i32(),
                diagnostics.len(),
                diagnostics.error_count(),
            ),
            Self::ProjectAlreadyOpen => f.write_str("a project is already open in this daemon"),
            Self::NoProjectOpen => f.write_str("no project is open in this daemon"),
            Self::DocumentAlreadyOpen { path } => write!(
                f,
                "document overlay already open for {}",
                path.display()
            ),
            Self::DocumentNotOpen { path } => write!(
                f,
                "no document overlay open for {}",
                path.display()
            ),
            Self::DocumentVersionStale {
                path,
                stored,
                attempted,
            } => write!(
                f,
                "stale document version for {}: stored {} is not strictly less than attempted {}",
                path.display(),
                stored.0,
                attempted.0,
            ),
        }
    }
}

impl std::error::Error for DaemonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::DriverInit(err) => Some(err),
            _ => None,
        }
    }
}

impl From<DriverError> for DaemonError {
    fn from(err: DriverError) -> Self {
        Self::DriverInit(err)
    }
}
