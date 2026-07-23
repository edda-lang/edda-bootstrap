//! The [`Diagnostic`] data carrier and its consume-builder chaining API.

use edda_span::Span;

use crate::class::DiagnosticClass;
use crate::severity::Severity;

use super::label::{CounterexampleValue, Label};

/// A single diagnostic, ready to be pushed into a [`Diagnostics`] take. The
/// renderer (in `edda-driver` / `edda-cli`) owns the surface format
/// defined in 06-tooling.md §6; this type carries only the data.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    /// Locked class from the §6.2 table.
    pub class: DiagnosticClass,
    /// Effective severity after `lints` escalation.
    pub severity: Severity,
    /// Top-line message; the renderer prints it on the header line.
    pub message: String,
    /// The label the diagnostic points at first.
    pub primary: Label,
    /// Additional labels (e.g. "first defined here", "expected type came from").
    pub secondary: Vec<Label>,
    /// Free-form footer lines printed below the labelled source.
    pub notes: Vec<String>,
    /// 06-tooling.md §6.3 — the fully-elaborated expression that failed.
    /// `None` when the diagnostic has no corresponding elaborated form
    /// (e.g. parse errors fire before elaboration).
    pub canonical_form: Option<String>,
    /// 06-tooling.md §6.3 — chain of in-scope predicates the discharge
    /// context assembled, one per line. Empty for non-refinement /
    /// non-effect diagnostics.
    pub obligation_trace: Vec<String>,
    /// 06-tooling.md §6.3 — concrete Edda-surface counterexample when
    /// the SMT discharge returned `sat`. Empty for diagnostics with no
    /// counterexample.
    pub counterexample: Vec<CounterexampleValue>,
}

impl Diagnostic {
    /// Build a diagnostic with the given class, effective severity, primary
    /// span, and header message. Use the chaining methods to attach labels
    /// and notes.
    pub fn new(
        class: DiagnosticClass,
        severity: Severity,
        span: Span,
        message: impl Into<String>,
    ) -> Self {
        Diagnostic {
            class,
            severity,
            message: message.into(),
            primary: Label::new(span),
            secondary: Vec::new(),
            notes: Vec::new(),
            canonical_form: None,
            obligation_trace: Vec::new(),
            counterexample: Vec::new(),
        }
    }

    /// Replace the primary label's inline message.
    pub fn with_primary_message(mut self, message: impl Into<String>) -> Self {
        self.primary.message = Some(message.into());
        self
    }

    /// Append a secondary label.
    pub fn with_label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.secondary.push(Label::with_message(span, message));
        self
    }

    /// Append a free-form footer note.
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// Set the fully-elaborated expression that failed (06-tooling.md §6.3).
    pub fn with_canonical_form(mut self, form: impl Into<String>) -> Self {
        self.canonical_form = Some(form.into());
        self
    }

    /// Append one entry to the obligation trace (06-tooling.md §6.3).
    /// Each entry is rendered on its own line.
    pub fn with_obligation_step(mut self, step: impl Into<String>) -> Self {
        self.obligation_trace.push(step.into());
        self
    }

    /// Set the obligation trace (06-tooling.md §6.3), replacing any
    /// existing entries.
    pub fn with_obligation_trace<I, S>(mut self, steps: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.obligation_trace = steps.into_iter().map(Into::into).collect();
        self
    }

    /// Append one counterexample binding (06-tooling.md §6.3).
    /// `value` is rendered in Edda surface syntax (e.g. `"5"`, `"true"`).
    pub fn with_counterexample_value(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.counterexample.push(CounterexampleValue::new(name, value));
        self
    }

    /// `true` if this diagnostic carries [`Severity::Error`].
    #[inline]
    pub fn is_error(&self) -> bool {
        self.severity.is_error()
    }
}
