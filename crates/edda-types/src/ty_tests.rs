//! Tests for the type interner and the TyKind variants.

use super::*;
use edda_resolve::ModuleId;
use std::sync::Arc;
use std::thread;

fn binding(module: u32, index: u32) -> BindingId {
    BindingId::new(ModuleId::new(module), index)
}

#[test]
fn new_preallocates_prims_and_error() {
    let interner = TyInterner::new();
    assert_eq!(interner.len(), PRIM_COUNT + CAPABILITY_COUNT + 1);
    for p in Primitive::ALL {
        let id = interner.prim(p);
        assert!(
            matches!(interner.kind(id), TyKind::Primitive(q) if *q == p),
            "prim slot for {p:?} did not hold the matching kind"
        );
    }
    for c in CapabilityType::ALL {
        let id = interner.capability(c);
        assert!(
            matches!(interner.kind(id), TyKind::Capability(d) if *d == c),
            "capability slot for {c:?} did not hold the matching kind"
        );
    }
    assert!(matches!(interner.kind(interner.error()), TyKind::Error));
}

#[test]
fn prim_handles_are_distinct() {
    let interner = TyInterner::new();
    let mut ids: Vec<TyId> = Primitive::ALL.iter().map(|p| interner.prim(*p)).collect();
    ids.sort_by_key(|id| id.0);
    ids.dedup();
    assert_eq!(ids.len(), PRIM_COUNT, "primitive handles must be distinct");
}

#[test]
fn intern_kind_is_idempotent_for_primitives() {
    let interner = TyInterner::new();
    let direct = interner.prim(Primitive::I32);
    let via_intern = interner.intern_kind(TyKind::Primitive(Primitive::I32));
    assert_eq!(direct, via_intern);
    assert_eq!(interner.len(), PRIM_COUNT + CAPABILITY_COUNT + 1, "no new entries added");
}

#[test]
fn slice_and_tuple_dedup() {
    let interner = TyInterner::new();
    let u8_id = interner.prim(Primitive::U8);
    let slice_a = interner.slice(u8_id);
    let slice_b = interner.slice(u8_id);
    assert_eq!(slice_a, slice_b);

    let i32_id = interner.prim(Primitive::I32);
    let str_id = interner.prim(Primitive::String);
    let tup_a = interner.tuple([i32_id, str_id]);
    let tup_b = interner.tuple(vec![i32_id, str_id]);
    assert_eq!(tup_a, tup_b);

    // Distinct shapes do not collide.
    assert_ne!(slice_a, tup_a);
    let tup_c = interner.tuple([str_id, i32_id]); // order matters
    assert_ne!(tup_a, tup_c);
}

#[test]
fn nested_structural_dedup() {
    let interner = TyInterner::new();
    let u8_id = interner.prim(Primitive::U8);
    let slice_u8 = interner.slice(u8_id);
    // [[u8]] interned twice — must dedup at the outer level too.
    let nested_a = interner.slice(slice_u8);
    let nested_b = interner.slice(slice_u8);
    assert_eq!(nested_a, nested_b);

    // Tuple of nested slices.
    let i32_id = interner.prim(Primitive::I32);
    let tup_a = interner.tuple([nested_a, i32_id]);
    let tup_b = interner.tuple([nested_b, i32_id]);
    assert_eq!(tup_a, tup_b);
}

#[test]
fn error_handle_is_stable() {
    let interner = TyInterner::new();
    let a = interner.error();
    let b = interner.intern_kind(TyKind::Error);
    assert_eq!(a, b);
    assert_eq!(interner.len(), PRIM_COUNT + CAPABILITY_COUNT + 1);
}

#[test]
fn nominal_dedups_by_binding_id() {
    let interner = TyInterner::new();
    let b = binding(0, 3);
    let a1 = interner.nominal(b);
    let a2 = interner.nominal(b);
    let a3 = interner.intern_kind(TyKind::Nominal(b));
    assert_eq!(a1, a2);
    assert_eq!(a1, a3);
    assert!(matches!(interner.kind(a1), TyKind::Nominal(other) if *other == b));
}

#[test]
fn nominal_distinct_by_binding_id() {
    let interner = TyInterner::new();
    let a = interner.nominal(binding(0, 1));
    let b = interner.nominal(binding(0, 2));
    let c = interner.nominal(binding(1, 1));
    assert_ne!(a, b);
    assert_ne!(a, c);
    assert_ne!(b, c);
}

#[test]
fn nominal_distinct_from_other_kinds() {
    let interner = TyInterner::new();
    let nom = interner.nominal(binding(0, 0));
    assert_ne!(nom, interner.error());
    assert_ne!(nom, interner.prim(Primitive::I32));
}

