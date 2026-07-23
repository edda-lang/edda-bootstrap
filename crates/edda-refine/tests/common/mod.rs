//! Shared test fixtures for the Z3 integration test suite.
//!
//! Z3 obligations have several recurring shapes — i32 sorts, point records,
//! connection sums — that every test file would otherwise re-declare. The
//! helpers live here so each `tests/z3_*.rs` file gets the same vocabulary
//! via `mod common;` (Rust's integration-test convention for shared code).
//!
//! Each integration test file is a separate crate from Cargo's perspective,
//! so helpers it does not use will trigger `dead_code` warnings if compiled
//! into that file's crate. The `#![allow(dead_code)]` here silences those.

#![allow(dead_code)]

use std::sync::Arc;

use edda_refine::{
    FieldRef, IntLit, IntSort, IntWidth, Obligation, ObligationKind, Predicate, RecordRef,
    RecordSchema, Schema, Sort, SumRef, SumSchema, VariantRef, VariantSchema, Variable,
};
use edda_span::Span;

/// `i32` integer sort — the workhorse for LIA tests.
pub fn i32_sort() -> IntSort {
    IntSort::sized(IntWidth::W32, true)
}

/// Construct a free variable predicate.
pub fn var(name: &str, sort: Sort) -> Predicate {
    Predicate::Var(Variable::new(name, sort))
}

/// `i32` integer literal.
pub fn lit_i32(v: i32) -> Predicate {
    Predicate::IntLit(IntLit::signed(v as i128, i32_sort()))
}

/// Construct an SMT-routed obligation against a dummy span.
pub fn obligation(goal: Predicate, context: Vec<Predicate>, kind: ObligationKind) -> Obligation {
    Obligation::new(goal, context, Span::DUMMY, kind, "")
}

/// `Point { x: i32, y: i32 }` record schema.
pub fn point_schema() -> Arc<Schema> {
    Arc::new(Schema::empty().with_record(RecordSchema::new(
        "Point",
        vec![
            ("x".into(), Sort::Int(i32_sort())),
            ("y".into(), Sort::Int(i32_sort())),
        ],
    )))
}

/// `Connection { closed, open }` sum schema — two payload-free variants,
/// the canonical refinement-decidability.md §5 tag-equality case.
pub fn connection_schema() -> Arc<Schema> {
    Arc::new(Schema::empty().with_sum(SumSchema::new(
        "Connection",
        vec![VariantSchema::tag("closed"), VariantSchema::tag("open")],
    )))
}

/// Free variable of sort `Record(Point)`.
pub fn point_var(name: &str) -> Predicate {
    var(name, Sort::Record(RecordRef::new("Point")))
}

/// Field reference for `Point.<field>` of sort `i32`.
pub fn point_field(field: &str) -> FieldRef {
    FieldRef::new(RecordRef::new("Point"), field, Sort::Int(i32_sort()))
}

/// Free variable of sort `Sum(Connection)`.
pub fn connection_var(name: &str) -> Predicate {
    var(name, Sort::Sum(SumRef::new("Connection")))
}

/// `Connection.closed` variant reference.
pub fn closed_variant() -> VariantRef {
    VariantRef::new(SumRef::new("Connection"), "closed")
}

/// `Connection.open` variant reference.
pub fn open_variant() -> VariantRef {
    VariantRef::new(SumRef::new("Connection"), "open")
}
