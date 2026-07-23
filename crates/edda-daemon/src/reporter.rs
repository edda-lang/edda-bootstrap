//! Cascade observer threaded into [`edda_driver::Driver::initialize`].
//!
//! Silent-by-default — the driver's cascade aggregates every
//! diagnostic into [`edda_driver::Outcome::diagnostics`] and the daemon
//! reads from there. The reporter exists only because the driver's
//! [`Reporter`] trait is non-optional on `Driver::initialize`. A future
//! streaming-events design can plug richer behaviour into
//! the same trait without forcing a public-surface change here.

use edda_diag::Diagnostic;
use edda_driver::{Reporter, Summary};

/// No-op [`Reporter`] used while the daemon runs the cascade.
///
/// Identical in behaviour to [`edda_driver::SilentReporter`] but lives
/// in this crate so a future version can grow event-stream plumbing without
/// touching the driver.
#[derive(Copy, Clone, Debug, Default)]
pub(crate) struct DaemonReporter;

impl Reporter for DaemonReporter {
    fn module_done(&self, _module: &str, _stage: &str) {}
    fn diagnostic(&self, _d: &Diagnostic) {}
    fn summary(&self, _s: &Summary) {}
}
