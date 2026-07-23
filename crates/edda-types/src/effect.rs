//! Effect-row representation per `docs/types/effect-tracking.md`.
//!
//! Rows are *closed sets* per `effect-tracking.md §1` / `§2`: two rows
//! are equal iff they contain the same set of entries; there is no row
//! variable, no width subtyping, no kind variance. For internal storage
//! [`EffectRow`] keeps its entries in a sorted, deduplicated
//! `Box<[EffectEntry]>`, so:
//!
//! - Structural equality reduces to slice equality (the `Eq` derive).
//! - [`EffectRow::union`] reduces to a single merge pass.
//! - [`EffectRow::contains`] is `O(log n)` via binary search.
//! - Hashing is well-defined (entries hash in canonical order).
//!
//! `EffectEntry`'s `Ord` derive determines the canonical order:
//! capability entries (variant index 0) come before pure-effect entries
//! (variant index 1); capabilities sort by `Symbol` u32 (insertion-order
//! identity, stable within a build); pure effects sort by
//! `PureEffect`'s declared variant order (`Panic` < `Err` < `Yield` <
//! `Divergence` < `Cancellation` < `Nondet`), breaking `Err` / `Yield`
//! ties by payload [`TyId`]. The order is
//! deterministic for any given interner state; consumers that need
//! source-order rendering must do their own pass over the source AST.
//!
//! [`GradedBound`] entries live on [`FnSig::graded_bounds`](crate::FnSig)
//! and are *not* part of the canonical [`EffectRow`]. The row tracks
//! kind set-membership; the bound expression is signature-level data
//! that participates in call-site discharge per
//! `corpus/edda-codex/language/02-modes-effects-refinements.md` §5.

use std::fmt;

use edda_intern::{Interner, Symbol};
use edda_span::Span;
use edda_syntax::ast;

use crate::ty::{TyId, TyInterner};

/// One of the six locked pure-effect kinds.
///
/// Per `effect-tracking.md §4`, each originator pairs with an
/// effect kind: `raise <expr>` → `Err`, `panic <expr>` → `Panic`,
/// `yield <expr>` → `Yield`, a recursive function or unbounded
/// `loop` without a `decreases` measure → `Divergence` per
/// `corpus/edda-codex/language/03-verification.md` §5, `.await`
/// → `Cancellation` per `05-concurrency-coherence.md` §2.2, and a
/// `scope(exec)` body using `group.race` / `group.any` (or an
/// ambient `Random` draw) → `Nondet` per
/// `05-concurrency-coherence.md` §"`nondet` effect for parallelism"
/// / §4.2.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
pub enum PureEffect {
    /// The bare `panic` effect — no payload. Originated by
    /// `panic <expr>`; the message expression is operational metadata,
    /// not a typed payload (`effect-tracking.md §4`).
    Panic,
    /// `err: T` — originated by `raise <expr>` of type `T`.
    /// Propagated by `?` per `effect-tracking.md §3`.
    Err(TyId),
    /// `yield: T` — originated by `yield <expr>` of type `T`.
    /// Consumed by `for x in producer()` per `effect-tracking.md §6`.
    Yield(TyId),
    /// `divergence` — bare, payload-less. Admitted as positive
    /// admission for a function that does not write `decreases` and
    /// cannot be proven to terminate by other means; also injected by
    /// the inference pass on every unbounded `loop` without a
    /// `decreases` measure. Per `03-verification.md` §5. Propagates
    /// to callers; bounded by `handle divergence -> <fallback> { … }`
    /// per `02-modes-effects-refinements.md:437`.
    Divergence,
    /// `cancellation` — bare, payload-less. Originated by `.await`
    /// per `05-concurrency-coherence.md` §2.2 (*"`await`'s row is
    /// `{cancellation}` only"*); propagates to the enclosing
    /// function's declared row like any other pure effect. No
    /// `handle cancellation -> ...` discharge form exists yet
    /// (handlers admit only `err: T` so far) — cancellation
    /// currently propagates unconditionally to every caller of an
    /// `.await`-performing function.
    Cancellation,
    /// `nondet` — bare, payload-less. Originated by a `scope(exec)`
    /// body that uses `group.race` / `group.any` per
    /// `05-concurrency-coherence.md` §"`nondet` effect for
    /// parallelism", and by every ambient `Random` draw (the seeded
    /// `DeterministicRandom` narrowing does *not* originate it — its
    /// draws are a pure function of the seed). Verification-only, like
    /// `Divergence` / `Cancellation`: propagates into the enclosing
    /// declared row and is excluded from the §7 stable effect-row
    /// whitelist (observable non-determinism breaks
    /// equal-inputs-equal-outputs), but carries no runtime payload —
    /// MIR lowering emits no code and no ABI slot for it.
    Nondet,
}

