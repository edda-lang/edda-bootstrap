//! Discharge-failure error type and its `edda-diag` mapping (`error/discharge.rs`).
//!
//! When [`Solver::discharge`](crate::Solver::discharge) returns a non-`Unsat`
//! outcome, the typechecker projects the obligation + outcome into a
//! [`RefineError`] and renders the diagnostic via [`RefineError::to_diagnostic`].
//!
//! The diagnostic class is fixed: every failure routes to
//! [`DiagnosticClass::RefinementUnproven`]. The trichotomy (sat / timeout /
//! unknown) appears in the diagnostic's *tail* — counter-example, configured
//! timeout + elapsed time, or solver reason string — per
//! `docs/types/refinement-decidability.md` §8.
//!
//! Cache-style two-family split:
//!   - *Local* (`Discharge`) — the obligation failed; the user can act
//!     (tighten refinements, restate the predicate, route through
//!     `@trust` / `@unverified`).
//!   - *Contract* (`SolverInternal`) — the solver returned a malformed
//!     reply; the user cannot act and the daemon should restart the solver.
//!     v0.1 surface; the Z3 backend may grow this family.

use std::fmt;
use std::time::Duration;

use edda_diag::{Diagnostic, DiagnosticClass, Severity};
use edda_span::Span;

use crate::obligation::ObligationKind;
use crate::outcome::{ConcreteValue, Counterexample, DischargeOutcome};

//            the failure to a source location
/// Refinement-layer error. Built from a failed [`DischargeOutcome`] paired
/// with the [`Obligation`](crate::Obligation) that produced it.
#[derive(Clone, Debug)]
pub enum RefineError {
    /// SMT discharge returned a non-`Unsat` outcome. The user can act: tighten
    /// refinements, restate the predicate, route through
    /// `@trust` / `@unverified`.
    Discharge {
        /// Source position of the failing operator / clause.
        site: Span,
        /// Why this obligation arose.
        kind: ObligationKind,
        /// Pretty-rendered goal predicate.
        predicate_text: String,
        /// Failure sub-mode — drives the diagnostic tail.
        failure: DischargeFailure,
    },
    /// The SMT backend returned a malformed reply. Solver-internal bug; the
    /// daemon should restart the solver and the user cannot directly fix the
    /// underlying issue.
    SolverInternal {
        /// Source position the obligation arose from.
        site: Span,
        /// Solver name (e.g., `"z3"`).
        solver: String,
        /// Free-form description of the malformation.
        message: String,
    },
}

//            refinement-decidability.md §2's trichotomy; `Unsat` is the success
//            path and never appears here
/// Why discharge failed.
#[derive(Clone, Debug)]
pub enum DischargeFailure {
    /// Solver reported `sat`; the predicate does NOT hold.
    Sat {
        /// Concrete counter-example produced by the solver.
        counterexample: Counterexample,
    },
    /// Solver did not return within the configured timeout.
    Timeout {
        /// Per-obligation timeout that was configured for the run.
        configured: Duration,
        /// Wall-clock time elapsed when the solver was halted.
        elapsed: Duration,
    },
    /// Solver explicitly admitted it could not decide.
    Unknown {
        /// Solver-supplied reason, when available.
        reason: Option<String>,
    },
}

impl DischargeFailure {
    /// Project a [`DischargeOutcome`] into a [`DischargeFailure`]. Returns
    /// `None` for the success path ([`DischargeOutcome::Unsat`]).
    pub fn from_outcome(outcome: DischargeOutcome) -> Option<DischargeFailure> {
        match outcome {
            DischargeOutcome::Unsat { .. } => None,
            DischargeOutcome::Sat { counterexample } => {
                Some(DischargeFailure::Sat { counterexample })
            }
            DischargeOutcome::Timeout {
                configured,
                elapsed,
            } => Some(DischargeFailure::Timeout {
                configured,
                elapsed,
            }),
            DischargeOutcome::Unknown { reason } => Some(DischargeFailure::Unknown { reason }),
        }
    }
}

impl RefineError {
    /// Construct a [`RefineError::Discharge`] from a failure sub-mode.
    pub fn discharge(
        site: Span,
        kind: ObligationKind,
        predicate_text: impl Into<String>,
        failure: DischargeFailure,
    ) -> RefineError {
        RefineError::Discharge {
            site,
            kind,
            predicate_text: predicate_text.into(),
            failure,
        }
    }

    /// Source span this error is attributed to.
    pub fn site(&self) -> Span {
        match self {
            RefineError::Discharge { site, .. } => *site,
            RefineError::SolverInternal { site, .. } => *site,
        }
    }

    //            for the discharge path; the per-mode tail follows refinement-decidability.md §8
    /// Project this error to an `edda-diag` [`Diagnostic`] for user-facing
    /// rendering. The class is always [`DiagnosticClass::RefinementUnproven`];
    /// per-mode tails encode the failure sub-mode.
    pub fn to_diagnostic(&self) -> Diagnostic {
        match self {
            RefineError::Discharge {
                site,
                kind,
                predicate_text,
                failure,
            } => discharge_diagnostic(*site, kind, predicate_text, failure),
            RefineError::SolverInternal {
                site,
                solver,
                message,
            } => Diagnostic::new(
                DiagnosticClass::RefinementUnproven,
                Severity::Error,
                *site,
                format!("solver `{solver}` returned a malformed reply: {message}"),
            )
            .with_note("this is an internal solver error; please file a bug"),
        }
    }
}

