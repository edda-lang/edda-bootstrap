//! Z3 backend integration tests: records, sum tag-equality, cast.
//!
//! Exercises the EUF-over-Z3-datatypes path, plus the
//! `Predicate::cast` no-op (since every Edda integer sort maps to Z3 Int).
//! Schema-gap and payload-bearing-variant negative paths verify the
//! [`DischargeOutcome::Unknown`] reasoning carries enough information to drive
//! the diagnostic tail per `refinement-decidability.md` §5.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{
    closed_variant, connection_schema, connection_var, i32_sort, lit_i32, obligation,
    open_variant, point_field, point_schema, point_var, var,
};
use edda_refine::{
    CmpOp, DischargeOutcome, IntSort, IntWidth, ObligationKind, Predicate, Schema, Solver, Sort,
    SumRef, SumSchema, VariantRef, VariantSchema, Z3Backend,
};

#[test]
fn record_field_equality_discharges_from_matching_context() {
    // Goal: p.x == 1. Context: p.x == 1.
    let backend = Z3Backend::with_schema(point_schema());
    let p = point_var("p");
    let goal = Predicate::cmp(
        CmpOp::Eq,
        Predicate::field_proj(p.clone(), point_field("x")),
        lit_i32(1),
    );
    let ctx_pred = Predicate::cmp(
        CmpOp::Eq,
        Predicate::field_proj(p, point_field("x")),
        lit_i32(1),
    );
    let o = obligation(goal, vec![ctx_pred], ObligationKind::DivByZero);
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    assert!(outcome.is_success(), "outcome: {outcome:?}");
}

#[test]
fn record_field_equality_discharges_via_let_binding_propagation() {
    // Goal: p2.x == p1.x. Context: p1 == p2 (e.g., `let p2 = p1`).
    // Z3 datatype structural equality lets this discharge without any
    // field-by-field axiomatisation on our side.
    let backend = Z3Backend::with_schema(point_schema());
    let p1 = point_var("p1");
    let p2 = point_var("p2");
    let goal = Predicate::cmp(
        CmpOp::Eq,
        Predicate::field_proj(p2.clone(), point_field("x")),
        Predicate::field_proj(p1.clone(), point_field("x")),
    );
    let ctx_pred = Predicate::cmp(CmpOp::Eq, p1, p2);
    let o = obligation(goal, vec![ctx_pred], ObligationKind::DivByZero);
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    assert!(outcome.is_success(), "outcome: {outcome:?}");
}

#[test]
fn record_field_equality_fails_when_unconstrained() {
    // Goal: p.x == 0 with no context → Sat (counter-example x = some non-zero).
    let backend = Z3Backend::with_schema(point_schema());
    let p = point_var("p");
    let goal = Predicate::cmp(
        CmpOp::Eq,
        Predicate::field_proj(p, point_field("x")),
        lit_i32(0),
    );
    let o = obligation(goal, Vec::new(), ObligationKind::DivByZero);
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    assert!(
        matches!(outcome, DischargeOutcome::Sat { .. }),
        "outcome: {outcome:?}"
    );
}

#[test]
fn tag_equality_discharges_from_matching_context() {
    // Goal: state == Connection.closed. Context: state == Connection.closed.
    let backend = Z3Backend::with_schema(connection_schema());
    let state = connection_var("state");
    let goal = Predicate::tag_eq(state.clone(), closed_variant());
    let ctx_pred = Predicate::tag_eq(state, closed_variant());
    let o = obligation(goal, vec![ctx_pred], ObligationKind::DivByZero);
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    assert!(outcome.is_success(), "outcome: {outcome:?}");
}

#[test]
fn tag_equality_against_different_variant_is_unsatisfiable() {
    // Context: state == closed ∧ state == open. Z3 datatype variant
    // distinctness makes this unsat; any goal discharges.
    let backend = Z3Backend::with_schema(connection_schema());
    let state = connection_var("state");
    let context = vec![
        Predicate::tag_eq(state.clone(), closed_variant()),
        Predicate::tag_eq(state, open_variant()),
    ];
    let goal = Predicate::BoolLit(false);
    let o = obligation(goal, context, ObligationKind::DivByZero);
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    assert!(outcome.is_success(), "outcome: {outcome:?}");
}

#[test]
fn non_narrowing_cast_collapses_to_identity_at_smt_layer() {
    // x as i64 == x as i64 — Cast translates to identity since every Edda
    // integer sort maps to Z3 Int. Goal is trivially Unsat under negation.
    let backend = Z3Backend::new();
    let x = var("x", Sort::Int(i32_sort()));
    let cast = Predicate::cast(x.clone(), IntSort::sized(IntWidth::W64, true));
    let goal = Predicate::cmp(CmpOp::Eq, cast.clone(), cast);
    let o = obligation(
        goal,
        Vec::new(),
        ObligationKind::NarrowingCast {
            from: i32_sort(),
            to: IntSort::sized(IntWidth::W64, true),
        },
    );
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    assert!(outcome.is_success(), "outcome: {outcome:?}");
}

#[test]
fn record_field_proj_without_schema_surfaces_unknown() {
    // Same predicate as the earlier LIA/Bool-only test, but now the error
    // reason points at the schema gap rather than at an "unsupported" form.
    let backend = Z3Backend::new(); // empty schema
    let buf = point_var("buf");
    let proj = Predicate::field_proj(buf, point_field("x"));
    let goal = Predicate::cmp(CmpOp::Gt, proj, lit_i32(0));
    let o = obligation(goal, Vec::new(), ObligationKind::DivByZero);
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    match outcome {
        DischargeOutcome::Unknown { reason: Some(r) } => {
            assert!(r.contains("Point"), "reason: {r}");
        }
        other => panic!("expected Unknown(schema gap), got {other:?}"),
    }
}

#[test]
fn payload_bearing_variant_in_tag_eq_routes_to_unsupported() {
    // A sum with a payload-bearing variant. Spec §5 rejects equality on
    // payload-bearing variants; we surface that as Unknown(unsupported).
    let schema = Arc::new(Schema::empty().with_sum(SumSchema::new(
        "Maybe",
        vec![
            VariantSchema::tag("none"),
            VariantSchema::with_payload(
                "some",
                vec![("value".into(), Sort::Int(i32_sort()))],
            ),
        ],
    )));
    let backend = Z3Backend::with_schema(schema);
    let m = var("m", Sort::Sum(SumRef::new("Maybe")));
    let goal = Predicate::tag_eq(m, VariantRef::new(SumRef::new("Maybe"), "some"));
    let o = obligation(goal, Vec::new(), ObligationKind::DivByZero);
    let outcome = backend.discharge(&o, Duration::from_secs(5));
    match outcome {
        DischargeOutcome::Unknown { reason: Some(r) } => {
            assert!(r.contains("payload-bearing"), "reason: {r}");
        }
        other => panic!("expected Unknown(unsupported), got {other:?}"),
    }
}
