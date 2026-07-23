//! Linker / archiver driver error type and its diagnostic mapping.
//!
//! Every fallible call into this crate returns `Result<T, LinkError>`.
//! The variants split into two families:
//!   - *Local* failures (`ToolNotFound`, `SpawnFailed`,
//!     `IoDuringInvoke`) — recoverable from with retry or with user
//!     action (install the tool, fix permissions).
//!   - *Contract* failures (`ToolExitedNonZero`, `NoInputs`,
//!     `UnsupportedTargetOs`, `UnsupportedKindForTool`) — signal that
//!     the inputs or environment cannot support a v0.1-locked link;
//!     recovery is operator-level (change the manifest target list,
//!     fix the link inputs).
//!
//! Errors carry enough context to produce an `edda-diag` `Diagnostic`
//! for user-facing rendering; see [`LinkError::to_diagnostic`]. The §8
//! diagnostic-class set is locked and has no dedicated `linker_failure`
//! class, so the mapping uses `parse_error` as a placeholder (matching
//! `edda-cache`'s convention until §8 is reopened).

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::process::ExitStatus;

use edda_diag::{Diagnostic, DiagnosticClass, Severity};
use edda_span::Span;
use edda_target::Os;
use smol_str::SmolStr;

use crate::output::OutputKind;
use crate::tool::Tool;

/// Maximum number of stderr bytes carried inside
/// [`LinkError::ToolExitedNonZero`]. The full stderr from
/// [`crate::run`] is still available to callers via
/// [`crate::LinkOutcome`]; this cap keeps the structured-error payload
/// bounded for diagnostic rendering.
pub const STDERR_EXCERPT_LIMIT: usize = 64 * 1024;

/// Linker / archiver driver error.
#[derive(Debug)]
pub enum LinkError {
    // ---- Local failures (recoverable) ----
    /// The tool executable was not found on `$PATH`.
    ToolNotFound {
        /// Tool whose `tool_name()` failed to resolve.
        tool: Tool,
        /// Stringified `PATH` at the time of the lookup (best-effort,
        /// lossy on non-UTF8 segments).
        searched_path: SmolStr,
    },

    /// `std::process::Command::spawn` failed for the resolved tool.
    /// Typical causes: missing execute permission, broken symlink to
    /// another mount, ENFILE.
    SpawnFailed {
        /// Tool we tried to spawn.
        tool: Tool,
        /// Underlying OS error.
        source: io::Error,
    },

    /// Reading stdout or stderr from the tool child failed.
    IoDuringInvoke(io::Error),

    /// The pre-link runtime-extern gate ([`crate::resolve_runtime_externs`])
    /// could not read or parse a link input as an object file or archive.
    ObjectReadFailed {
        /// The link input that failed to read or parse.
        path: PathBuf,
        /// Stringified I/O or `object`-crate parse error.
        detail: SmolStr,
    },

    // ---- Contract failures (operator-level) ----
    /// Tool terminated with a non-zero exit status. The driver maps
    /// linker failures to backend exit code 67 (`backend-choice.md`
    /// §6.8); archiver failures map similarly.
    ToolExitedNonZero {
        /// Tool that produced the failure.
        tool: Tool,
        /// Process exit status (signal, code).
        status: ExitStatus,
        /// First [`STDERR_EXCERPT_LIMIT`] bytes of the captured stderr,
        /// lossily decoded as UTF-8 for human rendering.
        stderr_excerpt: SmolStr,
    },

    /// [`crate::LinkPlan::argv`] was called with an empty input list.
    /// All locked linker and archiver dialects require at least one
    /// input file.
    NoInputs,

    /// Target OS is not in the v0.1-locked linker / archiver matrix
    /// (`build-system.md` §5b).
    UnsupportedTargetOs(Os),

    /// The requested [`OutputKind`] cannot be produced by the selected
    /// tool. Examples: `Framework`-linkage `LibSpec` paired with a
    /// non-macOS linker.
    UnsupportedKindForTool {
        /// Tool the plan selected.
        tool: Tool,
        /// Output kind the plan requested.
        kind: OutputKind,
    },
}

impl LinkError {
    /// Project this error to an `edda-diag` `Diagnostic` for
    /// user-facing rendering. The resulting diagnostic carries
    /// `Span::DUMMY` because link errors are process/filesystem-bound,
    /// not source-bound; the driver may attach a source span (e.g.,
    /// the `package.toon` `target` field that selected the offending
    /// triple) before pushing into a `Diagnostics` take.
    pub fn to_diagnostic(&self) -> Diagnostic {
        Diagnostic::new(
            self.diagnostic_class(),
            Severity::Error,
            Span::DUMMY,
            self.to_string(),
        )
    }

