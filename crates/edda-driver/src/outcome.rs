//! Driver outcome — what callers observe when a cascade finishes.
//!
//! [`Outcome`] aggregates the diagnostics produced by every pass, the
//! one-line summary (`build-system.md` §10), and the [`ExitCode`] the
//! CLI surface returns to its parent process.

use std::time::Duration;

use edda_diag::Diagnostics;

/// `build-system.md` §10 exit code.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum ExitCode {
    /// `0` — build succeeded.
    Success,
    /// `1` — at least one diagnostic was promoted to
    /// [`edda_diag::Severity::Error`].
    BuildError,
    /// `2` — system-level failure (manifest unparseable, missing file,
    /// IO failure).
    SystemError,
}

impl ExitCode {
    /// Raw integer value to pass to [`std::process::exit`].
    pub fn as_i32(self) -> i32 {
        match self {
            Self::Success => 0,
            Self::BuildError => 1,
            Self::SystemError => 2,
        }
    }

    /// Pick the worse of two exit codes — used when aggregating outcomes
    /// across workspace members so the overall result reflects the worst
    /// member's status.
    pub fn worst(self, other: ExitCode) -> ExitCode {
        match (self, other) {
            (ExitCode::SystemError, _) | (_, ExitCode::SystemError) => ExitCode::SystemError,
            (ExitCode::BuildError, _) | (_, ExitCode::BuildError) => ExitCode::BuildError,
            _ => ExitCode::Success,
        }
    }
}

/// What a driver invocation produces.
#[derive(Debug)]
pub struct Outcome {
    /// Every diagnostic emitted by any cascade stage, in push order.
    /// The CLI renders them; the daemon forwards them on the MCP wire.
    pub diagnostics: Diagnostics,
    /// Pre-computed fields for the §10 one-line summary. Rendering is
    /// done by [`crate::summary::render`] so the daemon can re-format.
    pub summary: Summary,
    /// `build-system.md` §10 exit code.
    pub exit_code: ExitCode,
}

impl Outcome {
    /// Zero-state outcome: empty diagnostics, zero summary, `Success`.
    /// Used as the seed when aggregating across workspace members.
    pub fn empty() -> Self {
        Self {
            diagnostics: Diagnostics::new(),
            summary: Summary::default(),
            exit_code: ExitCode::Success,
        }
    }

    /// Construct a success outcome. The caller's `diags` has already been
    /// populated; the Mímir verb driver moves it into the returned `Outcome`
    /// via [`run_mimir`].
    pub fn success(_diags: &mut Diagnostics) -> Self {
        Self::empty()
    }

    /// Construct a build-error outcome. The caller's `diags` has already been
    /// populated; the Mímir verb driver moves it into the returned `Outcome`
    /// via [`run_mimir`].
    pub fn build_error(_diags: &mut Diagnostics) -> Self {
        let mut out = Self::empty();
        out.exit_code = ExitCode::BuildError;
        out
    }

    /// Combine two outcomes — used by the workspace dispatcher to fold
    /// per-member outcomes into the overall result.
    pub fn merge(mut self, other: Outcome) -> Self {
        for diag in other.diagnostics.into_vec() {
            self.diagnostics.push(diag);
        }
        self.summary.modules_total =
            self.summary.modules_total.saturating_add(other.summary.modules_total);
        self.summary.modules_compiled =
            self.summary.modules_compiled.saturating_add(other.summary.modules_compiled);
        self.summary.artifacts_cached =
            self.summary.artifacts_cached.saturating_add(other.summary.artifacts_cached);
        self.summary.artifacts_generated = self
            .summary
            .artifacts_generated
            .saturating_add(other.summary.artifacts_generated);
        self.summary.elapsed += other.summary.elapsed;
        self.exit_code = self.exit_code.worst(other.exit_code);
        self
    }
}

/// `build-system.md` §10 summary fields.
///
/// Filled in by the cascade as each stage runs; the artifact fields
/// reflect the codegen pass's generated / cached counts.
#[derive(Copy, Clone, Debug, Default)]
pub struct Summary {
    /// Total modules in the resolved source graph.
    pub modules_total: usize,
    /// Modules that completed every cascade stage the plan ran.
    pub modules_compiled: usize,
    /// Codegen artifacts served from the cache.
    pub artifacts_cached: usize,
    /// Codegen artifacts produced fresh.
    pub artifacts_generated: usize,
    /// Wall-clock time the cascade took.
    pub elapsed: Duration,
}
