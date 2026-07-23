//! Type representation, the [`TyInterner`], and the [`TyId`] handle.
//!
//! The interner stores each distinct [`TyKind`] once and issues a 32-bit
//! [`TyId`]. Equal `TyId`s denote the same type by construction — the
//! interner dedup pass on insertion guarantees structural equality
//! reduces to handle equality. The rest of the compiler compares types
//! via `TyId == TyId` rather than walking [`TyKind`]s.
//!
//! The layout mirrors `edda_intern::Interner`: a `parking_lot::RwLock`
//! guards the state map, the kind table is append-only, and resolved
//! references are pointer-stable for the interner's lifetime.

use std::fmt;

use ahash::AHashMap;
use edda_intern::Interner;
use edda_resolve::{BindingId, ResolvedPackage};
use parking_lot::RwLock;

use crate::capability::{CAPABILITY_COUNT, CapabilityType};
use crate::prim::{PRIM_COUNT, Primitive};
use crate::sig::FnPtrSig;

/// Opaque 32-bit handle into a [`TyInterner`].
///
/// `TyId` mirrors `edda_intern::Symbol`: 32-bit, `Copy`, no payload.
/// Call [`TyInterner::kind`] to recover the underlying [`TyKind`] and
/// [`TyInterner::display`] to format a type for diagnostics.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
pub struct TyId(u32);

impl TyId {
    /// Raw `u32` representation of this type id.
    ///
    /// Exposed for debug tooling and serialisation. Do not use as a
    /// cross-process identifier — values depend on insertion order.
    #[inline]
    pub(crate) fn as_u32(self) -> u32 {
        self.0
    }
}

/// The shape of an Edda type, post-resolution.
///
/// Primitives, structurally-typed tuples, slices,
/// nominal user-type references, and the `Error` placeholder for
/// failed lowering are covered so far. Function types are still a future extension.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum TyKind {
    /// One of the [`Primitive`]s (per `docs/syntax/types.md`).
    Primitive(Primitive),
    /// Structural tuple `(T1, …, Tn)` with `n >= 1`. Surface tuples are
    /// `n >= 2` — `(x)` is grouping, not a one-tuple — but the D-22 sum
    /// fan-out mints one-element tuples synthetically: a single-payload
    /// variant `case data(u32)` reads and specializes as the one-element
    /// payload composite `(u32)`, structurally distinct from the bare
    /// `u32`. The unit type `()` is [`Primitive::Unit`],
    /// not a zero-tuple.
    Tuple(Box<[TyId]>),
    /// Slice `[T]` — dynamic-length contiguous sequence of `T`.
    Slice(TyId),
    /// Reference to a user-declared product or sum type, identified by
    /// the resolver's [`BindingId`]. Field / variant layout is recorded
    /// out-of-band on a `TyCx` keyed by the same `BindingId`; this
    /// payload is intentionally narrow so structural type identity
    /// stays decidable as handle equality.
    Nominal(BindingId),
    /// A built-in capability type from the [`CapabilityType`] catalogue
    /// (`Clock`, `MonotonicClock`, `Stdout`, `Stderr`, `Stdin`, `Allocator`,
    /// `Filesystem`, `Network`, `Random`, `Executor`, `ReadOnlyFilesystem`,
    /// `SandboxedFilesystem`, `LocalhostNetwork`, `RestrictedNetwork`,
    /// `BoundedAllocator`, `DeterministicRandom`).
    /// No runtime representation — capability types exist only at the type/effect layer.
    Capability(CapabilityType),
    /// First-class function-pointer type: `function(P, ...) -> R uses {row}`.
    ///
    /// Carries an [`FnPtrSig`] — a structural function signature with
    /// no parameter names or spans. Two `FnPtr` types are equal iff
    /// their `FnPtrSig`s are equal (same modes, same parameter types,
    /// same return, same effect row).
    ///
    /// The boxed payload keeps [`TyKind`]'s enum size flat — the same
    /// pattern `Tuple(Box<[TyId]>)` uses.
    FnPtr(Box<FnPtrSig>),
    /// Sentinel for failed lowering or unresolvable types.
    /// Structurally equal only to itself; lets passes continue past a
    /// local failure without short-circuiting downstream checks.
    Error,
}