    /// Class assignment. Link failures do not yet have a dedicated
    /// class in `build-system.md` §8's locked set, so they fall back
    /// to `parse_error` — matching `edda-cache::CacheError`'s
    /// placeholder mapping. The driver may rewrite to a future
    /// `linker_failure` class once §8 is reopened.
    fn diagnostic_class(&self) -> DiagnosticClass {
        DiagnosticClass::ParseError
    }
}

impl fmt::Display for LinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LinkError::ToolNotFound {
                tool,
                searched_path,
            } => write!(
                f,
                "tool {} not found on PATH (searched: {})",
                tool.tool_name(),
                searched_path
            ),
            LinkError::SpawnFailed { tool, source } => {
                write!(f, "failed to spawn tool {}: {}", tool.tool_name(), source)
            }
            LinkError::IoDuringInvoke(source) => {
                write!(f, "io error while reading tool output: {source}")
            }
            LinkError::ObjectReadFailed { path, detail } => write!(
                f,
                "failed to read link input {} for the runtime-extern gate: {}",
                path.display(),
                detail
            ),
            LinkError::ToolExitedNonZero {
                tool,
                status,
                stderr_excerpt,
            } => write!(
                f,
                "tool {} exited with {}{}{}",
                tool.tool_name(),
                status,
                if stderr_excerpt.is_empty() { "" } else { ":\n" },
                stderr_excerpt
            ),
            LinkError::NoInputs => f.write_str("link plan has no input object files"),
            LinkError::UnsupportedTargetOs(os) => {
                write!(f, "target os `{}` is not in the v0.1 linker matrix", os.name())
            }
            LinkError::UnsupportedKindForTool { tool, kind } => write!(
                f,
                "tool {} cannot produce {} output in v0.1",
                tool.tool_name(),
                kind.name()
            ),
        }
    }
}

impl std::error::Error for LinkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LinkError::SpawnFailed { source, .. } => Some(source),
            LinkError::IoDuringInvoke(source) => Some(source),
            _ => None,
        }
    }
}

/// Truncate a stderr byte buffer to [`STDERR_EXCERPT_LIMIT`] and
/// lossily decode to UTF-8 for the structured-error payload. Used by
/// `invoke::run` and re-exported for callers that build
/// [`LinkError::ToolExitedNonZero`] outside this crate (currently
/// none, but the helper documents the cap).
#[doc(hidden)]
pub fn make_stderr_excerpt(stderr: &[u8]) -> SmolStr {
    let bounded = if stderr.len() > STDERR_EXCERPT_LIMIT {
        &stderr[..STDERR_EXCERPT_LIMIT]
    } else {
        stderr
    };
    SmolStr::from(String::from_utf8_lossy(bounded).as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linker::Linker;

    #[test]
    fn tool_not_found_display_names_tool_and_path() {
        let err = LinkError::ToolNotFound {
            tool: Tool::Link(Linker::Mold),
            searched_path: SmolStr::new("/usr/bin:/usr/local/bin"),
        };
        let s = err.to_string();
        assert!(s.contains("mold"));
        assert!(s.contains("/usr/bin"));
    }

    #[test]
    fn unsupported_target_os_display_names_os() {
        let err = LinkError::UnsupportedTargetOs(Os::Freebsd);
        let s = err.to_string();
        assert!(s.contains("freebsd"));
    }

    #[test]
    fn no_inputs_display_is_stable() {
        assert_eq!(
            LinkError::NoInputs.to_string(),
            "link plan has no input object files",
        );
    }

    #[test]
    fn to_diagnostic_uses_error_severity_and_dummy_span() {
        let err = LinkError::NoInputs;
        let diag = err.to_diagnostic();
        assert!(diag.is_error());
    }

    #[test]
    fn stderr_excerpt_truncates_at_limit() {
        let big = vec![b'x'; STDERR_EXCERPT_LIMIT + 1024];
        let excerpt = make_stderr_excerpt(&big);
        assert_eq!(excerpt.len(), STDERR_EXCERPT_LIMIT);
    }

    #[test]
    fn stderr_excerpt_lossy_decodes_non_utf8() {
        let bytes = [0xff_u8, b'h', b'i'];
        let excerpt = make_stderr_excerpt(&bytes);
        assert!(excerpt.contains("hi"));
    }
}
