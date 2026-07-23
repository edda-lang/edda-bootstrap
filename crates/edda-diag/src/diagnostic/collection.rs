//! The append-only [`Diagnostics`] take with per-severity counters.

use crate::severity::Severity;

use super::diag::Diagnostic;

/// Append-only diagnostics take with per-severity counters. The driver
/// holds one per build invocation; passes write into it, the driver reads
/// it for rendering and exit-code selection.
#[derive(Clone, Default, Debug)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
    error_count: u32,
    warn_count: u32,
    info_count: u32,
}

impl Diagnostics {
    /// Construct an empty take.
    pub fn new() -> Self {
        Diagnostics::default()
    }

    /// Append a diagnostic. Updates the counter matching its severity.
    pub fn push(&mut self, d: Diagnostic) {
        match d.severity {
            Severity::Error => self.error_count = self.error_count.saturating_add(1),
            Severity::Warn => self.warn_count = self.warn_count.saturating_add(1),
            Severity::Info => self.info_count = self.info_count.saturating_add(1),
        }
        self.items.push(d);
    }

    /// `true` if any diagnostic with [`Severity::Error`] has been pushed.
    #[inline]
    pub fn has_errors(&self) -> bool {
        self.error_count > 0
    }

    /// Count of pushed diagnostics with [`Severity::Error`].
    #[inline]
    pub fn error_count(&self) -> u32 {
        self.error_count
    }

    /// Count of pushed diagnostics with [`Severity::Warn`].
    #[inline]
    pub fn warn_count(&self) -> u32 {
        self.warn_count
    }

    /// Count of pushed diagnostics with [`Severity::Info`].
    #[inline]
    pub fn info_count(&self) -> u32 {
        self.info_count
    }

    /// Total number of pushed diagnostics.
    #[inline]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// `true` if no diagnostics have been pushed.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Drop diagnostics at indices `>= len`, restoring the take to the
    /// length it had at the matching snapshot point. Used by the driver's
    /// two-pass cascade to discard pass-1 typecheck output before
    /// re-running typecheck against the post-codegen expanded source set.
    pub fn truncate(&mut self, len: usize) {
        if len >= self.items.len() {
            return;
        }
        for d in &self.items[len..] {
            match d.severity {
                Severity::Error => self.error_count = self.error_count.saturating_sub(1),
                Severity::Warn => self.warn_count = self.warn_count.saturating_sub(1),
                Severity::Info => self.info_count = self.info_count.saturating_sub(1),
            }
        }
        self.items.truncate(len);
    }

    /// Iterate the diagnostics in push order.
    pub fn iter(&self) -> std::slice::Iter<'_, Diagnostic> {
        self.items.iter()
    }

    /// Iterate the diagnostics in push order, mutably. Used by the
    /// driver's render pass to enrich each label with a
    /// pre-resolved file/line/col snapshot before the take leaves the
    /// cascade. Counter fields are *not* updated through this iterator —
    /// the post-condition is that severity stays as-pushed.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, Diagnostic> {
        self.items.iter_mut()
    }

    /// Consume the take and return the underlying `Vec`.
    pub fn into_vec(self) -> Vec<Diagnostic> {
        self.items
    }
}