/// Thread-safe interner for [`TyKind`].
///
/// Models `edda_intern::Interner`: read paths (lookup, [`TyInterner::kind`])
/// take the read lock; insertions take the write lock. `intern_*` methods
/// take `&self` via interior mutability so the daemon can share one
/// interner across worker threads.
///
/// Primitives, capabilities, and the `Error` sentinel are pre-allocated at
/// construction time. [`TyInterner::prim`], [`TyInterner::capability`], and
/// [`TyInterner::error`] are O(1) array accesses; tuple and slice interning
/// take the lock path.
pub struct TyInterner {
    inner: RwLock<TyInternerState>,
    /// Pre-allocated handle table — `prims[Primitive::as_index()]` is
    /// the `TyId` for that primitive. Populated once at construction.
    prims: [TyId; PRIM_COUNT],
    /// Pre-allocated handle table — `caps[CapabilityType::as_index()]` is
    /// the `TyId` for that capability. Populated once at construction.
    caps: [TyId; CAPABILITY_COUNT],
    /// Pre-allocated handle for [`TyKind::Error`]. Mirrors `prims` —
    /// always issued at construction time, never re-issued.
    error_id: TyId,
}

struct TyInternerState {
    /// `TyKind` -> `TyId` index. Key is `Box<TyKind>` so the map owns
    /// key memory independent of the `kinds` vector (avoids self-
    /// referential borrows).
    lookup: AHashMap<Box<TyKind>, TyId>,
    /// `TyId` index -> interned kind. Each `Box<TyKind>` lives on the
    /// heap; growing the `Vec` reallocates the slot array but never
    /// moves the boxed payload, so previously-returned `&TyKind`
    /// references stay valid. The boxing is load-bearing for the
    /// pointer-stability argument in [`TyInterner::kind`]'s unsafe
    /// block — removing it would let `Vec` growth invalidate
    /// outstanding `&TyKind` references mid-borrow.
    #[allow(clippy::vec_box)]
    kinds: Vec<Box<TyKind>>,
}

impl TyInterner {
    /// Construct a new interner with all primitives and the `Error`
    /// sentinel pre-allocated.
    ///
    /// After this call:
    /// - [`TyInterner::prim`] returns valid handles for every variant
    ///   of [`Primitive`].
    /// - [`TyInterner::error`] returns a stable handle for [`TyKind::Error`].
    /// - `len()` is `PRIM_COUNT + 1`.
    pub fn new() -> Self {
        let mut state = TyInternerState {
            lookup: AHashMap::with_capacity(PRIM_COUNT + 1),
            kinds: Vec::with_capacity(PRIM_COUNT + 1),
        };

        let mut prims: [TyId; PRIM_COUNT] = [TyId(0); PRIM_COUNT];
        for prim in Primitive::ALL {
            let id = Self::insert_locked(&mut state, TyKind::Primitive(prim));
            prims[prim.as_index()] = id;
        }
        let mut caps: [TyId; CAPABILITY_COUNT] = [TyId(0); CAPABILITY_COUNT];
        for cap in CapabilityType::ALL {
            let id = Self::insert_locked(&mut state, TyKind::Capability(cap));
            caps[cap.as_index()] = id;
        }
        let error_id = Self::insert_locked(&mut state, TyKind::Error);

        Self {
            inner: RwLock::new(state),
            prims,
            caps,
            error_id,
        }
    }

    /// Return the pre-allocated [`TyId`] for a primitive type.
    ///
    /// O(1) — looks up the pre-built handle table. Equivalent to
    /// `interner.intern_kind(TyKind::Primitive(p))`, but cheaper because
    /// it bypasses the lock and the hashmap.
    #[inline]
    pub fn prim(&self, p: Primitive) -> TyId {
        self.prims[p.as_index()]
    }

