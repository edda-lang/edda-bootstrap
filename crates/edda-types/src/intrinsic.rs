//! Compiler-intrinsic method kinds that lower to MIR rvalues directly,
//! plus the closed catalogue of primitive-headed static calls that the
//! resolver leaves silent for the typechecker to dispatch.
//!
//! [`CapabilityMethod`] catalogues the receiver-typed alloc-family methods
//! that admit a comptime `Type` argument (`allocator.alloc_array(T, n)`).
//! These are dispatched on the receiver's [`crate::CapabilityType`] —
//! the method has no Edda-side declaration; the typechecker recognises
//! the `(capability-type, method-name)` pair and synthesises the call.
//! MIR lowering routes the recorded variant through
//! [`crate::infer`]'s `comptime_type_paths` (the recorded `T`) plus the
//! `__edda_alloc_array` extern name, letting `alloc_family.rs`'s rewrite
//! prepend `size_of(T)` / `align_of(T)` constants.

use crate::CapabilityType;

/// Compiler-intrinsic methods that bypass free-function lookup and lower
/// directly to a MIR [`edda_mir::RvalueKind`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum IntrinsicKind {
    /// `s.bytes()` on a `String` receiver — lowers to `RvalueKind::StringBytes`.
    StringBytes,
    /// `.len()` on a slice receiver (`[T]`) — lowers to `RvalueKind::ArrayLen`.
    SliceLen,
    /// `discriminant_of(v)` on a sum-typed value — the active variant's
    /// declaration-order index, typed `usize`. Free-function shape (the
    /// lone non-method intrinsic); lowers to `RvalueKind::ExtractTag`
    /// widened to `usize` via `RvalueKind::Cast`. D-22 READ surface.
    DiscriminantOf,
}

/// Locked catalogue of capability-typed methods that admit a comptime
/// `Type` argument. Each variant identifies a `(receiver capability,
/// method name)` pair the typechecker dispatches directly without a
/// free-function lookup; MIR lowering reads the dispatch out of
/// [`crate::TypedPackage::capability_method_calls`] and emits a Call
/// against an `__edda_*` extern name, letting `edda-mir`'s
/// `alloc_family` rewrite materialise the runtime ABI.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum CapabilityMethod {
    /// `allocator.alloc_array(comptime T: Type, n: usize) -> [T] with {allocator, err: alloc.AllocError}`.
    ///
    /// Source-level surface for the per-T `__edda_alloc_array` extern
    /// the stdlib's `_alloc_array` wrappers route through (e.g.
    /// `Vec(T)._alloc_array`). The typechecker accepts the receiver
    /// form; MIR lowering routes to the same extern name so the existing
    /// `alloc_family` rewrite (`__edda_alloc_array` →
    /// `__edda_alloc_array_raw` with prepended `size_of(T)` /
    /// `align_of(T)`) handles the runtime ABI.
    AllocatorAllocArray,
    /// `allocator.fork() -> Allocator with {allocator}`.
    ///
    /// Mints a child arena allocator from the receiver. Lowers to the
    /// `__edda_heap_fork(parent_heap: u64) -> u64` extern the stdlib
    /// claims from `std.mem.rt`. The forked child is a DISTINCT
    /// capability value, not an alias of the parent: MIR registers it
    /// as a local capability slot (`Body::local_capabilities`) so
    /// downstream calls thread the child's handle, while the
    /// typechecker's alias map still discharges its uses against the
    /// parent's `{allocator}` row entry. Closing the child is a stdlib
    /// concern (`__edda_heap_destroy` via a capability-param function),
    /// not a catalogue entry.
    AllocatorFork,
    /// `random.deterministic(seed: u64) -> DeterministicRandom with {random}`.
    ///
    /// The locked `Random.deterministic(seed)` capability narrowing.
    /// Lowers to the
    /// `__edda_random_deterministic(random, seed: u64) -> handle` extern
    /// in `edda-rt`, which mints a seeded splitmix64 state and returns
    /// its heap handle as the capability value. Like `AllocatorFork`,
    /// the result is a DISTINCT capability value (the state handle), not
    /// an alias of the receiver: MIR registers the `let` binding as a
    /// local capability slot (`Body::local_capabilities`) so the
    /// `__edda_det_random_*` externs receive the state handle, while the
    /// typechecker's alias map discharges its uses against the parent's
    /// `{random}` row entry.
    RandomDeterministic,
}

impl CapabilityMethod {
    /// Source-level method spelling, for diagnostics
    /// (`allocator.alloc_array(...)`).
    pub fn method_name(self) -> &'static str {
        match self {
            Self::AllocatorAllocArray => "alloc_array",
            Self::AllocatorFork => "fork",
            Self::RandomDeterministic => "deterministic",
        }
    }

    /// The linker-visible extern name MIR lowering synthesises a Call
    /// against. Matched by `edda_mir::lower::alloc_family::AllocFamily::from_name`
    /// so the runtime-ABI rewrite kicks in.
    pub fn extern_symbol(self) -> &'static str {
        match self {
            Self::AllocatorAllocArray => "__edda_alloc_array",
            Self::AllocatorFork => "__edda_heap_fork",
            Self::RandomDeterministic => "__edda_random_deterministic",
        }
    }
}

/// Match a receiver capability + method-name pair against the locked
/// capability-method catalogue. The receiver type tells the typechecker
/// which capability the method dispatches on; the method name selects
/// the variant. Returns `None` for any pair outside the catalogue so the
/// caller can route through the regular free-function search.
pub fn resolve_capability_method(
    receiver: CapabilityType,
    method: &str,
) -> Option<CapabilityMethod> {
    match (receiver, method) {
        (CapabilityType::Allocator, "alloc_array") => {
            Some(CapabilityMethod::AllocatorAllocArray)
        }
        (CapabilityType::Allocator, "fork") => Some(CapabilityMethod::AllocatorFork),
        (CapabilityType::Random, "deterministic") => {
            Some(CapabilityMethod::RandomDeterministic)
        }
        _ => None,
    }
}

/// Locked catalogue of static-call methods on primitive types (`String`,
/// `Codepoint`, …). The resolver returns `Resolved::Error` for catalogue
/// heads and stays silent — see
/// [`edda_resolve::resolver::resolve_path`]; this catalogue is the
/// typechecker's intervention point, mirroring the comptime-builtin
/// short-circuit in [`crate::infer::call::synth_call`]. Each variant
/// lowers to a runtime extern through MIR's `FuncRef::Extern` arm.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum PrimitiveStaticMethod {}

impl PrimitiveStaticMethod {
    /// The `__edda_*` runtime symbol this static method dispatches to.
    pub fn extern_symbol(self) -> &'static str {
        match self {}
    }
}

/// Match a two-segment path's `(head, leaf)` pair against the locked
/// primitive-static-method catalogue. Currently empty — see
/// [`PrimitiveStaticMethod`].
pub fn resolve_primitive_static_method(_head: &str, _leaf: &str) -> Option<PrimitiveStaticMethod> {
    None
}
