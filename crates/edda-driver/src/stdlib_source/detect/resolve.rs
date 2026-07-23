//! The stdlib-source precedence resolver — selects one root per cascade
//! invocation from the discovery primitives.

use std::path::{Path, PathBuf};

use super::super::StdlibSource;
use super::discover::{
    exe_relative_stdlib_root, find_stdlib_sibling_under_ancestor, find_stdlib_worktree_ancestor,
    vendored_stdlib_root,
};

/// Select the single stdlib root for one cascade invocation.
///
/// Precedence:
///   1. If `active_package_root` itself OR any ancestor up to
///      [`MAX_WORKTREE_ANCESTOR_DEPTH`] looks like a stdlib worktree
///      (per [`looks_like_stdlib_worktree`]), return
///      [`StdlibSource::WorkingTree`] holding that path. This is the
///      dual-load fix — the working-tree copy wins and the vendored
///      copy is skipped, so both [`StdlibIndex`] construction and the
///      source-graph walk feed from the same on-disk tree.
///
///      The ancestor walk is necessary because `Driver` runs once per
///      workspace member: when the user invokes the bootstrap inside
///      the `edda-stdlibs` worktree, the per-member `active_package_root`
///      is `<top>/lib/core/option/` (a single-package manifest), not the
///      workspace top. The heuristic must climb to the workspace-only
///      `package.toml` at the worktree root to detect the stdlib shape.
///   2. Otherwise, if `vendored_override` was supplied and the path
///      exists on disk, return [`StdlibSource::Vendored`] holding it.
///      Production callers pass [`env_stdlib_override`]'s output (the
///      `EDDA_STDLIB_ROOT` env value if set, else `None`) so operators
///      can recover when the compile-time-baked vendored path is dead;
///      tests use this seam to inject a synthetic vendored root.
///   3. Otherwise, if any ancestor up to
///      [`MAX_WORKTREE_ANCESTOR_DEPTH`] has a sibling directory named
///      in [`STDLIB_SIBLING_NAMES`] (`edda-stdlibs`, `stdlib`) that
///      satisfies [`looks_like_stdlib_worktree`], return
///      [`StdlibSource::WorkingTree`] holding that sibling.
///      Covers the common monorepo /
///      umbrella-checkout shape — a nested worktree of the native
///      compiler's umbrella checkout contains `edda-stdlibs/` as a
///      sibling submodule — so the bootstrap finds a usable stdlib
///      without an explicit env var.
///   4. Otherwise, if [`exe_relative_stdlib_root`] (`<exe_dir>/../std`)
///      exists on disk, return [`StdlibSource::Vendored`] holding it.
///      This is the plug-and-play release-install case: a prebuilt
///      archive lays out `bin/edda(.exe)` next to `std/` and `runes/`
///      at a shared install root, so a fresh install resolves its
///      stdlib with no `EDDA_STDLIB_ROOT` set by the user.
///   5. Otherwise, if a compile-time-baked vendored path was configured
///      ([`vendored_stdlib_root`] — dev builds only) and exists on
///      disk, return [`StdlibSource::Vendored`] holding it.
///   6. Otherwise, return [`StdlibSource::None`]. Operator-visible note
///      is emitted by [`emit_stdlib_source_selection`].
pub(crate) fn resolve_stdlib_source(
    active_package_root: &Path,
    vendored_override: Option<PathBuf>,
) -> StdlibSource {
    if let Some(worktree) = find_stdlib_worktree_ancestor(active_package_root) {
        return StdlibSource::WorkingTree(worktree);
    }
    if let Some(p) = vendored_override {
        if p.is_dir() {
            return StdlibSource::Vendored(p);
        }
        return StdlibSource::None;
    }
    if let Some(sibling) = find_stdlib_sibling_under_ancestor(active_package_root) {
        return StdlibSource::WorkingTree(sibling);
    }
    if let Some(exe_relative) = exe_relative_stdlib_root() {
        if exe_relative.is_dir() {
            return StdlibSource::Vendored(exe_relative);
        }
    }
    if let Some(baked) = vendored_stdlib_root() {
        if baked.is_dir() {
            return StdlibSource::Vendored(baked);
        }
    }
    StdlibSource::None
}