    /// Return the pre-allocated [`TyId`] for a capability type.
    ///
    /// O(1) — looks up the pre-built handle table. Equivalent to
    /// `interner.intern_kind(TyKind::Capability(c))`, but cheaper because
    /// it bypasses the lock and the hashmap.
    #[inline]
    pub fn capability(&self, c: CapabilityType) -> TyId {
        self.caps[c.as_index()]
    }

    /// Return the pre-allocated [`TyId`] for [`TyKind::Error`].
    ///
    /// Use this as a placeholder when a lowering pass cannot produce a
    /// real type — downstream checks gate on `id == interner.error()`
    /// to suppress cascading diagnostics.
    #[inline]
    pub fn error(&self) -> TyId {
        self.error_id
    }

    /// Intern an arbitrary [`TyKind`] and return its [`TyId`].
    ///
    /// Idempotent: structurally equal `TyKind` inputs return the same
    /// `TyId`. Prefer the convenience constructors ([`TyInterner::prim`],
    /// [`TyInterner::slice`], [`TyInterner::tuple`]) where they apply —
    /// they validate invariants before reaching this path.
    pub fn intern_kind(&self, kind: TyKind) -> TyId {
        // Fast path: read lock + lookup only.
        if let Some(id) = self.inner.read().lookup.get(&kind).copied() {
            return id;
        }
        self.intern_kind_slow(kind)
    }

    #[cold]
    fn intern_kind_slow(&self, kind: TyKind) -> TyId {
        let mut guard = self.inner.write();
        let state = &mut *guard;
        // Double-check after acquiring the write lock — another thread
        // may have interned the same kind while we waited.
        if let Some(id) = state.lookup.get(&kind).copied() {
            return id;
        }
        Self::insert_locked(state, kind)
    }

    /// Inserts a kind that is *known not to exist* under an exclusive
    /// guard. Returns the newly issued [`TyId`].
    ///
    /// Internal-only: callers are responsible for the double-check.
    fn insert_locked(state: &mut TyInternerState, kind: TyKind) -> TyId {
        let idx = state.kinds.len();
        assert!(
            idx < (u32::MAX as usize),
            "edda-types: TyInterner exhausted (u32::MAX kinds)"
        );
        let id = TyId(idx as u32);
        let owned: Box<TyKind> = Box::new(kind);
        state.kinds.push(owned.clone());
        state.lookup.insert(owned, id);
        id
    }

    /// Intern a `[T]` slice type. Equivalent to
    /// `interner.intern_kind(TyKind::Slice(elem))` with a clearer name.
    pub fn slice(&self, elem: TyId) -> TyId {
        self.intern_kind(TyKind::Slice(elem))
    }

    /// Intern a structural tuple type `(T1, …, Tn)` with `n >= 1`.
    ///
    /// Surface tuples are `n >= 2` (`(x)` is grouping); the one-element
    /// form arises only synthetically as a D-22 sum-variant payload
    /// composite — `case data(u32)` reads and specializes as the
    /// one-element tuple `(u32)`, structurally distinct from the bare
    /// `u32`. `()` stays [`Primitive::Unit`], so the empty
    /// tuple is still rejected.
    ///
    /// # Panics
    ///
    /// Debug-panics if `elements.len() == 0`.
    pub fn tuple(&self, elements: impl Into<Box<[TyId]>>) -> TyId {
        let elements: Box<[TyId]> = elements.into();
        debug_assert!(
            !elements.is_empty(),
            "edda-types: TyKind::Tuple requires >= 1 element (`()` is Primitive::Unit)"
        );
        self.intern_kind(TyKind::Tuple(elements))
    }

    /// Intern a nominal user-type reference. The [`BindingId`] must be
    /// issued by the same `ResolvedPackage` whose `TyCx` records this
    /// type's field / variant layout — equality of two nominal handles
    /// reduces to equality of their `BindingId`s.
    pub fn nominal(&self, binding: BindingId) -> TyId {
        self.intern_kind(TyKind::Nominal(binding))
    }

    /// Intern a `function(...) -> T uses {row}` type from its structural
    /// signature. Structurally equal `sig`s return the same [`TyId`].
    pub fn fn_ptr(&self, sig: FnPtrSig) -> TyId {
        self.intern_kind(TyKind::FnPtr(Box::new(sig)))
    }

