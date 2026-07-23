//! String interner and `Symbol` handles.
//!
//! Module paths, identifiers, and string literals reach into the interner so
//! the rest of the compiler compares u32-sized handles rather than `&str`.
//!
//! Bootstrap infrastructure — not a spec'd language feature.
//!
//! # Lifetime contract
//!
//! [`Interner::resolve`] returns a `&str` borrowed from the interner itself
//! (lifetime tied to `&self`). This matches rustc's interner style and avoids
//! per-resolve allocation. Callers that need an owned handle across thread or
//! lifetime boundaries should store the [`Symbol`] (`Copy`, 32 bits) and
//! resolve again on demand. The interner is intentionally append-only; once a
//! string is interned, its memory is pinned for the interner's lifetime.
//!
//! # Concurrency
//!
//! [`Interner`] is `Send + Sync`. Internal state is guarded by a
//! [`parking_lot::RwLock`]: reads (lookups, resolves) take the read lock;
//! insertions take the write lock. `intern` takes `&self` via interior
//! mutability so the daemon can share one interner across worker threads.

use ahash::AHashMap;
use parking_lot::RwLock;

/// Opaque 32-bit handle into an [`Interner`].
///
/// `Symbol` is `Copy` and the same size as a `u32`. It carries no string data
/// itself — call [`Interner::resolve`] to recover the original `&str`.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
pub struct Symbol(u32);

impl Symbol {
    /// Reserved sentinel for placeholder slots that have no associated string.
    ///
    /// `DUMMY` is `u32::MAX`, which is outside the range an interner can issue
    /// (a `Vec` cannot hold `u32::MAX + 1` elements on any supported target).
    /// Passing `DUMMY` to [`Interner::resolve`] panics.
    pub const DUMMY: Symbol = Symbol(u32::MAX);

    /// Raw `u32` representation of this symbol.
    ///
    /// Exposed for serialization and debug tooling; do not use as a stable
    /// cross-process identifier — values depend on insertion order.
    #[inline]
    pub fn as_u32(self) -> u32 {
        self.0
    }
}

/// Thread-safe string interner.
///
/// Maps `&str` to compact [`Symbol`] handles and back. Idempotent: interning
/// the same string twice returns the same `Symbol`. Designed to be shared
/// across daemon worker threads behind an `Arc<Interner>`.
pub struct Interner {
    inner: RwLock<InternerState>,
}

struct InternerState {
    /// String -> Symbol index. The key is a `Box<str>` so the map owns its key
    /// memory independent of the `strings` vector (avoids self-referential
    /// borrows).
    lookup: AHashMap<Box<str>, Symbol>,
    /// Symbol index -> interned string. Each `Box<str>` lives on the heap;
    /// growing the `Vec` reallocates the slot array but never moves the
    /// `str` payloads, so previously-returned `&str` references stay valid.
    strings: Vec<Box<str>>,
}

impl Interner {
    /// Construct an empty interner.
    #[inline]
    pub fn new() -> Self {
        Self::with_capacity(0)
    }

    /// Construct an empty interner pre-sized for at least `n` distinct strings.
    pub fn with_capacity(n: usize) -> Self {
        Self {
            inner: RwLock::new(InternerState {
                lookup: AHashMap::with_capacity(n),
                strings: Vec::with_capacity(n),
            }),
        }
    }

    /// Intern `s` and return its [`Symbol`].
    ///
    /// If `s` was already interned, the existing `Symbol` is returned and no
    /// allocation occurs. Otherwise `s` is copied into the interner's owned
    /// storage and assigned the next available index.
    ///
    /// # Panics
    ///
    /// Panics if the interner already holds `u32::MAX` strings (the reserved
    /// space for [`Symbol::DUMMY`]). In practice the compiler will OOM long
    /// before reaching this limit.
    pub fn intern(&self, s: &str) -> Symbol {
        // Fast path: read lock, lookup only. The vast majority of interns hit
        // an existing entry (identifiers repeat heavily in real code).
        if let Some(sym) = self.inner.read().lookup.get(s).copied() {
            return sym;
        }
        self.intern_slow(s)
    }

    /// Write-locked insertion path; double-checks under the write lock to
    /// handle the race where another thread interned `s` between our read
    /// unlock and write lock acquisition.
    #[cold]
    fn intern_slow(&self, s: &str) -> Symbol {
        let mut guard = self.inner.write();
        let state = &mut *guard;
        // Double-check: another thread may have interned `s` while we waited.
        if let Some(sym) = state.lookup.get(s).copied() {
            return sym;
        }
        let idx = state.strings.len();
        assert!(
            idx < (u32::MAX as usize),
            "edda-intern: Interner exhausted (u32::MAX strings)"
        );
        let sym = Symbol(idx as u32);
        // Allocate the owned string once, then store identical `Box<str>`
        // values in both maps. `Box<str>` is cheap to clone-by-copying-the-
        // bytes once, and we never do it again for this string.
        let owned: Box<str> = s.into();
        state.strings.push(owned.clone());
        state.lookup.insert(owned, sym);
        sym
    }

