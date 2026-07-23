//! Property-based-testing generator strategies for `edda test
//! --properties`.
//!
//! Per `corpus/edda-codex/language/03-verification.md` §6
//! (Property-based testing — refinements as runnable properties),
//! every `requires` and `ensures` is a runnable property. This module
//! owns the **generator strategy table** — given a function's
//! parameter and its `requires` clauses, decide which input-generator
//! shape to use and produce N concrete values.
//!
//! ## Strategy table
//!
//! | Refinement form  | Strategy                              |
//! |------------------|---------------------------------------|
//! | `x == c`         | [`Strategy::Constant`]                |
//! | `x >= c` / `x > c` | [`Strategy::IntRange`] half-bounded |
//! | `x <= c` / `x < c` | [`Strategy::IntRange`] half-bounded |
//! | `x in lo..<hi`   | [`Strategy::IntRange`] bounded        |
//! | (no refinement)  | [`Strategy::IntRange`] full-width      |
//! | bool param       | [`Strategy::BoolValue`]               |
//! | other            | [`Strategy::Unanalyzable`]            |
//!
//! Compositional generators (lists, tuples, sums) and the
//! Z3-model-enumeration fallback for uninterpreted predicates are
//! intentional follow-ups — the success criterion the runner closes
//! (`requires n > 0 ensures result >= 1`) needs only the integer
//! half-bounded form. The strategy enum is shaped so adding variants
//! does not perturb existing callers.
//!
//! ## Output shape
//!
//! Each strategy generates a [`Value`] — a concrete primitive that
//! the C9 runner serialises into a synthesised Edda call site
//! (literally as a token in the generated test module's source). The
//! runner takes the Cartesian product across a function's params
//! and emits one call per tuple of values.
//!
//! ## Module layout
//!
//! - [`value`] — the concrete [`Value`] model.
//! - [`strategy`] — the [`Strategy`] enum and [`generate_values`].
//! - [`analyse`] — `requires`-clause → [`Strategy`] analysis.
//! - [`discover`] — [`PropertyTarget`] discovery + predicate rendering.
//! - [`synth`] — runner-module source synthesis and disk emission.

mod analyse;
mod discover;
mod strategy;
mod synth;
mod value;

// The cascade consumes these four entry points directly.
pub use discover::discover_targets;
pub use synth::{runner_module_path, synthesize_runner_source, write_runner_module};

// Stable public surface of the property-test analyser. Re-exported at
// the module root so the analyser's types resolve through
// `crate::properties::*`; `analyse_param` / `generate_values` /
// `IntPrim` are exercised by the submodule test suites rather than the
// cascade, so the re-exports are allowed to be unused by non-test code.
#[allow(unused_imports)]
pub use analyse::{IntPrim, analyse_param};
#[allow(unused_imports)]
pub use discover::PropertyTarget;
#[allow(unused_imports)]
pub use strategy::{Strategy, generate_values};
#[allow(unused_imports)]
pub use value::Value;
