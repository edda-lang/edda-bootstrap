//! Cache-layer error type and its diagnostic mapping.
//!
//! Every fallible call into this crate returns `Result<T, CacheError>`. The
//! variants split into two families:
//!   - *Local* failures (`Io`, `ManifestParse`, `HeaderParse`) — recoverable
//!     from with retry or with user action.
//!   - *Contract* failures (`SchemaVersionMismatch`, `HashMismatch`,
//!     `StagingCorrupt`, `MarkerStuck`) — signal that the cache itself is in a
//!     state the build cannot reason about; recovery is operator-level
//!     (manual cleanup of `.edda/cache/`, toolchain upgrade, etc.).
//!
//! Errors carry enough context to produce an `edda-diag` `Diagnostic` for
//! user-facing rendering; see [`CacheError::to_diagnostic`].

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use edda_diag::{Diagnostic, DiagnosticClass, Severity};
use edda_span::Span;

use crate::hash::ArtifactHash;

/// Cache-layer error.
#[derive(Debug)]
pub enum CacheError {
    /// Filesystem operation failed. Wraps the underlying `io::Error` together
    /// with the path that produced it (the standard `io::Error` does not
    /// always carry one).
    Io {
        /// Path the operation was targeting.
        path: PathBuf,
        /// Short verb naming the failed operation: `"read"`, `"write"`,
        /// `"rename"`, `"create_dir_all"`, `"remove"`.
        op: &'static str,
        /// Underlying OS error.
        source: io::Error,
    },

    /// A TOON file (`manifest.toon`, `projects.toon`) could not be parsed.
    ManifestParse {
        /// Path of the file whose parse failed.
        path: PathBuf,
        /// Line number (1-based) within the file. `0` if not localizable.
        line: u32,
        /// Human-readable message describing the parse failure.
        message: String,
    },

    /// An artifact's `// @generated` header is missing or malformed.
    HeaderParse {
        /// Path of the artifact file.
        path: PathBuf,
        /// Human-readable message describing the failure.
        message: String,
    },

    /// Manifest declared a `schema_version` this build does not understand.
    /// v0.1 ships `1`; future versions ship a forward-migration table
    /// (`migration.md` §4).
    SchemaVersionMismatch {
        /// Path of the manifest.
        path: PathBuf,
        /// Value found in the manifest.
        found: u32,
        /// Value this build supports.
        supported: u32,
    },

    /// On-disk artifact content hashed to a value different from the value
    /// recorded in the manifest. Indicates either a corrupt cache or a
    /// version-byte change without a manifest update.
    HashMismatch {
        /// Path of the artifact file.
        path: PathBuf,
        /// Hash the manifest expected.
        expected: ArtifactHash,
        /// Hash recomputed from the on-disk bytes.
        actual: ArtifactHash,
    },

    /// Staging directory layout is inconsistent (e.g., manifest update failed
    /// after artifact renames). Recovery is rebuild-from-source per
    /// `migration.md` §3.
    StagingCorrupt {
        /// Path of the staging directory.
        path: PathBuf,
        /// Human-readable description.
        message: String,
    },

    /// GC marker file (`.gc-in-progress`) is older than the configured stuck
    /// threshold. Per `build-system.md` §7, an interrupted GC resumes on the
    /// next build; this variant signals that resume itself failed.
    MarkerStuck {
        /// Path of the marker file.
        path: PathBuf,
    },

    /// Manifest serialised to bytes whose size exceeds the per-entry sanity
    /// budget. Defence-in-depth introduced in response to a corrupt 23.6 GB
    /// `manifest.toon` that survived on disk silently until the next reader
    /// OOM'd allocating a buffer for its contents.
    /// The budget is intentionally loose — order-of-magnitude blowups, not
    /// legitimate large manifests, are what this catches.
    ManifestSizeAnomaly {
        /// Path the manifest was about to be written to.
        path: PathBuf,
        /// Number of artifacts the manifest declares.
        artifact_count: usize,
        /// Number of bytes the serialised manifest occupied.
        actual_bytes: usize,
        /// Loose upper bound the writer refuses to exceed.
        expected_max: usize,
    },
}

impl CacheError {
    /// Construct an `Io` variant from an `io::Error` and the path that
    /// produced it. Use this at every filesystem call site so the resulting
    /// error always names a path.
    pub(crate) fn io(op: &'static str, path: impl AsRef<Path>, source: io::Error) -> Self {
        CacheError::Io {
            op,
            path: path.as_ref().to_path_buf(),
            source,
        }
    }

    /// Project this error to an `edda-diag` `Diagnostic` for user-facing
    /// rendering. The resulting diagnostic carries `Span::DUMMY` because
    /// cache errors are filesystem-bound, not source-bound; the driver may
    /// attach a source span (e.g., the `import` site that triggered the
    /// read) before pushing into a `Diagnostics` take.
    pub fn to_diagnostic(&self) -> Diagnostic {
        let class = self.diagnostic_class();
        Diagnostic::new(class, Severity::Error, Span::DUMMY, self.to_string())
    }

    /// Class assignment table. Cache errors do not yet have a dedicated class
    /// in `build-system.md` §8's locked set, so they map onto the closest
    /// existing classes:
    ///   - manifest parse failures use `unknown_manifest_key` (the only
    ///     manifest-shaped class in the locked set);
    ///   - everything else is system error and falls back to
    ///     `parse_error` for now (driver may rewrite to a future
    ///     cache-specific class once the §8 set is reopened).
    fn diagnostic_class(&self) -> DiagnosticClass {
        match self {
            CacheError::ManifestParse { .. } => DiagnosticClass::UnknownManifestKey,
            _ => DiagnosticClass::ParseError,
        }
    }
}

impl fmt::Display for CacheError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CacheError::Io { op, path, source } => {
                write!(f, "{} {}: {}", op, path.display(), source)
            }
            CacheError::ManifestParse {
                path,
                line,
                message,
            } => {
                if *line == 0 {
                    write!(f, "{}: {}", path.display(), message)
                } else {
                    write!(f, "{}:{}: {}", path.display(), line, message)
                }
            }
            CacheError::HeaderParse { path, message } => {
                write!(f, "{}: artifact header: {}", path.display(), message)
            }
            CacheError::SchemaVersionMismatch {
                path,
                found,
                supported,
            } => write!(
                f,
                "{}: schema_version {} not supported by this build (expected {})",
                path.display(),
                found,
                supported
            ),
            CacheError::HashMismatch {
                path,
                expected,
                actual,
            } => write!(
                f,
                "{}: content hash mismatch: expected {}, got {}",
                path.display(),
                expected,
                actual
            ),
            CacheError::StagingCorrupt { path, message } => {
                write!(f, "{}: staging directory corrupt: {}", path.display(), message)
            }
            CacheError::MarkerStuck { path } => {
                write!(f, "{}: GC marker file is stuck", path.display())
            }
            CacheError::ManifestSizeAnomaly {
                path,
                artifact_count,
                actual_bytes,
                expected_max,
            } => write!(
                f,
                "{}: refusing to write manifest of {} bytes for {} artifacts \
                 (exceeds sanity budget of {} bytes — likely writer regression)",
                path.display(),
                actual_bytes,
                artifact_count,
                expected_max,
            ),
        }
    }
}

impl std::error::Error for CacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CacheError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
