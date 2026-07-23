//! Per-binding state lattice + §4 mode-discipline algorithm + §5
//! per-field tracker.
//!
//! [`BindingState`] is the lattice and [`BindingState::glb`] is the
//! join used at branch merges per `inference-rules.md §4` / `§5`.
//! The algorithm that operates on the lattice — call-arg mode
//! transitions (binding-level and field-level), loop re-entry checks,
//! function-body exit checks, and the canonical `path_binding`
//! helper that extracts a single-segment-Path binding — also lives
//! here so the §4 / §5 machinery is one navigable place.
//!
//! # Module layout
//!
//! - [`lattice`] — the [`BindingState`] enum + its GLB join and
//!   field-state derivation.
//! - [`helpers`] — `path_binding` / `field_projection_binding` place
//!   extraction + `type_field_set`.
//! - [`transitions`] — §4 / §5 call-arg mode transitions + the loop
//!   re-entry check.
//! - [`exit`] — the §4 function-exit check + the droppability predicate.
//! - [`linear`] — the §6 `linear`-unconsumed scope-exit sweep + the
//!   by-value move that consumes a moved `linear` binding.

mod exit;
mod helpers;
mod lattice;
mod linear;
mod transitions;

pub(crate) use lattice::BindingState;

pub(crate) use helpers::{field_projection_binding, path_binding, tuple_index_binding};

pub(crate) use exit::check_fn_body_exit;
pub(crate) use linear::{check_block_exit_linear, consume_moved_linear};
pub(crate) use transitions::{
    apply_call_mode_transition, apply_field_assign_transition, apply_mode_transition,
    apply_struct_field_mode_transition, check_loop_reentry,
};

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
