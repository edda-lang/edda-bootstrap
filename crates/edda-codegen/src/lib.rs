//! Spec instantiation and monomorphization.
//!
//! Expands `spec` invocations into materialized `.ea` artifacts written to
//! the content-addressed cache. This is the "codegen" pass in Edda's
//! vocabulary (parse → import-resolve → typecheck → **codegen** →
//! compile → link). The result of this pass is Edda source, not LLVM
//! IR — `edda-compile` handles the LLVM step. Materialization is
//! reachability-driven: only artifacts reachable from the active
//! command's root set are written.
//!
//! Implements the corpus codegen surface:
//!   - `corpus/edda-codex/language/04-specs-comptime.md` (spec language,
//!     canonical forms, monomorphization)
//!   - `corpus/edda-codex/language/07-distribution.md` (storage layout,
//!     manifest schema, content addressing)
//!   - `corpus/edda-codex/language/06-tooling.md` (cascade build,
//!     reachability-driven codegen)
//!
//! # Current state
//!
//! As of 2026-05-18 the crate ships:
//!
//! - Canonical-form hashing — [`CanonicalForm`] / [`NestedDependency`],
//!   deterministic byte encoder ([`encode`]) at `BodyVersion(0x01)`,
//!   BLAKE3 entry point ([`compute_hash`]).
//! - Argument tuples for all five kinds — Type, EffectRow,
//!   Primitive, UserDefined, Function.
//! - Full AST byte encoding ([`body::Encoder`]) — every expression,
//!   statement, pattern, item, leaf-enum, with locked one-byte tag
//!   tables ([`body::tags`]).
//! - Mangling grammar ([`mangle_short_name`]) + artifact-name
//!   composer ([`artifact_name`]).
//! - Cascade machinery — [`CodegenSession`] driving the
//!   `begin_cascade` → stage → atomic commit lifecycle;
//!   [`CascadeGraph`] + [`walk_cascade`] for the reverse-reachable
//!   traversal in upstream-first topological order; cycle detection.
//! - Substitution + emission — [`SubstitutionMap`],
//!   [`substitute_spec_body`], [`emit_items`].
//! - End-to-end per-invocation instantiation ([`instantiate_spec`]).
//! - Sibling-spec mangled-name substitution, parent-import inheritance
//!   in generated artifacts, qualified-form rewrite for bare
//!   parent-sibling references, dispatcher rewiring for
//!   SpecInvocation bindings through the generated module.
//!
//! The running history for any specific item lives in
//! `git log --oneline crates/edda-codegen/`.
//!
//! Still owed: spec-body item walk (once `edda-syntax` admits item
//! declarations inside `Spec.body`); body admission check;
//! repo-tier orphan removal; full `where`-clause discharge.

mod argument;
pub mod body;
mod canonical;
mod cascade;
mod emit;
mod encode;
mod error;
mod graph;
mod hash;
mod instantiate;
mod mangle;
mod manifest;
mod substitution;

pub use argument::{
    Argument, ArgumentTuple, EffectRowArgument, EffectRowEntry, PrimitiveValue, UserField,
    UserValue, UserVariantPayload,
};
pub use emit::emit_items;
pub use instantiate::instantiate_spec;
pub use canonical::{CanonicalForm, NestedDependency};
pub use cascade::{CodegenSession, CommitOutcome, StageRequest, StagedArtifact};
pub use encode::{encode, encode_into};
pub use error::CodegenError;
pub use graph::{CascadeGraph, CascadePlan, walk_cascade};
pub use hash::compute_hash;
pub use mangle::{artifact_name, mangle_short_name, module_disambig_hex};
pub use manifest::{to_argument_entry, to_artifact_header, to_artifact_inputs};
pub use substitution::{SubstitutionMap, substitute_spec_body};

// Re-export the cache types that appear on this crate's public
// surface so consumers (driver, daemon, MCP) can pattern-match on
// the same types without needing a transitive `edda-cache`
// dependency.
pub use edda_cache::{
    ArgumentEntry, ArgumentKind, ArtifactEntry, ArtifactHash, ArtifactHeader, ArtifactInputs,
    ArtifactName, BodyVersion, Manifest, NestedDep, ReachableFrom, Store, Tier,
};
