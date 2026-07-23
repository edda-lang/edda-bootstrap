//! Z3 backend integration tests: LIA, Bool, Array, timeout projection.
//!
//! Exercises the discharge path against the required-decidable fragment from
//! `docs/types/refinement-decidability.md` §2 minus the record / sum / cast /
//! annotation-routing extensions (covered elsewhere). Includes the
//! timeout-routing coverage and a smoke check on [`TranslationError`]'s
//! `Display` impl.

mod common;

use std::time::Duration;

use common::{i32_sort, lit_i32, obligation, var};
use edda_refine::{
    CmpOp, DischargeOutcome, IntSort, IntWidth, ObligationKind, Predicate, Solver, Sort,
    TranslationError, Z3Backend,
};

#[test]
fn div_by_zero_discharges_from_where_clause() {
    let backend = Z3Backend::new();
    // Goal: den != 0. Context: den != 0 (from a `where` clause).
    let den = var("den", Sort::Int(i32_sort()));
    let goal = Predicate::cmp(CmpOp::Ne, den.clone(), lit_i32(0));
    let ctx_pred = Predicate::cmp(CmpOp::Ne, den, lit_i32(0));
    let o = obligation(goal, vec![ctx_pred], ObligationKind::DivByZero);
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    assert!(outcome.is_success(), "outcome: {outcome:?}");
}

#[test]
fn div_by_zero_discharges_from_stronger_constraint() {
    // Goal: den != 0. Context: den > 0 (which implies den != 0).
    let backend = Z3Backend::new();
    let den = var("den", Sort::Int(i32_sort()));
    let goal = Predicate::cmp(CmpOp::Ne, den.clone(), lit_i32(0));
    let ctx_pred = Predicate::cmp(CmpOp::Gt, den, lit_i32(0));
    let o = obligation(goal, vec![ctx_pred], ObligationKind::DivByZero);
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    assert!(outcome.is_success(), "outcome: {outcome:?}");
}

#[test]
fn slice_bound_discharges_from_matching_requires() {
    // Goal: i < xs.len(). Context: i < xs.len().
    let backend = Z3Backend::new();
    let xs = var(
        "xs",
        Sort::slice(Sort::Int(IntSort::sized(IntWidth::W64, true))),
    );
    let i = var("i", Sort::usize());
    let goal = Predicate::cmp(CmpOp::Lt, i.clone(), Predicate::slice_len(xs.clone()));
    let ctx_pred = Predicate::cmp(CmpOp::Lt, i, Predicate::slice_len(xs));
    let o = obligation(goal, vec![ctx_pred], ObligationKind::SliceBound);
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    assert!(outcome.is_success(), "outcome: {outcome:?}");
}

#[test]
fn slice_bound_fails_with_empty_context() {
    // Goal: i < xs.len(). Context: empty. Sat: i = 0, xs.len() = 0 (or similar).
    let backend = Z3Backend::new();
    let xs = var(
        "xs",
        Sort::slice(Sort::Int(IntSort::sized(IntWidth::W64, true))),
    );
    let i = var("i", Sort::usize());
    let goal = Predicate::cmp(CmpOp::Lt, i, Predicate::slice_len(xs));
    let o = obligation(goal, Vec::new(), ObligationKind::SliceBound);
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    match outcome {
        DischargeOutcome::Sat { counterexample } => {
            // Z3 should produce a counter-example assigning concrete values.
            // Counter-example must include `i` — len(xs) is not directly a
            // Translator variable (it's a UF application) so it may or may
            // not appear depending on model completion.
            let names: Vec<_> = counterexample
                .bindings
                .iter()
                .map(|(n, _)| n.as_str())
                .collect();
            assert!(
                names.contains(&"i"),
                "counter-example should include `i`; got: {names:?}"
            );
        }
        other => panic!("expected Sat, got {other:?}"),
    }
}

#[test]
fn unsatisfiable_context_discharges_anything() {
    // Context is internally contradictory (x > 5 ∧ x < 3). Goal: false.
    // The negated goal is true, so we end up asserting (true) ∧ (x > 5) ∧ (x < 3),
    // which is unsat — Z3 reports unsat and we count it as success.
    let backend = Z3Backend::new();
    let x = var("x", Sort::Int(i32_sort()));
    let goal = Predicate::BoolLit(false);
    let context = vec![
        Predicate::cmp(CmpOp::Gt, x.clone(), lit_i32(5)),
        Predicate::cmp(CmpOp::Lt, x, lit_i32(3)),
    ];
    let o = obligation(goal, context, ObligationKind::DivByZero);
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    assert!(outcome.is_success(), "outcome: {outcome:?}");
}

