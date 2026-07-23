//! Discharge outcome — the four-element result enum from
//! `docs/types/refinement-decidability.md` §2.
//!
//! `Unsat` is the only success outcome. `Sat`, `Timeout`, and `Unknown` each
//! carry a distinct diagnostic tail per §8's per-mode locks:
//!   - `Sat` — counter-example bindings;
//!   - `Timeout` — configured timeout + elapsed wall-clock time;
//!   - `Unknown` — solver's reason string (when available).
//!
//! The split between `Timeout` and `Unknown` is load-bearing. `Timeout`
//! signals "more time might have helped," while `Unknown` signals "the solver
//! explicitly admitted it couldn't decide" — the suggested user remediations
//! differ (raise the timeout vs. consider `@trust`).

use std::time::Duration;

use smol_str::SmolStr;

use crate::certificate::ProofCertificate;

//            set requires a spec edit
/// Result of a single discharge attempt.
#[derive(Clone, Debug)]
pub enum DischargeOutcome {
    /// The goal's negation is unsatisfiable; the predicate holds. Discharge
    /// succeeds and emits a [`ProofCertificate`] for write-only persistence
    /// per refinement-decidability.md §9.
    Unsat {
        /// Solver witness — see [`ProofCertificate`].
        certificate: ProofCertificate,
    },
    /// The goal's negation is satisfiable; the predicate does NOT hold.
    /// Discharge fails; the diagnostic carries the counter-example.
    Sat {
        /// Concrete bindings that satisfy `context ∧ ¬goal`.
        counterexample: Counterexample,
    },
    /// The solver did not return within the configured timeout. Discharge
    /// fails; the diagnostic names the timeout and the elapsed time.
    Timeout {
        /// Per-obligation timeout that was configured for the run.
        configured: Duration,
        /// Wall-clock time elapsed when the solver was halted.
        elapsed: Duration,
    },
    /// The solver returned a result indicating it could not decide (theory's
    /// decision procedure did not apply, quantifier instantiation exhausted,
    /// etc.). Discharge fails; the diagnostic suggests `@trust`.
    Unknown {
        /// Solver-supplied reason string, when available.
        reason: Option<String>,
    },
}

impl DischargeOutcome {
    /// `true` when the outcome counts as discharge success (`Unsat`).
    pub fn is_success(&self) -> bool {
        matches!(self, DischargeOutcome::Unsat { .. })
    }
}

//            the context — the diagnostic layer renders only those the user sees
/// Concrete counter-example produced by a `sat` discharge.
#[derive(Clone, Debug, Default)]
pub struct Counterexample {
    /// Variable name → concrete value. Order matches the typechecker's
    /// declaration order so the diagnostic renders parameters in their
    /// signature position.
    pub bindings: Vec<(SmolStr, ConcreteValue)>,
}

impl Counterexample {
    /// Construct an empty counter-example.
    pub fn empty() -> Counterexample {
        Counterexample {
            bindings: Vec::new(),
        }
    }

    /// Append a binding.
    pub fn push(&mut self, name: impl Into<SmolStr>, value: ConcreteValue) {
        self.bindings.push((name.into(), value));
    }
}

//            cleanly to `i128` / `u128` / `bool` — record literals, tuple literals,
//            and sum constructors render as strings the user can re-paste verbatim
/// Concrete value carried by a counter-example binding.
#[derive(Clone, Debug)]
pub enum ConcreteValue {
    /// Signed integer value.
    Signed(i128),
    /// Unsigned integer value (kept distinct from `Signed` so u128 max is lossless).
    Unsigned(u128),
    /// Boolean.
    Bool(bool),
    /// Rendered string for values that don't decompose into the variants above
    /// (record literals, tuple literals, sum constructors). The string is the
    /// solver-produced source-form rendering — re-pastable into a refinement.
    String(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn is_success_only_for_unsat() {
        let unsat = DischargeOutcome::Unsat {
            certificate: ProofCertificate::placeholder(),
        };
        assert!(unsat.is_success());

        let sat = DischargeOutcome::Sat {
            counterexample: Counterexample::empty(),
        };
        assert!(!sat.is_success());

        let timeout = DischargeOutcome::Timeout {
            configured: Duration::from_secs(5),
            elapsed: Duration::from_secs(5),
        };
        assert!(!timeout.is_success());

        let unknown = DischargeOutcome::Unknown { reason: None };
        assert!(!unknown.is_success());
    }

    #[test]
    fn counterexample_round_trip_basic() {
        let mut cx = Counterexample::empty();
        cx.push("i", ConcreteValue::Unsigned(7));
        cx.push("xs.len()", ConcreteValue::Unsigned(5));
        assert_eq!(cx.bindings.len(), 2);
        assert_eq!(cx.bindings[0].0.as_str(), "i");
        assert!(matches!(cx.bindings[0].1, ConcreteValue::Unsigned(7)));
    }
}