    /// Resolve `id` to the [`TyKind`] it was interned from.
    ///
    /// The returned reference is borrowed from the interner; it remains
    /// valid for the lifetime of the `&self` borrow even if other
    /// threads continue to intern new kinds (the boxed payload behind
    /// each stored kind is never moved or freed).
    ///
    /// # Panics
    ///
    /// Panics if `id` was not issued by this interner.
    pub fn kind(&self, id: TyId) -> &TyKind {
        let state = self.inner.read();
        let idx = id.0 as usize;
        let entry = state
            .kinds
            .get(idx)
            .unwrap_or_else(|| panic!("edda-types: TyId({}) is out of range", id.0));
        // SAFETY: kinds is append-only — kinds[idx] is never removed or
        // replaced after assignment, and Vec growth reallocates the slot
        // array (the Box headers) but not the boxed TyKind payload. The
        // pointer-stability argument is the same as
        // `edda_intern::Interner::resolve`: the box payload outlives any
        // &self borrow, and the interner hands out no interior mutation
        // path. See edda-intern's `resolve` SAFETY comment for the
        // full justification.
        let ptr: *const TyKind = &**entry;
        drop(state);
        unsafe { &*ptr }
    }

    /// Number of distinct kinds currently interned (including pre-allocated primitives + `Error`).
    pub fn len(&self) -> usize {
        self.inner.read().kinds.len()
    }

    /// `false` for any constructed interner — `Self::new()` pre-allocates
    /// `PRIM_COUNT + 1` kinds before returning. Provided so consumers
    /// can follow the `len`/`is_empty` convention.
    pub fn is_empty(&self) -> bool {
        self.inner.read().kinds.is_empty()
    }

    /// Returns a [`fmt::Display`] adapter that renders `id` as Edda
    /// source-form text (`i32`, `[u8]`, `(i32, String)`, `<error>`).
    ///
    /// Borrows the interner so element ids can be recursively resolved.
    pub fn display(&self, id: TyId) -> TyDisplay<'_> {
        TyDisplay { id, interner: self }
    }

    /// Like [`TyInterner::display`] but able to name [`TyKind::Nominal`]
    /// types. Borrows the symbol [`Interner`] and the issuing
    /// [`ResolvedPackage`] so a nominal renders as its path-qualified
    /// source name (`ast.tree.Expr`, spec-mangled `Vec_HExpr`) instead
    /// of the opaque `<nominal module:index>` coordinate. Diagnostics
    /// with a resolved package in scope (the inference pass) should
    /// prefer this; pass `package: None` to get the coordinate fallback.
    pub fn display_cx<'a>(
        &'a self,
        id: TyId,
        interner: &'a Interner,
        package: Option<&'a ResolvedPackage>,
    ) -> TyDisplayCx<'a> {
        TyDisplayCx {
            id,
            ty_interner: self,
            interner,
            package,
        }
    }
}

impl Default for TyInterner {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for TyInterner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.inner.read();
        f.debug_struct("TyInterner")
            .field("len", &state.kinds.len())
            .finish()
    }
}

/// Display adapter returned by [`TyInterner::display`]. Renders a type
/// in Edda source form, recursing through tuple and slice payloads.
pub struct TyDisplay<'a> {
    id: TyId,
    interner: &'a TyInterner,
}