/// A single entry inside an [`EffectRow`].
///
/// Two visually distinguishable shapes in source (`effect-tracking.md §1`):
/// a bare identifier (capability) or an identifier-with-`:` (pure
/// effect with payload). Alias inclusion via spread (`...X`) is a
/// distinct surface form and is resolved at lowering time — by the
/// time a row reaches `EffectRow` the spread has been expanded.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
pub enum EffectEntry {
    /// A capability entry — names a parameter binding in the enclosing
    /// function's signature (`effect-tracking.md §1`, *Capability entry*).
    /// Matching is by parameter identity (the `Symbol`), not by
    /// capability type — see `§2`, *Capability entries match by
    /// parameter identity*.
    Capability(Symbol),
    /// A pure-effect entry — a locked kind plus an optional payload type.
    Pure(PureEffect),
}

/// A closed effect row — a set of [`EffectEntry`]s per
/// `effect-tracking.md §1`.
///
/// `EffectRow` is value-typed and cheap to clone (one `Box<[…]>`
/// allocation per non-empty row). Equality, hashing, and ordering are
/// derived from the canonical sorted form, so `row1 == row2` is set
/// equality without any normalisation step at the call site.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct EffectRow {
    entries: Box<[EffectEntry]>,
}

impl EffectRow {
    /// Construct the empty row (`with {}` in source).
    ///
    /// Pure functions have the empty row; in source, an empty row is
    /// omitted (`effect-tracking.md §1`, *Preliminaries*).
    pub fn empty() -> Self {
        Self {
            entries: Box::from([]),
        }
    }

    /// Build a row from any iterable of entries.
    ///
    /// Sorts and deduplicates — duplicate entries silently collapse
    /// (`effect-tracking.md §1`, *Row entries are unique*). Callers may
    /// pass entries in any order; the resulting row is canonical.
    pub fn from_entries(entries: impl IntoIterator<Item = EffectEntry>) -> Self {
        let mut v: Vec<EffectEntry> = entries.into_iter().collect();
        v.sort_unstable();
        v.dedup();
        Self {
            entries: v.into_boxed_slice(),
        }
    }

    /// Borrow the entries in canonical (sorted, deduplicated) order.
    #[inline]
    pub fn entries(&self) -> &[EffectEntry] {
        &self.entries
    }

