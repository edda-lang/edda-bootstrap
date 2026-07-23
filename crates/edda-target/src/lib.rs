//! Build-target triples, feature sets, and target-feature gating.
//!
//! Owns the locked triple grammar (`<arch>-<os>-<abi-variant>`) and the
//! feature-name catalogue per architecture. The `target_has(feature)`
//! comptime predicate (consumed by `edda-comptime`) is grounded in this
//! crate's [`TargetCfg`].
//!
//! Implements: `docs/tooling/build-system.md` §9.
//!
//! Bootstrap infrastructure — not a spec'd language feature.
//!
//! # Triple grammar is locked
//!
//! The three component sets in [`Arch`], [`Os`], and [`AbiVariant`] are
//! locked. Adding a value requires reopening that lock — the parser and
//! the cache-hash inputs both treat the locked set as authoritative.
//!
//! # Feature catalogue is arch-scoped
//!
//! Feature names are valid only for specific architectures. The
//! [`known_features`] table is the source of truth; [`FeatureSet::insert`]
//! refuses unknown pairings and surfaces them through the
//! `unknown_target_feature` diagnostic class (build-system.md §8).
//!
//! # Grounding `target_has(feature)`
//!
//! The locked `function target_has(comptime feature: String) -> bool`
//! built-in (build-system.md §9) is evaluated against a [`TargetCfg`].
//! Because the feature name itself can be unknown for the active arch, the
//! Rust ground-truth function [`TargetCfg::target_has`] returns the
//! tri-valued [`TargetHas`] instead of `bool`: comptime resolves `Yes` /
//! `No` to the literal bool and lowers `Unknown` to an
//! `unknown_target_feature` diagnostic.

mod cfg;
mod feature;
mod triple;

pub use cfg::{ArchMismatch, TargetCfg, TargetHas};
pub use feature::{FeatureSet, UnknownFeature, known_features};
pub use triple::{ABI_COUNT, ARCH_COUNT, AbiVariant, Arch, OS_COUNT, Os, TargetTriple, TripleParseError};
