//! Solver-agnostic discharge interface.
//!
//! Per `docs/bootstrap/host-choice.md` §2, three SMT solvers are conformant:
//! Z3, CVC5, Yices. The v0.1 default is Z3 via `z3.rs`
//! (`docs/bootstrap/v0.1-scope.md` §3.5). The [`Solver`] trait lets refine's
//! callers pick a backend: `Z3Backend` is the production implementation;
//! [`StubSolver`] lets downstream crates compile and run smoke tests
//! against the discharge surface without the Z3 dependency.
//!
//! Implementations must respect the per-obligation timeout passed in the
//! call. The refinement-decidability.md §2 default is 5 seconds; the caller
//! (typechecker driver) honours `package.toml`'s `[refine]` block when it
//! lands.

use std::time::Duration;

use crate::obligation::Obligation;
use crate::outcome::DischargeOutcome;

//            a concrete outcome or with `Timeout` once the budget is exhausted
//          host-choice.md §2's conformant solver set
/// Discharge a refinement obligation using an SMT backend.
///
/// Implementations:
///   - Translate [`Obligation::goal`] and [`Obligation::context`] into the
///     backend's input language.
///   - Ask the backend whether `context ∧ ¬goal` is satisfiable.
///   - Return [`DischargeOutcome::Unsat`] (with a [`ProofCertificate`])
///     iff the backend reports `unsat`.
///   - Surface `sat` / `timeout` / `unknown` per the four-variant outcome.
///
/// Implementations must be `Send + Sync` so the typechecker can share a
/// single solver across worker threads in the daemon's parallel typecheck
/// pass.
///
/// [`ProofCertificate`]: crate::ProofCertificate
pub trait Solver: Send + Sync {
    /// Attempt to discharge `obligation` within `timeout`.
    fn discharge(&self, obligation: &Obligation, timeout: Duration) -> DischargeOutcome;
}

//            production callers replace this with the Z3 backend
//          without the Z3 backend
/// Trivial [`Solver`] that returns [`DischargeOutcome::Unknown`] for every
/// obligation.
///
/// Used by the typechecker's smoke tests and by downstream crates
/// that want to exercise the discharge plumbing without the Z3 backend
/// wired in. Discharge against [`StubSolver`] always fails, which means
/// programs with refinements do not currently compile against it — that's
/// expected until the caller wires the Z3 backend.
#[derive(Default, Debug)]
pub struct StubSolver;

impl StubSolver {
    /// Construct a fresh stub solver.
    pub fn new() -> StubSolver {
        StubSolver
    }
}

impl Solver for StubSolver {
    fn discharge(&self, _obligation: &Obligation, _timeout: Duration) -> DischargeOutcome {
        DischargeOutcome::Unknown {
            reason: Some(
                "stub solver: no SMT backend wired".to_string(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obligation::{Obligation, ObligationKind};
    use crate::predicate::{IntLit, Predicate};
    use crate::sort::{IntSort, IntWidth};
    use edda_span::Span;

    #[test]
    fn stub_solver_always_returns_unknown() {
        let solver = StubSolver::new();
        let i32_sort = IntSort::sized(IntWidth::W32, true);
        let goal = Predicate::BoolLit(true);
        let obligation = Obligation::new(
            goal,
            vec![Predicate::IntLit(IntLit::signed(0, i32_sort))],
            Span::DUMMY,
            ObligationKind::DivByZero,
            "",
        );
        let outcome = solver.discharge(&obligation, Duration::from_secs(5));
        match outcome {
            DischargeOutcome::Unknown { reason: Some(r) } => {
                assert!(r.contains("stub"), "reason: {r}");
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
    }
}
