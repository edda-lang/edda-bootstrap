//! Effect-row accumulator threaded through inference per
//! `effect-tracking.md §1` and `inference-rules.md §1a.4`.
//!
//! Inference helpers push entries as they walk the HIR; sub-walk
//! contributions are extracted by recording a [`EffectAcc::checkpoint`]
//! before the recursion and inspecting [`EffectAcc::entries_since`]
//! after. The accumulator is finalised into a canonical [`EffectRow`]
//! at function-body exit (see [`super::check_fn_body`]) so the
//! containment check ⊆ declared-row is a single comparison.

use crate::effect::{EffectEntry, EffectRow, PureEffect};

/// In-progress effect-row accumulator.
///
/// Wraps a `Vec<EffectEntry>` so inference helpers can push without
/// re-canonicalising on every contribution. Sub-row extraction uses
/// [`EffectAcc::checkpoint`] to record a write position and
/// [`EffectAcc::entries_since`] to view entries pushed after that point —
/// the seam `?` propagation (`effect-tracking.md §3`) reaches for.
#[derive(Clone, Debug, Default)]
pub(crate) struct EffectAcc {
    entries: Vec<EffectEntry>,
}

impl EffectAcc {
    /// Construct an empty accumulator.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Push a single entry. Duplicates are NOT collapsed here —
    /// [`EffectAcc::to_row`] canonicalises at finalize time. The rule-side
    /// semantics in `inference-rules.md §1a.4` is *union*; multiset
    /// accumulation in this Vec collapses to set semantics at finalisation.
    pub(crate) fn push(&mut self, entry: EffectEntry) {
        self.entries.push(entry);
    }

    /// Current write position — pass to [`EffectAcc::entries_since`] to
    /// view entries pushed after this point.
    pub(crate) fn checkpoint(&self) -> usize {
        self.entries.len()
    }

    /// View the entries pushed since `cp`. The returned slice borrows
    /// `self`; it remains valid for the lifetime of the borrow.
    pub(crate) fn entries_since(&self, cp: usize) -> &[EffectEntry] {
        debug_assert!(
            cp <= self.entries.len(),
            "EffectAcc::entries_since called with stale checkpoint",
        );
        &self.entries[cp..]
    }

    /// Canonicalise (sort + dedup) the pushed entries and return the
    /// resulting [`EffectRow`]. Does not consume `self` — callers may
    /// continue to push afterwards (the next finalisation will reflect
    /// every entry pushed since construction or the last `clear`).
    pub(crate) fn to_row(&self) -> EffectRow {
        EffectRow::from_entries(self.entries.iter().copied())
    }

    /// Empty the accumulator. Used by [`super::check_fn_body`] to reset
    /// state between nested function bodies.
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// Remove every occurrence of `entry` from the entries pushed at
    /// or after `cp`. Used by `synth_handle` to discharge the handler's
    /// named effect before it propagates past the handler.
    pub(crate) fn discharge_since(&mut self, cp: usize, entry: &EffectEntry) {
        debug_assert!(
            cp <= self.entries.len(),
            "EffectAcc::discharge_since called with stale checkpoint",
        );
        let tail_start = cp;
        let mut write = tail_start;
        for read in tail_start..self.entries.len() {
            if self.entries[read] == *entry {
                continue;
            }
            if read != write {
                self.entries[write] = self.entries[read];
            }
            write += 1;
        }
        self.entries.truncate(write);
    }

    /// Remove every comptime-envelope entry — `panic` and `yield: T` —
    /// from the entries pushed at or after `cp`. Used by the
    /// `comptime` / `comptime { … }` synth arms: the envelope effects
    /// are the only ones a comptime-pure body may perform, and they
    /// discharge at compile time (a comptime `panic` surfaces as a
    /// compile error from the evaluator, not as a runtime `panic`
    /// obligation). Non-envelope entries (`err: T`, capabilities, …)
    /// are left in place — comptime-purity enforcement reports those
    /// separately, and silently dropping them here would mask the
    /// violation from the row-containment check.
    pub(crate) fn discharge_comptime_envelope_since(&mut self, cp: usize) {
        debug_assert!(
            cp <= self.entries.len(),
            "EffectAcc::discharge_comptime_envelope_since called with stale checkpoint",
        );
        let mut write = cp;
        for read in cp..self.entries.len() {
            if matches!(
                self.entries[read],
                EffectEntry::Pure(PureEffect::Panic) | EffectEntry::Pure(PureEffect::Yield(_))
            ) {
                continue;
            }
            if read != write {
                self.entries[write] = self.entries[read];
            }
            write += 1;
        }
        self.entries.truncate(write);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::{EffectEntry, PureEffect};
    use crate::ty::TyInterner;

    #[test]
    fn empty_acc_finalises_to_empty_row() {
        let acc = EffectAcc::new();
        assert_eq!(acc.checkpoint(), 0);
        assert!(acc.to_row().is_empty());
    }

    #[test]
    fn push_then_finalise_canonicalises() {
        let ty = TyInterner::new();
        let a = ty.prim(crate::prim::Primitive::I32);
        let b = ty.prim(crate::prim::Primitive::String);
        let mut acc = EffectAcc::new();
        // Push out-of-order with a duplicate.
        acc.push(EffectEntry::Pure(PureEffect::Err(a)));
        acc.push(EffectEntry::Pure(PureEffect::Panic));
        acc.push(EffectEntry::Pure(PureEffect::Err(b)));
        acc.push(EffectEntry::Pure(PureEffect::Err(a))); // duplicate
        let row = acc.to_row();
        // Three unique entries; canonical order is Panic < Err(a) < Err(b)
        // (by Ord derive on `PureEffect`, then by TyId).
        assert_eq!(row.len(), 3);
        assert!(row.contains(&EffectEntry::Pure(PureEffect::Panic)));
        assert!(row.contains(&EffectEntry::Pure(PureEffect::Err(a))));
        assert!(row.contains(&EffectEntry::Pure(PureEffect::Err(b))));
    }

    #[test]
    fn checkpoint_and_since_isolate_subwalks() {
        let ty = TyInterner::new();
        let a = ty.prim(crate::prim::Primitive::I32);
        let b = ty.prim(crate::prim::Primitive::String);
        let mut acc = EffectAcc::new();
        acc.push(EffectEntry::Pure(PureEffect::Err(a)));
        let cp = acc.checkpoint();
        acc.push(EffectEntry::Pure(PureEffect::Err(b)));
        acc.push(EffectEntry::Pure(PureEffect::Panic));
        let since = acc.entries_since(cp);
        assert_eq!(since.len(), 2);
        assert_eq!(since[0], EffectEntry::Pure(PureEffect::Err(b)));
        assert_eq!(since[1], EffectEntry::Pure(PureEffect::Panic));
    }

    #[test]
    fn clear_resets_state() {
        let ty = TyInterner::new();
        let a = ty.prim(crate::prim::Primitive::I32);
        let mut acc = EffectAcc::new();
        acc.push(EffectEntry::Pure(PureEffect::Err(a)));
        acc.clear();
        assert_eq!(acc.checkpoint(), 0);
        assert!(acc.to_row().is_empty());
    }
}