    /// Number of entries (after dedup).
    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` iff the row is empty (no entries).
    ///
    /// Equivalent to `self == &EffectRow::empty()` and to
    /// `self.len() == 0`.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `true` iff `entry` is in this row.
    ///
    /// Runs in `O(log n)` via binary search on the canonical form.
    pub fn contains(&self, entry: &EffectEntry) -> bool {
        self.entries.binary_search(entry).is_ok()
    }

    /// Set union — every entry of `self` plus every entry of `other`,
    /// canonicalised.
    ///
    /// This is the row-side combinator used by `T-FunCall` and
    /// `T-MethodCall` (`inference-rules.md §1a.4`) when joining a
    /// callee's row with each argument's row.
    pub fn union(&self, other: &EffectRow) -> EffectRow {
        if other.is_empty() {
            return self.clone();
        }
        if self.is_empty() {
            return other.clone();
        }
        // Merge two sorted slices, dropping duplicates.
        let mut merged = Vec::with_capacity(self.entries.len() + other.entries.len());
        let mut i = 0;
        let mut j = 0;
        while i < self.entries.len() && j < other.entries.len() {
            let a = &self.entries[i];
            let b = &other.entries[j];
            match a.cmp(b) {
                std::cmp::Ordering::Less => {
                    merged.push(*a);
                    i += 1;
                }
                std::cmp::Ordering::Greater => {
                    merged.push(*b);
                    j += 1;
                }
                std::cmp::Ordering::Equal => {
                    merged.push(*a);
                    i += 1;
                    j += 1;
                }
            }
        }
        merged.extend_from_slice(&self.entries[i..]);
        merged.extend_from_slice(&other.entries[j..]);
        EffectRow {
            entries: merged.into_boxed_slice(),
        }
    }

    /// Returns a [`fmt::Display`] adapter that renders this row as
    /// `{a, b, err: T, panic, yield: U}` — the brace form the parser
    /// accepts after `with`.
    ///
    /// Borrows both interners so symbols can be resolved and payload
    /// type ids displayed. Empty rows render as `{}`.
    pub fn display<'a>(
        &'a self,
        interner: &'a Interner,
        ty_interner: &'a TyInterner,
    ) -> EffectRowDisplay<'a> {
        EffectRowDisplay {
            row: self,
            interner,
            ty_interner,
        }
    }
}

impl Default for EffectRow {
    fn default() -> Self {
        Self::empty()
    }
}

/// Display adapter returned by [`EffectRow::display`].
///
/// Renders a canonical-order representation suitable for diagnostics:
/// capabilities first (sorted by `Symbol` id), then pure effects
/// (`panic` first, then `err: T`, then `yield: T`, then `divergence`,
/// then `cancellation`, then `nondet` entries).
pub struct EffectRowDisplay<'a> {
    row: &'a EffectRow,
    interner: &'a Interner,
    ty_interner: &'a TyInterner,
}

impl<'a> fmt::Display for EffectRowDisplay<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("{")?;
        for (i, entry) in self.row.entries.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            match entry {
                EffectEntry::Capability(sym) => f.write_str(self.interner.resolve(*sym))?,
                EffectEntry::Pure(PureEffect::Panic) => f.write_str("panic")?,
                EffectEntry::Pure(PureEffect::Err(t)) => {
                    f.write_str("err: ")?;
                    self.ty_interner.display(*t).fmt(f)?;
                }
                EffectEntry::Pure(PureEffect::Yield(t)) => {
                    f.write_str("yield: ")?;
                    self.ty_interner.display(*t).fmt(f)?;
                }
                EffectEntry::Pure(PureEffect::Divergence) => f.write_str("divergence")?,
                EffectEntry::Pure(PureEffect::Cancellation) => f.write_str("cancellation")?,
                EffectEntry::Pure(PureEffect::Nondet) => f.write_str("nondet")?,
            }
        }
        f.write_str("}")
    }
}

/// One of the three locked graded resource kinds.
///
/// Per `02-modes-effects-refinements.md` §5.2, three resources admit
/// graded bounds in v0.1: `alloc(bytes <= N)`, `io(calls <= N)`,
/// `time(ops <= N)`. Additional kinds (e.g., wall-clock time) are
/// reserved for v1.0.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
pub enum GradedKind {
    /// `alloc(bytes <= N)` — body allocates at most `N` bytes through
    /// held `Allocator` capabilities, summed over all paths.
    Alloc,
    /// `io(calls <= N)` — body makes at most `N` external I/O calls.
    Io,
    /// `time(ops <= N)` — body executes at most `N` counted operations.
    Time,
}

impl GradedKind {
    /// Parse a kind name (`"alloc"`, `"io"`, `"time"`). Returns `None`
    /// for any other identifier — the typechecker rejects unknown kinds
    /// with `effect_graded_bound_exceeded`.
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "alloc" => Some(Self::Alloc),
            "io" => Some(Self::Io),
            "time" => Some(Self::Time),
            _ => None,
        }
    }

    /// Lowercase source spelling of this kind.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Alloc => "alloc",
            Self::Io => "io",
            Self::Time => "time",
        }
    }

    /// The resource-variable name conventionally appearing on the LHS
    /// of the bound: `bytes` for [`Alloc`](Self::Alloc), `calls` for
    /// [`Io`](Self::Io), `ops` for [`Time`](Self::Time). Per
    /// `02-modes-effects-refinements.md` §5.2 / §5.3.
    pub const fn resource_var(self) -> &'static str {
        match self {
            Self::Alloc => "bytes",
            Self::Io => "calls",
            Self::Time => "ops",
        }
    }
}

/// One graded-bound entry on a function signature.
///
/// Lives on [`FnSig::graded_bounds`](crate::FnSig::graded_bounds);
/// **not** an [`EffectEntry`] (the row tracks membership, the bound
/// lives on the signature). The bound expression is the
/// already-extracted RHS of `<resource_var> <= EXPR` (the kind's
/// resource variable is implicit, available via
/// [`GradedKind::resource_var`]).
///
/// Equality is structural over `(kind, bound, span)`. Span participates
/// because two functions with the same bound expression from different
/// source positions are distinguishable signatures; this matches the
/// rest of `FnSig`'s span-sensitive comparison behavior.
///
/// Hash deliberately omits `bound` because `ast::Expr` is not `Hash`.
/// Two `GradedBound`s with the same `(kind, span)` but distinct
/// `bound` expressions hash equal yet compare unequal — a hash
/// collision that `PartialEq` resolves correctly downstream.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct GradedBound {
    /// Graded resource kind.
    pub kind: GradedKind,
    /// RHS expression of the source-level `<resource_var> <= EXPR`
    /// bound. Stored as an AST node so the call-site discharge pass
    /// can lift it to a [`Predicate`](edda_refine::Predicate) against
    /// the typechecker's per-function environment.
    pub bound: Box<ast::Expr>,
    /// Source location of the entire `kind(<bound>)` row entry — used
    /// for the diagnostic primary label.
    pub span: Span,
}

impl std::hash::Hash for GradedBound {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.kind.hash(state);
        self.span.hash(state);
        // `bound` is intentionally omitted — `ast::Expr` is not `Hash`.
        // See type-level doc above.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prim::Primitive;

    fn intern_sym(interner: &Interner, text: &str) -> Symbol {
        interner.intern(text)
    }

    #[test]
    fn empty_row_is_canonical() {
        let r = EffectRow::empty();
        assert_eq!(r.len(), 0);
        assert!(r.is_empty());
        assert_eq!(r, EffectRow::default());
        assert_eq!(r, EffectRow::from_entries(std::iter::empty()));
    }

    #[test]
    fn from_entries_sorts_and_deduplicates() {
        let interner = Interner::new();
        let ty = TyInterner::new();
        let fs = intern_sym(&interner, "fs");
        let alloc = intern_sym(&interner, "allocator");
        let i32_id = ty.prim(Primitive::I32);

        // Mixed-order input with a duplicate.
        let r = EffectRow::from_entries([
            EffectEntry::Pure(PureEffect::Err(i32_id)),
            EffectEntry::Capability(fs),
            EffectEntry::Capability(alloc),
            EffectEntry::Pure(PureEffect::Panic),
            EffectEntry::Capability(fs), // duplicate
        ]);

        // 3 unique entries, sorted: capabilities (by Symbol id) then
        // pure effects (Panic before Err per derived Ord).
        assert_eq!(r.len(), 4);
        let entries = r.entries();
        // First two entries are capabilities (variant index 0).
        assert!(matches!(entries[0], EffectEntry::Capability(_)));
        assert!(matches!(entries[1], EffectEntry::Capability(_)));
        // Then pure effects.
        assert!(matches!(entries[2], EffectEntry::Pure(PureEffect::Panic)));
        assert!(matches!(entries[3], EffectEntry::Pure(PureEffect::Err(_))));
    }

    #[test]
    fn equality_is_set_equality() {
        let interner = Interner::new();
        let fs = intern_sym(&interner, "fs");
        let alloc = intern_sym(&interner, "allocator");

        let a = EffectRow::from_entries([
            EffectEntry::Capability(fs),
            EffectEntry::Capability(alloc),
        ]);
        let b = EffectRow::from_entries([
            EffectEntry::Capability(alloc),
            EffectEntry::Capability(fs),
        ]);
        assert_eq!(a, b);

        // Distinct sets are not equal.
        let c = EffectRow::from_entries([EffectEntry::Capability(fs)]);
        assert_ne!(a, c);
    }

    #[test]
    fn contains_uses_binary_search() {
        let interner = Interner::new();
        let ty = TyInterner::new();
        let fs = intern_sym(&interner, "fs");
        let alloc = intern_sym(&interner, "allocator");
        let i32_id = ty.prim(Primitive::I32);
        let str_id = ty.prim(Primitive::String);

        let r = EffectRow::from_entries([
            EffectEntry::Capability(fs),
            EffectEntry::Pure(PureEffect::Panic),
            EffectEntry::Pure(PureEffect::Err(i32_id)),
        ]);
        assert!(r.contains(&EffectEntry::Capability(fs)));
        assert!(r.contains(&EffectEntry::Pure(PureEffect::Panic)));
        assert!(r.contains(&EffectEntry::Pure(PureEffect::Err(i32_id))));
        assert!(!r.contains(&EffectEntry::Capability(alloc)));
        assert!(!r.contains(&EffectEntry::Pure(PureEffect::Err(str_id))));
        assert!(!r.contains(&EffectEntry::Pure(PureEffect::Yield(i32_id))));
    }

    #[test]
    fn union_preserves_canonical_form() {
        let interner = Interner::new();
        let ty = TyInterner::new();
        let fs = intern_sym(&interner, "fs");
        let alloc = intern_sym(&interner, "allocator");
        let i32_id = ty.prim(Primitive::I32);

        let a = EffectRow::from_entries([
            EffectEntry::Capability(fs),
            EffectEntry::Pure(PureEffect::Err(i32_id)),
        ]);
        let b = EffectRow::from_entries([
            EffectEntry::Capability(alloc),
            EffectEntry::Pure(PureEffect::Err(i32_id)),
            EffectEntry::Pure(PureEffect::Panic),
        ]);
        let u = a.union(&b);
        // 4 unique entries — the duplicated `Err(i32_id)` collapses.
        assert_eq!(u.len(), 4);
        assert!(u.contains(&EffectEntry::Capability(fs)));
        assert!(u.contains(&EffectEntry::Capability(alloc)));
        assert!(u.contains(&EffectEntry::Pure(PureEffect::Err(i32_id))));
        assert!(u.contains(&EffectEntry::Pure(PureEffect::Panic)));
        // Commutative.
        assert_eq!(u, b.union(&a));
    }

    #[test]
    fn union_with_empty_is_identity() {
        let interner = Interner::new();
        let fs = intern_sym(&interner, "fs");
        let a = EffectRow::from_entries([EffectEntry::Capability(fs)]);
        assert_eq!(a.union(&EffectRow::empty()), a);
        assert_eq!(EffectRow::empty().union(&a), a);
        let e = EffectRow::empty();
        assert_eq!(e.union(&EffectRow::empty()), EffectRow::empty());
    }

    #[test]
    fn display_renders_canonical_form() {
        let interner = Interner::new();
        let ty = TyInterner::new();
        // Intern in a specific order so the Symbol ids are predictable.
        let alloc = intern_sym(&interner, "allocator"); // Symbol(0)
        let fs = intern_sym(&interner, "fs"); // Symbol(1)
        let io_err = ty.prim(Primitive::U64); // arbitrary type for the err payload
        let u8_id = ty.prim(Primitive::U8);

        let r = EffectRow::from_entries([
            EffectEntry::Pure(PureEffect::Yield(u8_id)),
            EffectEntry::Capability(fs),
            EffectEntry::Pure(PureEffect::Panic),
            EffectEntry::Pure(PureEffect::Err(io_err)),
            EffectEntry::Capability(alloc),
        ]);
        let s = r.display(&interner, &ty).to_string();
        // Capabilities first (sorted by Symbol id: allocator=0, fs=1),
        // then pure: Panic < Err < Yield (declared variant order).
        assert_eq!(s, "{allocator, fs, panic, err: u64, yield: u8}");
    }

    #[test]
    fn display_renders_cancellation_after_divergence() {
        let interner = Interner::new();
        let ty = TyInterner::new();
        let r = EffectRow::from_entries([
            EffectEntry::Pure(PureEffect::Nondet),
            EffectEntry::Pure(PureEffect::Cancellation),
            EffectEntry::Pure(PureEffect::Divergence),
            EffectEntry::Pure(PureEffect::Panic),
        ]);
        let s = r.display(&interner, &ty).to_string();
        // Panic < Divergence < Cancellation < Nondet (declared variant order).
        assert_eq!(s, "{panic, divergence, cancellation, nondet}");
    }

    #[test]
    fn display_renders_empty_braces() {
        let interner = Interner::new();
        let ty = TyInterner::new();
        assert_eq!(EffectRow::empty().display(&interner, &ty).to_string(), "{}");
    }

    #[test]
    fn err_payload_differentiates_entries() {
        let ty = TyInterner::new();
        let a = ty.prim(Primitive::I32);
        let b = ty.prim(Primitive::String);
        let r = EffectRow::from_entries([
            EffectEntry::Pure(PureEffect::Err(a)),
            EffectEntry::Pure(PureEffect::Err(b)),
        ]);
        // Two distinct error types — both retained.
        assert_eq!(r.len(), 2);
        assert!(r.contains(&EffectEntry::Pure(PureEffect::Err(a))));
        assert!(r.contains(&EffectEntry::Pure(PureEffect::Err(b))));
    }
}