#[test]
fn counterexample_carries_bool_and_int_bindings() {
    // Goal: x > 0 ∧ b. Context: empty. Sat: x = 0 (or negative), b = false.
    let backend = Z3Backend::new();
    let x = var("x", Sort::Int(i32_sort()));
    let b = var("b", Sort::Bool);
    let goal = Predicate::and(Predicate::cmp(CmpOp::Gt, x, lit_i32(0)), b);
    let o = obligation(goal, Vec::new(), ObligationKind::DivByZero);
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    match outcome {
        DischargeOutcome::Sat { counterexample } => {
            let names: Vec<_> = counterexample
                .bindings
                .iter()
                .map(|(n, _)| n.as_str())
                .collect();
            assert!(
                names.contains(&"x") && names.contains(&"b"),
                "expected both x and b in counter-example; got {names:?}"
            );
        }
        other => panic!("expected Sat, got {other:?}"),
    }
}

#[test]
fn unsupported_predicate_returns_unknown() {
    // Field projection isn't supported → Unknown.
    let backend = Z3Backend::new();
    let buf = var(
        "buf",
        Sort::Record(edda_refine::RecordRef::new("StringBuf")),
    );
    let field = edda_refine::FieldRef::new(
        edda_refine::RecordRef::new("StringBuf"),
        "len",
        Sort::usize(),
    );
    let proj = Predicate::field_proj(buf, field);
    let goal = Predicate::cmp(CmpOp::Gt, proj, lit_i32(0));
    let o = obligation(goal, Vec::new(), ObligationKind::DivByZero);
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    match outcome {
        DischargeOutcome::Unknown { reason: Some(r) } => {
            assert!(r.contains("translation failure"), "reason: {r}");
        }
        other => panic!("expected Unknown(translation failure), got {other:?}"),
    }
}

#[test]
fn discharge_at_normal_timeout_succeeds_on_trivial_obligation() {
    // x == x is unsat under negation regardless of any context, so the
    // happy path with a generous 5s timeout returns Unsat. This confirms
    // the timeout-configuration call doesn't poison non-timeout outcomes.
    let backend = Z3Backend::new();
    let x = var("x", Sort::Int(i32_sort()));
    let goal = Predicate::cmp(CmpOp::Eq, x.clone(), x);
    let o = obligation(goal, Vec::new(), ObligationKind::DivByZero);
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    assert!(outcome.is_success(), "outcome: {outcome:?}");
}

#[test]
fn unsatisfiably_tight_timeout_routes_to_timeout_outcome() {
    // A 1ms timeout is shorter than Z3's cold-start (~40ms on Windows).
    // Z3 returns Unknown with `reason_unknown` containing "timeout"; we
    // project that into [`DischargeOutcome::Timeout`] with the configured
    // and elapsed durations.
    let backend = Z3Backend::new();
    let x = var("x", Sort::Int(i32_sort()));
    let goal = Predicate::cmp(CmpOp::Eq, x.clone(), x);
    let o = obligation(goal, Vec::new(), ObligationKind::DivByZero);
    let timeout = Duration::from_millis(1);
    let outcome = backend.discharge(&o, timeout);
    match outcome {
        DischargeOutcome::Timeout {
            configured,
            elapsed: _,
        } => {
            assert_eq!(configured, timeout, "configured timeout passed through");
        }
        // Fast machines may finish under 1ms; that's a valid Unsat in our
        // Sat/Unsat/Timeout/Unknown enumeration. We only need to confirm
        // the negative-result path is wired — the positive path is
        // covered by `discharge_at_normal_timeout_succeeds_on_trivial_obligation`.
        DischargeOutcome::Unsat { .. } => {}
        other => panic!("expected Timeout or Unsat, got {other:?}"),
    }
}

#[test]
fn translation_error_display_renders_unsupported() {
    let err = TranslationError::Unsupported {
        what: "test".to_string(),
    };
    let s = format!("{err}");
    assert!(s.contains("unsupported"), "display: {s}");
}
