//! The per-binding state lattice — [`BindingState`] and its GLB join.
//!
//! [`BindingState::glb`] is the join used at branch merges per
//! `inference-rules.md §4` / `§5`; [`BindingState::field_state`]
//! derives a field's state from a whole-binding state for the §5
//! per-field tracker.

use std::collections::BTreeSet;

use edda_intern::Symbol;

/// State of a single binding tracked by the mode discipline.
#[derive(Clone, Eq, PartialEq, Debug)]
pub(crate) enum BindingState {
    /// Bottom of the lattice — moved out. Reading is a compile error.
    Consumed,
    /// Declared but not initialised. Reading is a compile error.
    Uninit,
    /// `partial-init(F)` per `inference-rules.md §5`. `F` is the set
    /// of fields that are currently **valid**; complement is the set
    /// of fields that are consumed or uninitialised. The whole
    /// binding is *not* readable in this state — only individual
    /// fields in `F` are.
    PartialInit(BTreeSet<Symbol>),
    /// Top of the lattice — fully initialised. Reading is admissible.
    Valid,
}

impl BindingState {
    /// Greatest lower bound on the **valid > uninit > consumed**
    /// lattice, extended with `partial-init` per
    /// `inference-rules.md §5`.
    ///
    /// Cross-state rules:
    /// - `Consumed` absorbs (bottom).
    /// - `Valid glb Valid = Valid`.
    /// - `PartialInit(F1) glb PartialInit(F2) = PartialInit(F1 ∩ F2)`.
    /// - `Valid glb PartialInit(F) = PartialInit(F)` (Valid = "all
    ///   fields valid" = `PartialInit(fields(T))`, so the GLB keeps
    ///   only the explicitly-listed fields).
    /// - `Uninit glb PartialInit(_) = Uninit` (no fields are
    ///   trustably valid after the merge).
    /// - `Uninit glb Valid = Uninit` (unchanged from §4).
    pub fn glb(&self, other: &BindingState) -> BindingState {
        use BindingState::*;
        match (self, other) {
            (Consumed, _) | (_, Consumed) => Consumed,
            (Valid, Valid) => Valid,
            (Valid, PartialInit(f)) | (PartialInit(f), Valid) => PartialInit(f.clone()),
            (Valid, Uninit) | (Uninit, Valid) => Uninit,
            (PartialInit(f1), PartialInit(f2)) => {
                let intersect: BTreeSet<Symbol> = f1.intersection(f2).copied().collect();
                PartialInit(intersect)
            }
            (PartialInit(_), Uninit) | (Uninit, PartialInit(_)) => Uninit,
            (Uninit, Uninit) => Uninit,
        }
    }

    /// Whether the binding **as a whole** may be read at a use site.
    /// Only [`BindingState::Valid`] admits whole-binding reads. A
    /// [`BindingState::PartialInit`] binding cannot be read whole —
    /// individual field reads must consult
    /// [`BindingState::field_state`].
    pub fn is_readable(&self) -> bool {
        matches!(self, BindingState::Valid)
    }

    /// Source-friendly word for diagnostics ("uninitialised",
    /// "consumed", "valid", "partially initialised").
    pub fn describe(&self) -> &'static str {
        match self {
            BindingState::Uninit => "uninitialised",
            BindingState::Valid => "valid",
            BindingState::Consumed => "consumed (moved out)",
            BindingState::PartialInit(_) => "partially initialised",
        }
    }

    /// Derive a field's state from a whole-binding state.
    ///
    /// - `Valid` → every field is `Valid`.
    /// - `Uninit` → every field is `Uninit`.
    /// - `Consumed` → every field is `Consumed`.
    /// - `PartialInit(F)` → field `f` is `Valid` iff `f ∈ F`,
    ///   otherwise `Uninit` (the spec models field-state in the
    ///   complement set; we adopt `Uninit` as the canonical "not
    ///   valid in this partial state" representative because the
    ///   §5 transitions only re-initialise via `M-Field-Assign` /
    ///   `M-Init-Field`).
    pub fn field_state(&self, field: Symbol) -> BindingState {
        match self {
            BindingState::Valid => BindingState::Valid,
            BindingState::Uninit => BindingState::Uninit,
            BindingState::Consumed => BindingState::Consumed,
            BindingState::PartialInit(f) => {
                if f.contains(&field) {
                    BindingState::Valid
                } else {
                    BindingState::Uninit
                }
            }
        }
    }
}