#[test]
fn tuple_admits_one_element_distinct_from_bare() {
    // The one-element tuple is the D-22 sum-variant payload composite
    // (`case data(u32)` → `(u32)`) — a real tuple, kept
    // structurally distinct from the bare element so `is_primitive`
    // folds false on it and the payload field-walk `x.(0)` types.
    let interner = TyInterner::new();
    let i32_id = interner.prim(Primitive::I32);
    let one = interner.tuple([i32_id]);
    assert_ne!(one, i32_id);
    assert_eq!(interner.kind(one), &TyKind::Tuple(Box::from([i32_id])));
    // Idempotent, and distinct from the two-element tuple.
    assert_eq!(one, interner.tuple([i32_id]));
    assert_ne!(one, interner.tuple([i32_id, i32_id]));
}

#[test]
#[should_panic(expected = "TyKind::Tuple requires >= 1 element")]
fn tuple_rejects_empty() {
    let interner = TyInterner::new();
    let _ = interner.tuple(Vec::<TyId>::new());
}

#[test]
fn display_renders_primitives() {
    let interner = TyInterner::new();
    assert_eq!(interner.display(interner.prim(Primitive::I32)).to_string(), "i32");
    assert_eq!(interner.display(interner.prim(Primitive::Unit)).to_string(), "()");
    assert_eq!(
        interner.display(interner.prim(Primitive::String)).to_string(),
        "String"
    );
    assert_eq!(
        interner.display(interner.prim(Primitive::Never)).to_string(),
        "never"
    );
}

#[test]
fn display_renders_composites() {
    let interner = TyInterner::new();
    let u8_id = interner.prim(Primitive::U8);
    let slice_u8 = interner.slice(u8_id);
    assert_eq!(interner.display(slice_u8).to_string(), "[u8]");

    let i32_id = interner.prim(Primitive::I32);
    let str_id = interner.prim(Primitive::String);
    let tup = interner.tuple([i32_id, str_id]);
    assert_eq!(interner.display(tup).to_string(), "(i32, String)");

    // Nested: [(i32, [u8])]
    let inner_tup = interner.tuple([i32_id, slice_u8]);
    let outer = interner.slice(inner_tup);
    assert_eq!(interner.display(outer).to_string(), "[(i32, [u8])]");
}

#[test]
fn display_renders_error() {
    let interner = TyInterner::new();
    assert_eq!(interner.display(interner.error()).to_string(), "<error>");
}

#[test]
fn display_renders_nominal() {
    let interner = TyInterner::new();
    let nom = interner.nominal(binding(2, 5));
    assert_eq!(interner.display(nom).to_string(), "<nominal 2:5>");
}

#[test]
fn kind_reference_survives_concurrent_inserts() {
    let interner = TyInterner::new();
    let u8_id = interner.prim(Primitive::U8);
    let anchor = interner.slice(u8_id);
    // Hold a reference to the anchor's kind.
    let kind_ref: &TyKind = interner.kind(anchor);
    // Force many subsequent inserts (and thus likely Vec reallocs).
    for n in 0..1024_u32 {
        let inner_slice = interner.slice(interner.prim(Primitive::U8));
        let _ = interner.tuple([inner_slice, TyId(n % (PRIM_COUNT as u32))]);
    }
    // The original reference must still read the correct content.
    assert!(matches!(kind_ref, TyKind::Slice(elem) if *elem == u8_id));
}

#[test]
#[should_panic(expected = "out of range")]
fn kind_panics_on_unknown_id() {
    let interner = TyInterner::new();
    let _ = interner.kind(TyId(u32::MAX - 1));
}

#[test]
fn thread_safe_concurrent_interning() {
    const THREADS: usize = 4;
    const ELEMS_PER_THREAD: usize = 128;

    let interner = Arc::new(TyInterner::new());
    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            let interner = Arc::clone(&interner);
            thread::spawn(move || {
                let u8_id = interner.prim(Primitive::U8);
                let i32_id = interner.prim(Primitive::I32);
                let mut ids = Vec::with_capacity(ELEMS_PER_THREAD);
                // Each thread interns the same set of types.
                for n in 0..ELEMS_PER_THREAD {
                    let slice = interner.slice(u8_id);
                    let tup = interner.tuple([slice, i32_id, TyId(n as u32 % 4)]);
                    ids.push((slice, tup));
                }
                ids
            })
        })
        .collect();

    let results: Vec<Vec<(TyId, TyId)>> =
        handles.into_iter().map(|h| h.join().unwrap()).collect();

    // All threads must agree on the TyId for each shape.
    let first = &results[0];
    for other in &results[1..] {
        assert_eq!(first, other, "threads disagreed on TyId assignment");
    }
}
