//! Streaming observer for cascade progress.
//!
//! The cascade calls [`Reporter`] methods as work completes so callers
//! can render diagnostics, update a TUI, or forward events to the MCP
//! wire without waiting for the whole build to finish. The accumulated
//! state still lands in [`crate::outcome::Outcome`]; the reporter is
//! an *additional* hook, not the only one.

use edda_diag::Diagnostic;

use crate::outcome::Summary;

/// Streaming observer. Every method has a default no-op
/// implementation so concrete reporters override only the events they
/// care about.
pub trait Reporter: Send + Sync {
    /// The cascade is about to start work on `package`. Emitted once at
    /// entry to [`crate::cascade::run`] so that callers driving multiple
    /// packages (workspace dispatch) can identify which member is
    /// currently in flight before its summary line lands. The name is
    /// the manifest-declared package name (`manifest.package`).
    fn package_started(&self, _package: &str) {}

    /// A module just finished a cascade stage. `module` is the
    /// canonical dot-path (`my_project.parser.tokens`); `stage` names
    /// the just-completed stage from the [`crate::plan::Stage`]
    /// enumeration.
    fn module_done(&self, _module: &str, _stage: &str) {}

    /// A diagnostic was emitted by some pass. Reporters that print
    /// during the build (CLI stdout, LSP `textDocument/diagnostics`)
    /// flush here. The diagnostic is also accumulated into
    /// [`crate::outcome::Outcome::diagnostics`].
    fn diagnostic(&self, _d: &Diagnostic) {}

    /// The cascade finished and the §10 summary is ready to render.
    /// Reporters that compose progress UIs close them here.
    fn summary(&self, _s: &Summary) {}
}

/// A reporter that discards every event. Default for library / test
/// usage where the caller inspects the [`crate::outcome::Outcome`]
/// directly.
#[derive(Copy, Clone, Debug, Default)]
pub struct SilentReporter;

impl Reporter for SilentReporter {}
