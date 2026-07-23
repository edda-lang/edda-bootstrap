//! Stdlib source-of-truth selection and [`StdlibIndex`] construction.
//!
//! Single-sources the compiler's view of `std.*` so the driver never
//! double-registers the canonical stdlib modules. The selection picks
//! exactly one root via [`resolve_stdlib_source`]:
//!
//! - the active worktree (when CWD looks like a stdlib worktree per
//!   [`looks_like_stdlib_worktree`]), winning over vendored;
//! - the compile-time-baked vendored path; or
//! - `None` when neither is available (degraded mode — empty index).
//!
//! Both [`crate::context::Driver`] (for [`StdlibIndex`] construction)
//! and [`crate::codegen`] (for tier classification) consume the same
//! [`StdlibSource`] so the two paths cannot drift. Closes upstream-bug
//! B-023.

use std::path::{Path, PathBuf};

mod detect;
mod index;

pub(crate) use detect::{emit_stdlib_source_selection, env_stdlib_override, resolve_stdlib_source};
pub(crate) use index::build_stdlib_index;

/// Which root populated the [`StdlibIndex`] for one cascade invocation.
///
/// The selection is made once per
/// [`crate::context::Driver::initialize_with_siblings`] by
/// [`resolve_stdlib_source`] and threaded both into stdlib-index
/// construction ([`build_stdlib_index`]) and codegen tier classification
/// ([`crate::codegen`]). Picking exactly one root prevents the
/// dual-registration bug where a CWD inside the stdlib worktree caused
/// both the vendored copy and the working-tree copy to feed canonical
/// `std.*` modules into [`StdlibIndex`] + the source graph, colliding
/// at [`edda_resolve::build_source_graph`]'s `report_collision` path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StdlibSource {
    /// CWD looks like a stdlib worktree per [`looks_like_stdlib_worktree`];
    /// the working-tree path is used as the single source of truth and
    /// the vendored copy is skipped.
    WorkingTree(PathBuf),
    /// Normal consumer build — the compile-time-baked vendored path
    /// (or a test-supplied override) is used.
    Vendored(PathBuf),
    /// Neither a stdlib worktree nor a usable vendored path was found.
    /// The resulting [`StdlibIndex`] is empty; `std.*` imports will
    /// fail to resolve through the stdlib bypass (and either succeed
    /// via the source graph or surface as `import_resolution_error`).
    None,
}

impl StdlibSource {
    /// Borrow the selected root path, if any.
    pub fn root(&self) -> Option<&Path> {
        match self {
            StdlibSource::WorkingTree(p) | StdlibSource::Vendored(p) => Some(p),
            StdlibSource::None => None,
        }
    }
}
