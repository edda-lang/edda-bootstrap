//! Typed-index vector and the `Idx` trait that gates which handles may key
//! into one.
//!
//! `IndexVec<I, T>` is the workhorse container for the MIR's per-body and
//! per-program arenas: locals, basic blocks, ADTs, constants, and bodies all
//! live in their own `IndexVec` instances. The phantom `I` parameter prevents
//! cross-arena ID confusion at the type level — handing a `BlockId` to a
//! method that expects a `LocalId` does not compile.

use std::marker::PhantomData;
use std::ops::{Index, IndexMut};

pub(crate) mod sealed {
    /// Crate-private supertrait sealing [`super::Idx`]: external crates can
    /// reference `*Id` handles but cannot implement `Idx` for their own
    /// types — preserves the `Idx::new(n).index() == n` round-trip invariant
    /// that every internal walker relies on.
    pub trait Sealed {}
}

/// Trait implemented by every ID type that keys into an [`IndexVec`].
pub trait Idx: sealed::Sealed + Copy + Eq + std::hash::Hash + std::fmt::Debug {
    /// Build an ID from its raw `usize` index.
    fn new(idx: usize) -> Self;
    /// Recover the raw `usize` index this ID stands for.
    fn index(self) -> usize;
}

/// Append-only vector keyed by a typed [`Idx`] handle.
///
/// The phantom `fn(I) -> I` makes the parameter invariant — `IndexVec<A, T>`
/// is not coercible to `IndexVec<B, T>` even when `A` and `B` share a layout.
pub struct IndexVec<I: Idx, T> {
    raw: Vec<T>,
    _marker: PhantomData<fn(I) -> I>,
}

impl<I: Idx, T> IndexVec<I, T> {
    /// Construct an empty arena.
    pub fn new() -> Self {
        IndexVec {
            raw: Vec::new(),
            _marker: PhantomData,
        }
    }

    /// Construct an empty arena pre-sized to hold at least `n` entries.
    pub fn with_capacity(n: usize) -> Self {
        IndexVec {
            raw: Vec::with_capacity(n),
            _marker: PhantomData,
        }
    }

    /// Append `value` and return the [`Idx`] that now points to it.
    pub fn push(&mut self, value: T) -> I {
        let idx = self.raw.len();
        self.raw.push(value);
        I::new(idx)
    }

    /// Number of entries currently in the arena.
    pub fn len(&self) -> usize {
        self.raw.len()
    }

    /// Whether the arena has zero entries.
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// Iterate the entries in insertion order.
    pub fn iter(&self) -> std::slice::Iter<'_, T> {
        self.raw.iter()
    }

    /// Iterate the entries mutably in insertion order.
    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, T> {
        self.raw.iter_mut()
    }

    /// Iterate `(I, &T)` pairs in insertion order — the typed counterpart to
    /// `slice::iter().enumerate()`.
    pub fn iter_enumerated(&self) -> impl Iterator<Item = (I, &T)> {
        self.raw.iter().enumerate().map(|(i, t)| (I::new(i), t))
    }

    /// Fallible accessor; returns `None` when `id.index()` is out of range.
    pub fn get(&self, id: I) -> Option<&T> {
        self.raw.get(id.index())
    }

    /// Fallible mutable accessor; returns `None` when `id.index()` is out of range.
    pub fn get_mut(&mut self, id: I) -> Option<&mut T> {
        self.raw.get_mut(id.index())
    }
}

impl<I: Idx, T> Default for IndexVec<I, T> {
    fn default() -> Self {
        IndexVec::new()
    }
}

impl<I: Idx, T> Index<I> for IndexVec<I, T> {
    type Output = T;

    fn index(&self, id: I) -> &T {
        &self.raw[id.index()]
    }
}

impl<I: Idx, T> IndexMut<I> for IndexVec<I, T> {
    fn index_mut(&mut self, id: I) -> &mut T {
        &mut self.raw[id.index()]
    }
}

impl<I: Idx, T: std::fmt::Debug> std::fmt::Debug for IndexVec<I, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.raw.iter()).finish()
    }
}

impl<I: Idx, T: Clone> Clone for IndexVec<I, T> {
    fn clone(&self) -> Self {
        IndexVec {
            raw: self.raw.clone(),
            _marker: PhantomData,
        }
    }
}

impl<I: Idx, T: PartialEq> PartialEq for IndexVec<I, T> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<I: Idx, T: Eq> Eq for IndexVec<I, T> {}
