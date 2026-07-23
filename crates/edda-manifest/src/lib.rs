//! `package.toml` parsing, validation, and the v0.1 manifest schema.
//!
//! Implements: `docs/tooling/build-system.md` §3 (`package.toml` schema).
//!
//! The driver refuses to start the build if `package.toml` is missing or
//! unparseable (exit code 2; `build-system.md` §10). Successful parsing
//! produces a [`PackageManifest`] with the locked §3 schema and the
//! manifest's `lints` overrides materialised into an
//! [`edda_diag::LintConfig`].
//!
//! # Wave status
//!
//! - **Manifest schema (`§3`)**: complete for the v0.1 surface, including
//!   reserved-namespace validation on `root_namespace`,
//!   duplicate-dependency-name rejection, per-feature spans on
//!   `unknown_target_feature` diagnostics, and the [`load`] convenience
//!   that reads `package.toml` from disk via an [`edda_span::SourceMap`].
//! - **Lockfile (`§7`)**: implemented — [`lockfile`] module ships the
//!   `package.lock.toml` schema, parser, writer, and tamper-check.
//!   See [`lockfile::Lockfile`] and [`lockfile::load`].
//! - **Path resolution (`§4`)**: not yet implemented; lands when
//!   `edda-resolve` needs it.

mod parse;
pub mod compiler_pin;
pub mod lockfile;
mod schema;

pub use compiler_pin::{CompilerPin, CompilerPinError};
pub use parse::{load, load_any, parse, parse_any};
pub use schema::{
    BuildConfig, CodegenConfig, Compression, DebugInfo, DefaultTier, Dependency,
    DependencySource, FeatureSpec, GcSchedule, GcTag, GlobalCache, LoadedManifest,
    PackageKind, PackageManifest, Profile, ProfileSet, PublisherPin, SemVer,
    StructmapConfig, TokenBudgetEncoding, WorkspaceDiscover, WorkspaceManifest,
    WorkspaceTable,
};
pub use lockfile::{
    Lockfile, LockfileEntry, LockfileMeta, LockfileError,
};
