//! Annotation-route integration tests for the Z3 backend.
//!
//! Confirms that `@unverified` and `@trust` routes bypass Z3 entirely and emit
//! the corresponding [`CertificateWitness`] variants per
//! `docs/types/refinement-decidability.md` §9, and that the
//! `Comptime` / `Implicit` routes (owned by edda-comptime / edda-types) surface
//! [`DischargeOutcome::Unknown`] when they leak into refine.

mod common;

use std::time::Duration;

use common::{i32_sort, lit_i32, obligation, var};
use edda_refine::{
    CertificateWitness, CmpOp, DischargeOutcome, DischargeRoute, ObligationKind, Predicate, Solver,
    Sort, TrustAnnotation, UnverifiedAnnotation, Z3Backend,
};
use edda_span::Span;

#[test]
fn unverified_route_skips_solver_and_emits_unverified_certificate() {
    let backend = Z3Backend::new();
    let ann = UnverifiedAnnotation::new(
        "FFI shim; correctness audited manually against LLVM 18 docs",
        Span::DUMMY,
    )
    .unwrap();
    // Goal would *not* discharge against an empty context (no constraint
    // on `den`), so an Unsat outcome here is a sure sign the route bypassed
    // the solver and routed through the annotation.
    let den = var("den", Sort::Int(i32_sort()));
    let goal = Predicate::cmp(CmpOp::Ne, den, lit_i32(0));
    let o = obligation(goal, Vec::new(), ObligationKind::DivByZero)
        .with_route(DischargeRoute::Unverified(ann.clone()));
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    match outcome {
        DischargeOutcome::Unsat { certificate } => match certificate.witness() {
            CertificateWitness::Unverified {
                reason,
                function_site,
            } => {
                assert_eq!(reason, &ann.reason);
                assert_eq!(function_site, &ann.function_site);
            }
            other => panic!("expected Unverified witness, got {other:?}"),
        },
        other => panic!("expected Unsat, got {other:?}"),
    }
}

#[test]
fn trust_route_skips_solver_and_emits_trust_certificate() {
    let backend = Z3Backend::new();
    let ann = TrustAnnotation::new(
        "Karatsuba step; NLA outside the required-decidable fragment",
        Span::DUMMY,
    )
    .unwrap();
    let x = var("x", Sort::Int(i32_sort()));
    let y = var("y", Sort::Int(i32_sort()));
    // Predicate referencing two non-literal multiplications — outside LIA
    // and would otherwise reach Unknown. The route should bypass.
    let goal = Predicate::cmp(CmpOp::Gt, Predicate::add(x, y), lit_i32(0));
    let o = obligation(goal, Vec::new(), ObligationKind::DivByZero)
        .with_route(DischargeRoute::Trust(ann.clone()));
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    match outcome {
        DischargeOutcome::Unsat { certificate } => match certificate.witness() {
            CertificateWitness::Trust {
                reason,
                obligation_site,
            } => {
                assert_eq!(reason, &ann.reason);
                assert_eq!(obligation_site, &ann.obligation_site);
            }
            other => panic!("expected Trust witness, got {other:?}"),
        },
        other => panic!("expected Unsat, got {other:?}"),
    }
}

#[test]
fn comptime_or_implicit_routes_surface_unknown() {
    // edda-comptime and edda-types mint these — if they reach refine,
    // we surface Unknown with a clear "wrong layer" message.
    let backend = Z3Backend::new();
    let goal = Predicate::BoolLit(true);
    let o = obligation(goal, Vec::new(), ObligationKind::DivByZero)
        .with_route(DischargeRoute::Comptime);
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    match outcome {
        DischargeOutcome::Unknown { reason: Some(r) } => {
            assert!(r.contains("edda-comptime"), "reason: {r}");
        }
        other => panic!("expected Unknown(wrong layer), got {other:?}"),
    }
}
