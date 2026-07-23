//! System-level driver failure.
//!
//! [`DriverError`] covers failures that prevent the cascade from
//! starting at all (missing manifest, missing source root, bad
//! target). They map to exit code 2 per `build-system.md` §10.
//!
//! Build-level failures (parse errors, typecheck failures, refinement
//! discharge failures, …) do **not** become [`DriverError`]s. They
//! flow through [`edda_diag::Diagnostics`] and end up in
//! [`crate::outcome::Outcome::diagnostics`] with
//! [`crate::outcome::ExitCode::BuildError`].

use std::fmt;
use std::io;
use std::path::PathBuf;

/// System-level driver failure.
#[derive(Debug)]
pub enum DriverError {
    /// `package.toml` could not be read or parsed. The driver pushed
    /// the underlying parse diagnostic into the diagnostics take
    /// before returning; rendering them is the CLI / daemon's job.
    ManifestLoad {
        /// The manifest path the driver attempted to load.
        path: PathBuf,
    },
    /// `<package_root>/src/` is missing or unreadable. v0.1 only
    /// compiles modules under `src/`; an empty package has no `src/`
    /// at all and this variant fires.
    SourceRootMissing {
        /// The path that was probed for `src/`.
        path: PathBuf,
        /// Underlying I/O error from the filesystem probe.
        source: io::Error,
    },
    /// No target was specified by either `manifest.build.default_target`
    /// or `--target`. The cascade cannot ground `target_has(...)`
    /// without an active target so it refuses to start.
    TargetUnspecified,
    /// `--target <triple>` did not parse against the locked
    /// `build-system.md` §9 grammar.
    InvalidTriple {
        /// The triple spelling that failed to parse.
        spelling: String,
    },
}

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ManifestLoad { path } => {
                write!(f, "failed to load manifest at `{}`", path.display())
            }
            Self::SourceRootMissing { path, source } => {
                write!(
                    f,
                    "source root `{}` is unreadable: {source}",
                    path.display()
                )
            }
            Self::TargetUnspecified => f.write_str(
                "no target specified: set `build.default_target` in package.toml or pass --target",
            ),
            Self::InvalidTriple { spelling } => {
                write!(f, "invalid target triple `{spelling}`")
            }
        }
    }
}

impl std::error::Error for DriverError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SourceRootMissing { source, .. } => Some(source),
            _ => None,
        }
    }
}
