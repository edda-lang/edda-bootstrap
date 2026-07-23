//! BLAKE3 content-addressed store and TOON manifest I/O.
//!
//! The exclusive gateway for reads and writes under `.edda/cache/{codegen,
//! typecheck,objects}/`. Hash-sharded layout (`<2-byte-hex>/<artifact>.ea`)
//! and the per-tier manifest schema live here. The persistent daemon index
//! under `.edda/cache/index/` is `edda-daemon`'s territory, not this crate's.
//!
//! Implements:
//!   - `docs/codegen/storage.md` §7 (hash-sharded layout, tiering)
//!   - `docs/codegen/migration.md` §4 (manifest schema)
//!   - `docs/tooling/build-system.md` §5 (`.edda/cache/` layout)
//!   - `docs/tooling/build-system.md` §6 (global cache, cross-project sharing)
//!   - `docs/tooling/build-system.md` §7 (GC schedule and named tags)
//!
//! # Crate layout
//!
//! - [`hash`] — `ArtifactHash`, `HashPrefix`, `BodyVersion`. The universal
//!   content-addressed key.
//! - [`name`] — `ArtifactName` (`<short>__<12-hex-prefix>`) parsing.
//! - [`tier`] — repo / cache tier classification and the manifest's locked
//!   `default_tier` policy values.
//! - [`paths`] — pure path computation for repo-tier, cache-tier, global-cache,
//!   and staging directories.
//! - [`toon`] — minimal TOON reader/writer scoped to this crate's schemas.
//! - [`header`] — the `// @generated` comment block at the top of every
//!   generated artifact.
//! - [`manifest`] — the cache-tier `manifest.toon` schema (version 1).
//! - [`store`] — the read/write gateway. Atomic stage-rename. `Send + Sync`.
//! - [`gc`] — schedule enum, marker file, reachability-driven removal.
//! - [`global`] — `~/.edda/global-cache/` path computation and project
//!   registry.
//! - [`error`] — `CacheError` and its `edda-diag` mapping.
//!
//! # Scope cuts (deferred past this wave)
//!
//! - Compression (zstd) — `storage.md` §10.
//! - `.epack` packing — `distribution.md` §4 (v1.0 default).
//! - Per-pass blob layout (AST/typecheck/proofs/IR/obj) — `distribution.md`
//!   §2; this wave stores opaque `.ea` files.
//! - Global-cache GC walk-all-projects — `edda gc --global` belongs to
//!   `edda-cli`; this crate only owns per-machine path layout and
//!   `projects.toon`.
//! - Cross-project cascade — `build-system.md` §6, `edda-driver`'s
//!   territory.
//! - Daemon persistent index under `.edda/cache/index/` — `edda-daemon`'s
//!   territory.

mod error;
mod gc;
mod global;
mod hash;
mod header;
mod manifest;
mod name;
mod paths;
mod store;
mod tier;
mod toon;

pub use error::CacheError;
pub use gc::{Gc, GcSchedule, GcSummary, GcTier};
pub use global::GlobalCache;
pub use hash::{ArtifactHash, BodyVersion, HashPrefix, hash_bytes};
pub use header::{ArtifactHeader, NestedDep};
pub use manifest::{
    ArgumentEntry, ArgumentKind, ArtifactEntry, ArtifactInputs, Manifest, ReachableFrom,
    SCHEMA_VERSION,
};
pub use name::ArtifactName;
pub use paths::{
    cache_tier_path, global_cache_path, objects_cache_root, repo_tier_path, staging_dir,
    typecheck_cache_root,
};
pub use store::{CacheRoots, StagingHandle, Store};
pub use tier::{Tier, TierPolicy};
