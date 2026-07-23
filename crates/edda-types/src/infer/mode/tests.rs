//! Tests for the [`BindingState`] lattice (GLB, field-state derivation,
//! readability, and the `describe` rendering).

use super::BindingState::*;
use std::collections::BTreeSet;
use edda_intern::Symbol;

#[test]
fn glb_consumed_absorbs() {
    assert_eq!(Consumed.glb(&Valid), Consumed);
    assert_eq!(Valid.glb(&Consumed), Consumed);
    assert_eq!(Consumed.glb(&Uninit), Consumed);
    assert_eq!(Uninit.glb(&Consumed), Consumed);
    assert_eq!(Consumed.glb(&Consumed), Consumed);
}

#[test]
fn glb_uninit_below_valid() {
    assert_eq!(Uninit.glb(&Valid), Uninit);
    assert_eq!(Valid.glb(&Uninit), Uninit);
    assert_eq!(Uninit.glb(&Uninit), Uninit);
}

#[test]
fn glb_valid_only_when_both_valid() {
    assert_eq!(Valid.glb(&Valid), Valid);
}

#[test]
fn glb_is_commutative() {
    for a in [Consumed, Uninit, Valid] {
        for b in [Consumed, Uninit, Valid] {
            assert_eq!(a.glb(&b), b.glb(&a));
        }
    }
}

#[test]
fn is_readable_only_for_valid() {
    let interner = edda_intern::Interner::new();
    let a = interner.intern("a");
    assert!(Valid.is_readable());
    assert!(!Uninit.is_readable());
    assert!(!Consumed.is_readable());
    // PartialInit is never whole-readable, even when every field
    // would be Valid (the binding must be promoted explicitly).
    let mut full = BTreeSet::new();
    full.insert(a);
    assert!(!PartialInit(full).is_readable());
}

fn syms(names: &[&str]) -> (edda_intern::Interner, Vec<Symbol>) {
    let interner = edda_intern::Interner::new();
    let v: Vec<Symbol> = names.iter().map(|n| interner.intern(n)).collect();
    (interner, v)
}

#[test]
fn glb_partial_init_intersects() {
    let (_int, sym) = syms(&["a", "b", "c"]);
    let f1: BTreeSet<Symbol> = [sym[0], sym[1]].into_iter().collect();
    let f2: BTreeSet<Symbol> = [sym[1], sym[2]].into_iter().collect();
    let expected: BTreeSet<Symbol> = [sym[1]].into_iter().collect();
    assert_eq!(
        PartialInit(f1).glb(&PartialInit(f2)),
        PartialInit(expected),
    );
}

#[test]
fn glb_valid_with_partial_init_keeps_partial() {
    let (_int, sym) = syms(&["a"]);
    let f: BTreeSet<Symbol> = [sym[0]].into_iter().collect();
    assert_eq!(Valid.glb(&PartialInit(f.clone())), PartialInit(f.clone()));
    assert_eq!(PartialInit(f.clone()).glb(&Valid), PartialInit(f));
}

#[test]
fn glb_uninit_with_partial_init_is_uninit() {
    let (_int, sym) = syms(&["a"]);
    let f: BTreeSet<Symbol> = [sym[0]].into_iter().collect();
    assert_eq!(Uninit.glb(&PartialInit(f.clone())), Uninit);
    assert_eq!(PartialInit(f).glb(&Uninit), Uninit);
}

#[test]
fn field_state_on_partial_init_distinguishes_in_and_out_of_set() {
    let (_int, sym) = syms(&["a", "b", "c"]);
    let valid_fields: BTreeSet<Symbol> = [sym[0], sym[2]].into_iter().collect();
    let s = PartialInit(valid_fields);
    assert_eq!(s.field_state(sym[0]), Valid);
    assert_eq!(s.field_state(sym[1]), Uninit);
    assert_eq!(s.field_state(sym[2]), Valid);
}

#[test]
fn field_state_on_whole_state_returns_that_state() {
    let (_int, sym) = syms(&["a"]);
    assert_eq!(Valid.field_state(sym[0]), Valid);
    assert_eq!(Uninit.field_state(sym[0]), Uninit);
    assert_eq!(Consumed.field_state(sym[0]), Consumed);
}

#[test]
fn describe_renders_each_state() {
    assert_eq!(Valid.describe(), "valid");
    assert_eq!(Uninit.describe(), "uninitialised");
    assert_eq!(Consumed.describe(), "consumed (moved out)");
    assert_eq!(
        PartialInit(BTreeSet::new()).describe(),
        "partially initialised",
    );
}