// Build the canonical `Discharge` diagnostic. Split out so the orchestration
// stays under the 80-line function limit and the per-mode tail is testable in
// isolation.
fn discharge_diagnostic(
    site: Span,
    kind: &ObligationKind,
    predicate_text: &str,
    failure: &DischargeFailure,
) -> Diagnostic {
    let header = format!("{}: {predicate_text}", kind.header());
    let mut diag = Diagnostic::new(
        kind.diagnostic_class(),
        Severity::Error,
        site,
        header,
    );
    for note in failure_notes(failure) {
        diag = diag.with_note(note);
    }
    diag
}

// Compose per-mode notes per refinement-decidability.md §8's locked tails.
fn failure_notes(failure: &DischargeFailure) -> Vec<String> {
    match failure {
        DischargeFailure::Sat { counterexample } => {
            counterexample_notes(counterexample)
        }
        DischargeFailure::Timeout {
            configured,
            elapsed,
        } => vec![format!(
            "solver timed out after {:.3}s (configured limit: {:.3}s)",
            elapsed.as_secs_f64(),
            configured.as_secs_f64(),
        )],
        DischargeFailure::Unknown { reason } => {
            let mut notes = match reason {
                Some(r) => vec![format!("solver could not decide: {r}")],
                None => vec!["solver could not decide".to_string()],
            };
            notes.push(
                "consider whether this predicate lies outside the required-decidable fragment, \
                 or use `@trust(reason: \"...\")` to admit this site explicitly"
                    .to_string(),
            );
            notes
        }
    }
}

// Render the counter-example bindings as a sequence of notes — one per
// binding so the user sees each concrete value on its own line.
fn counterexample_notes(counterexample: &Counterexample) -> Vec<String> {
    if counterexample.bindings.is_empty() {
        return vec!["solver returned `sat` but no counter-example bindings".to_string()];
    }
    let mut notes = Vec::with_capacity(counterexample.bindings.len() + 1);
    notes.push("counter-example:".to_string());
    for (name, value) in &counterexample.bindings {
        notes.push(format!("  {name} = {}", render_value(value)));
    }
    notes
}

fn render_value(value: &ConcreteValue) -> String {
    match value {
        ConcreteValue::Signed(v) => v.to_string(),
        ConcreteValue::Unsigned(v) => v.to_string(),
        ConcreteValue::Bool(b) => b.to_string(),
        ConcreteValue::String(s) => s.clone(),
    }
}

impl fmt::Display for RefineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RefineError::Discharge {
                kind,
                predicate_text,
                failure,
                ..
            } => write!(f, "{}: {predicate_text} ({failure})", kind.header()),
            RefineError::SolverInternal {
                solver, message, ..
            } => write!(f, "solver `{solver}` malformed reply: {message}"),
        }
    }
}

impl fmt::Display for DischargeFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DischargeFailure::Sat { .. } => f.write_str("sat — counter-example available"),
            DischargeFailure::Timeout {
                configured,
                elapsed,
            } => write!(
                f,
                "timeout after {:.3}s (limit: {:.3}s)",
                elapsed.as_secs_f64(),
                configured.as_secs_f64()
            ),
            DischargeFailure::Unknown { reason } => match reason {
                Some(r) => write!(f, "unknown: {r}"),
                None => f.write_str("unknown"),
            },
        }
    }
}

impl std::error::Error for RefineError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outcome::ConcreteValue;

    #[test]
    fn discharge_diagnostic_uses_refinement_unproven_class() {
        let cx = Counterexample::empty();
        let err = RefineError::discharge(
            Span::DUMMY,
            ObligationKind::DivByZero,
            "den != 0",
            DischargeFailure::Sat { counterexample: cx },
        );
        let diag = err.to_diagnostic();
        assert_eq!(diag.class, DiagnosticClass::RefinementUnproven);
        assert!(diag.is_error());
    }

    #[test]
    fn sat_failure_renders_counterexample_bindings() {
        let mut cx = Counterexample::empty();
        cx.push("i", ConcreteValue::Unsigned(7));
        cx.push("xs.len()", ConcreteValue::Unsigned(5));
        let err = RefineError::discharge(
            Span::DUMMY,
            ObligationKind::SliceBound,
            "i < xs.len()",
            DischargeFailure::Sat { counterexample: cx },
        );
        let diag = err.to_diagnostic();
        let notes_joined = diag.notes.join("|");
        assert!(notes_joined.contains("i = 7"), "notes: {notes_joined}");
        assert!(
            notes_joined.contains("xs.len() = 5"),
            "notes: {notes_joined}"
        );
    }

    #[test]
    fn timeout_failure_renders_configured_and_elapsed() {
        let err = RefineError::discharge(
            Span::DUMMY,
            ObligationKind::DivByZero,
            "den != 0",
            DischargeFailure::Timeout {
                configured: Duration::from_secs(5),
                elapsed: Duration::from_millis(5_017),
            },
        );
        let diag = err.to_diagnostic();
        let joined = diag.notes.join("|");
        assert!(joined.contains("5.017"), "notes: {joined}");
        assert!(joined.contains("5.000"), "notes: {joined}");
    }

    #[test]
    fn unknown_failure_suggests_trust_annotation() {
        let err = RefineError::discharge(
            Span::DUMMY,
            ObligationKind::DivByZero,
            "den != 0",
            DischargeFailure::Unknown {
                reason: Some("NLA tactic exhausted".to_string()),
            },
        );
        let diag = err.to_diagnostic();
        let joined = diag.notes.join("|");
        assert!(joined.contains("@trust"), "notes: {joined}");
        assert!(joined.contains("NLA tactic exhausted"), "notes: {joined}");
    }
}