    /// Resolve a symbol to its interned string, returning None for Symbol::DUMMY or any out-of-range handle.
    ///
    /// Defence-in-depth alternative to [`Interner::resolve`] for callers that
    /// may route AST-derived [`Symbol`]s — including the parser-recovery
    /// sentinel [`Symbol::DUMMY`] — through the interner without a prior
    /// guard. Returns `None` instead of panicking when `sym == Symbol::DUMMY`
    /// or `sym.0` exceeds the current interner length; otherwise returns
    /// `Some(&str)` borrowed from the interner under the same heap-pin
    /// contract as [`Interner::resolve`].
    pub fn try_resolve(&self, sym: Symbol) -> Option<&str> {
        if sym == Symbol::DUMMY {
            return None;
        }
        let state = self.inner.read();
        let idx = sym.0 as usize;
        let entry = state.strings.get(idx)?;
        // Take a raw pointer to the heap-allocated `str` payload, then
        // re-borrow it with the `&self` lifetime after the guard drops.
        //
        // SAFETY: The pointed-to bytes live in a `Box<str>` whose heap
        // payload is never moved or freed until the Interner itself is
        // dropped. This holds because:
        //   1. `strings` is append-only — `strings[idx]` is never removed,
        //      replaced, or reordered after assignment.
        //   2. Vec growth reallocates the slot array (pointers), not the
        //      heap payload behind each `Box<str>`. So the `*const str`
        //      remains valid across concurrent `intern` calls.
        //   3. `&self` outlives the dropped read guard; no `&mut self`
        //      method exists that could free the heap memory while the
        //      returned `&str` lives.
        //   4. The interner does not hand out interior `&mut` references
        //      to any interned string, so no aliasing-XOR-mutability rule
        //      is broken.
        let ptr: *const str = &**entry;
        drop(state);
        Some(unsafe { &*ptr })
    }

    /// Resolve `sym` to the `&str` it was interned from.
    ///
    /// The returned reference is borrowed from the interner; it remains valid
    /// for the lifetime of the `&self` borrow even if other threads continue
    /// to intern new strings (interned-string memory is pinned).
    ///
    /// # Panics
    ///
    /// Panics if `sym` was not issued by this interner — including
    /// [`Symbol::DUMMY`] and any `Symbol` from a different `Interner`
    /// instance. The compiler treats this as a logic bug: callers must only
    /// resolve symbols they previously interned.
    pub fn resolve(&self, sym: Symbol) -> &str {
        self.try_resolve(sym).unwrap_or_else(|| {
            if sym == Symbol::DUMMY {
                panic!(
                    "edda-intern: attempted to resolve Symbol::DUMMY — a lexer/parser \
                     recovery sentinel leaked into a resolving pass; the producing pass \
                     should have emitted a diagnostic instead"
                );
            }
            panic!("edda-intern: Symbol({}) is out of range", sym.0)
        })
    }

    /// Number of distinct strings currently interned.
    pub fn len(&self) -> usize {
        self.inner.read().strings.len()
    }

    /// `true` if no strings have been interned yet.
    pub fn is_empty(&self) -> bool {
        self.inner.read().strings.is_empty()
    }

