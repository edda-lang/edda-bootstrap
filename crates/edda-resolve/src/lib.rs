//! Name resolution, scopes, and the source-import graph.
//!
//! Resolves identifiers to stable `BindingId`s, walks `import` statements to
//! build the source graph (the parse-and-resolve pass per
//! `docs/tooling/build-system.md` §2), and rejects module cycles with the
//! `import_cycle` diagnostic class.
//!
//! Implements:
//!   - `docs/syntax/declarations.md` §286 (module identity)
//!   - `docs/syntax/declarations.md` §301 (bare-leaf import resolution)
//!   - `docs/syntax/declarations.md` §312 (path resolution semantics)
//!   - `docs/tooling/build-system.md` §4 (path resolution, `module` override)
//!
//! # Resolution stages
//!
//! - **Path-resolution rules**: pure, I/O-free §4 rules
//!   over [`PackageLayout`] / [`DepIndex`] / [`StdlibIndex`] inputs;
//!   [`resolve_import_path`] and [`module_identity_for_file`] are the
//!   public surface.
//! - **Source-import graph**: [`build_source_graph`]
//!   drives parse-and-resolve from a set of entry files via a
//!   [`SourceLoader`] (concrete impl: [`FsLoader`]), produces a
//!   [`ResolvedSourceGraph`] with adjacency + topological order, and
//!   rejects cycles with `import_cycle` plus same-path collisions
//!   with `import_resolution_error`.
//! - **Top-level resolution**: [`build_resolved_package`]
//!   layers the per-module [`ItemTable`] (Function / TypeDecl / Spec
//!   bindings) and [`ImportLeafTable`] (leaf → ModuleId) on top of
//!   the source graph. Duplicate top-level decls and duplicate
//!   import leaves emit `import_resolution_error`.
//! - **Intra-function resolution**: per-module
//!   lexical scope walker declares Param + Local [`BindingEntry`]s
//!   for function/generic parameters, `let`/`var` bindings, and
//!   pattern bindings; resolves every Path AST node in expression,
//!   type, and pattern position to a [`Resolved`] (Binding / Module
//!   / Error) keyed by Path span. Cross-module Path access enforces
//!   `public` visibility. Unused imports emit `unused_import`.
//!
//! # Layering
//!
//! The crate is layered so module-graph errors short-circuit before
//! per-module resolution runs: a missing imported file produces
//! `import_resolution_error` once, not N times per use site.
//!
//! 1. **Module graph**: file-level. Path-resolution rules and
//!    the source-import graph driven by [`SourceLoader`].
//! 2. **Per-module resolution**: intra-file. Item tables,
//!    visibility, lexical scopes, and the resolution map keyed by
//!    span (AST nodes carry no `NodeId` — by design, per
//!    `crates/edda-syntax/src/ast/mod.rs`).

mod binding;
mod derive_specs;
mod graph;
mod imports;
mod items;
mod spec_mangling;
mod layout;
mod loader;
mod module;
mod package;
mod path;
mod resolutions;
mod resolve;
mod resolver;

pub use binding::{BindingEntry, BindingId, BindingKind};
pub use derive_specs::{DeriveSpecTarget, derive_spec_target};
pub use graph::{ModuleEntry, ModuleId, ResolvedSourceGraph, build_source_graph};
pub use imports::{ImportLeafBinding, ImportLeafTable};
pub use items::ItemTable;
pub use spec_mangling::{
    mangle_spec_invocation_name, module_disambig_hex_for_args, module_disambig_hex_from_ast,
    spec_invocation_module_leaf,
};
pub use layout::{DepIndex, ImporterContext, PackageLayout, StdlibIndex, owning_layout_for_file};
pub use loader::{FsLoader, SourceLoader};
pub use module::{
    ResolvedModule, ResolvedPackage, build_resolved_package, emit_binding_should_be_let_lints,
    emit_capability_safe_stdlib_lints, emit_dead_private_function_lints,
    emit_duplicate_spec_invocation_lints, emit_exec_scope_without_spawn_lints,
    emit_mode_overgrab_lints, emit_trust_budget_lints, emit_trust_hatch_too_dense_lints,
    emit_trust_points_listing, emit_unused_closure_capture_lints, emit_unused_import_lints,
};
pub use package::package_layout_from_manifest;
pub use path::{ModulePath, ModulePathDisplay};
pub use resolutions::{Resolved, Resolutions};
pub use resolve::{
    ImportKind, ResolveCx, ResolvedImport, module_identity_for_file, resolve_import_path,
};