impl<'a> fmt::Display for TyDisplay<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.interner.kind(self.id) {
            TyKind::Primitive(p) => f.write_str(p.name()),
            TyKind::Tuple(elems) => {
                f.write_str("(")?;
                for (i, elem) in elems.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    self.interner.display(*elem).fmt(f)?;
                }
                f.write_str(")")
            }
            TyKind::Slice(elem) => {
                f.write_str("[")?;
                self.interner.display(*elem).fmt(f)?;
                f.write_str("]")
            }
            TyKind::Nominal(binding) => {
                // Without a `ResolvedPackage` + `Interner` we can only
                // render the raw binding handle. Diagnostic paths that
                // want the user-facing name route through a future
                // `TyDisplayCx` that borrows both contexts.
                write!(
                    f,
                    "<nominal {}:{}>",
                    binding.module.as_u32(),
                    binding.index,
                )
            }
            TyKind::Capability(c) => f.write_str(c.name()),
            TyKind::FnPtr(sig) => {
                // Renders `function(<mode> <ty>, ...) -> <return> [with {row}]`.
                // Names are not part of the type so the type-level
                // display deliberately omits them.
                f.write_str("function(")?;
                for (i, p) in sig.params.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    if p.mode != crate::sig::ParamMode::Default {
                        f.write_str(p.mode.keyword())?;
                        f.write_str(" ")?;
                    }
                    self.interner.display(p.ty).fmt(f)?;
                }
                f.write_str(") -> ")?;
                self.interner.display(sig.return_ty).fmt(f)?;
                // Row display needs the symbol interner, which TyDisplay
                // does not hold. Render `{...}` as a marker when present
                // and let diagnostics that need full row text use
                // `FnPtrSig::display` (which does carry both interners).
                if !sig.effects.is_empty() {
                    f.write_str(" uses {...}")?;
                }
                Ok(())
            }
            TyKind::Error => f.write_str("<error>"),
        }
    }
}

/// Display adapter that renders a type in Edda source form *and* names
/// nominal types. Returned by [`TyInterner::display_cx`].
///
/// Unlike [`TyDisplay`] it borrows the symbol [`Interner`] and the
/// [`ResolvedPackage`], so [`TyKind::Nominal`] renders as its
/// path-qualified declared name (`ast.tree.Expr`) rather than the opaque
/// `<nominal module:index>` coordinate. With `package: None` it degrades
/// to the same coordinate form [`TyDisplay`] emits.
pub struct TyDisplayCx<'a> {
    id: TyId,
    ty_interner: &'a TyInterner,
    interner: &'a Interner,
    package: Option<&'a ResolvedPackage>,
}

impl<'a> TyDisplayCx<'a> {
    /// Adapter for a child [`TyId`] carrying the same resolution context,
    /// so nested nominals inside tuple / slice / fn-ptr payloads also
    /// render by name.
    fn child(&self, id: TyId) -> TyDisplayCx<'a> {
        TyDisplayCx {
            id,
            ty_interner: self.ty_interner,
            interner: self.interner,
            package: self.package,
        }
    }
}

impl<'a> fmt::Display for TyDisplayCx<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.ty_interner.kind(self.id) {
            TyKind::Primitive(p) => f.write_str(p.name()),
            TyKind::Tuple(elems) => {
                f.write_str("(")?;
                for (i, elem) in elems.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    self.child(*elem).fmt(f)?;
                }
                f.write_str(")")
            }
            TyKind::Slice(elem) => {
                f.write_str("[")?;
                self.child(*elem).fmt(f)?;
                f.write_str("]")
            }
            TyKind::Nominal(binding) => match self.package {
                Some(pkg) => {
                    let entry = pkg.binding(*binding);
                    let name = self.interner.resolve(entry.name);
                    let module = pkg.module_entry(binding.module);
                    write!(
                        f,
                        "{}.{}",
                        module.canonical_path.display(self.interner),
                        name,
                    )
                }
                None => write!(
                    f,
                    "<nominal {}:{}>",
                    binding.module.as_u32(),
                    binding.index,
                ),
            },
            TyKind::Capability(c) => f.write_str(c.name()),
            TyKind::FnPtr(sig) => {
                f.write_str("function(")?;
                for (i, p) in sig.params.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    if p.mode != crate::sig::ParamMode::Default {
                        f.write_str(p.mode.keyword())?;
                        f.write_str(" ")?;
                    }
                    self.child(p.ty).fmt(f)?;
                }
                f.write_str(") -> ")?;
                self.child(sig.return_ty).fmt(f)?;
                if !sig.effects.is_empty() {
                    f.write_str(" uses {...}")?;
                }
                Ok(())
            }
            TyKind::Error => f.write_str("<error>"),
        }
    }
}

#[cfg(test)]
#[path = "ty_tests.rs"]
mod tests;