    /// Look up `s` without interning. Returns `Some(Symbol)` if `s` was
    /// previously interned, `None` otherwise.
    pub fn contains(&self, s: &str) -> Option<Symbol> {
        self.inner.read().lookup.get(s).copied()
    }
}

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Interner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.inner.read();
        f.debug_struct("Interner")
            .field("len", &state.strings.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn intern_is_idempotent() {
        let i = Interner::new();
        let a = i.intern("foo");
        let b = i.intern("foo");
        assert_eq!(a, b);
        assert_eq!(i.len(), 1);
    }

    #[test]
    fn resolve_round_trips() {
        let i = Interner::new();
        let cases = [
            "",
            "x",
            "hello",
            "a_long_identifier_name_with_underscores",
            "unicode_\u{1F600}_smiley",
            "multi\nline\tstring",
            "\u{4E2D}\u{6587}\u{6D4B}\u{8BD5}", // Chinese
        ];
        let syms: Vec<Symbol> = cases.iter().map(|s| i.intern(s)).collect();
        for (s, sym) in cases.iter().zip(syms.iter()) {
            assert_eq!(i.resolve(*sym), *s);
        }
    }

    #[test]
    fn distinct_strings_get_distinct_symbols() {
        let i = Interner::new();
        let a = i.intern("alpha");
        let b = i.intern("beta");
        let c = i.intern("gamma");
        assert_ne!(a, b);
        assert_ne!(b, c);
        assert_ne!(a, c);
        assert_eq!(i.len(), 3);
    }

    #[test]
    fn len_and_is_empty_track_inserts() {
        let i = Interner::new();
        assert!(i.is_empty());
        assert_eq!(i.len(), 0);
        i.intern("one");
        assert!(!i.is_empty());
        assert_eq!(i.len(), 1);
        i.intern("one"); // duplicate must not grow len
        assert_eq!(i.len(), 1);
        i.intern("two");
        assert_eq!(i.len(), 2);
    }

    #[test]
    fn contains_does_not_insert() {
        let i = Interner::new();
        assert!(i.contains("nope").is_none());
        assert_eq!(i.len(), 0);
        let sym = i.intern("yep");
        assert_eq!(i.contains("yep"), Some(sym));
        assert!(i.contains("still_nope").is_none());
        assert_eq!(i.len(), 1);
    }

    #[test]
    fn pointer_stability_under_growth() {
        let i = Interner::new();
        let first = i.intern("anchor");
        // Borrow the resolved &str and hold it across many subsequent inserts
        // that will force the lookup map and strings Vec to reallocate.
        let s_ref: &str = i.resolve(first);
        for n in 0..4096_u32 {
            i.intern(&format!("filler_{n}"));
        }
        // The original &str must still read the correct content.
        assert_eq!(s_ref, "anchor");
        // And the symbol still resolves correctly.
        assert_eq!(i.resolve(first), "anchor");
    }

    #[test]
    fn symbol_dummy_is_reserved() {
        assert_eq!(Symbol::DUMMY.as_u32(), u32::MAX);
        let i = Interner::new();
        // No matter what we intern, we never receive DUMMY.
        for n in 0..16_u32 {
            let s = i.intern(&format!("k_{n}"));
            assert_ne!(s, Symbol::DUMMY);
        }
    }

    #[test]
    #[should_panic(expected = "Symbol::DUMMY")]
    fn resolve_panics_on_dummy() {
        let i = Interner::new();
        let _ = i.resolve(Symbol::DUMMY);
    }

    #[test]
    fn try_resolve_returns_none_for_dummy() {
        let i = Interner::new();
        assert!(i.try_resolve(Symbol::DUMMY).is_none());
    }

    #[test]
    fn try_resolve_returns_some_for_interned() {
        let i = Interner::new();
        let sym = i.intern("hello");
        assert_eq!(i.try_resolve(sym), Some("hello"));
    }

    #[test]
    fn try_resolve_returns_none_for_out_of_range() {
        let i = Interner::new();
        // Empty interner; Symbol(42) is well beyond any valid handle.
        assert!(i.try_resolve(Symbol(42)).is_none());
        // After interning one string, Symbol(0) is valid but Symbol(1) is not.
        let sym = i.intern("only");
        assert_eq!(sym, Symbol(0));
        assert!(i.try_resolve(Symbol(1)).is_none());
    }

    #[test]
    fn thread_safety_concurrent_interning() {
        const THREADS: usize = 4;
        const STRINGS_PER_THREAD: usize = 100;

        // Build the input set: STRINGS_PER_THREAD unique strings, each
        // interned by every thread.
        let inputs: Arc<Vec<String>> = Arc::new(
            (0..STRINGS_PER_THREAD)
                .map(|n| format!("ident_{n}"))
                .collect(),
        );
        let interner = Arc::new(Interner::new());

        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let interner = Arc::clone(&interner);
                let inputs = Arc::clone(&inputs);
                thread::spawn(move || {
                    inputs
                        .iter()
                        .map(|s| interner.intern(s))
                        .collect::<Vec<Symbol>>()
                })
            })
            .collect();

        let results: Vec<Vec<Symbol>> =
            handles.into_iter().map(|h| h.join().unwrap()).collect();

        // All threads must agree on the Symbol assigned to each input.
        let first = &results[0];
        for other in &results[1..] {
            assert_eq!(first, other, "threads disagreed on Symbol assignment");
        }

        // No duplicates: the interner holds exactly STRINGS_PER_THREAD entries.
        assert_eq!(interner.len(), STRINGS_PER_THREAD);

        // And every Symbol round-trips to its input.
        for (s, sym) in inputs.iter().zip(first.iter()) {
            assert_eq!(interner.resolve(*sym), s.as_str());
        }
    }
}
