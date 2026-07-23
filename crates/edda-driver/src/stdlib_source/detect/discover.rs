//! Stdlib-root discovery primitives: env override, the compile-time-baked
//! vendored path, the stdlib-worktree heuristic, and the bounded
//! ancestor / sibling walks the precedence resolver composes.

use std::path::{Path, PathBuf};

use edda_diag::{Diagnostics, LintConfig};
use edda_manifest::{LoadedManifest, WorkspaceDiscover};
use edda_span::SourceMap;

/// Read `EDDA_STDLIB_ROOT` from the process environment as the operator's
/// override for stdlib root discovery. The returned `PathBuf` is NOT
/// existence-checked here; downstream [`resolve_stdlib_source`] performs
/// the `is_dir()` check against it before honouring the override.
pub(crate) fn env_stdlib_override() -> Option<PathBuf> {
    std::env::var_os("EDDA_STDLIB_ROOT").map(PathBuf::from)
}

/// Compute the compile-time-baked path to the dev-layout stdlib tree.
///
/// The baked path is taken from the `EDDA_DEV_STDLIB_ROOT` environment
/// variable at *compile* time (dev trees pin it in `.cargo/config.toml`
/// `[env]`). Builds without it get no baked fallback and rely on the
/// other discovery seams (`EDDA_STDLIB_ROOT`, worktree / sibling
/// discovery). The returned path is normalised via
/// [`std::path::absolute`] (not `canonicalize`) so Windows UNC /
/// extended-length prefixes do not leak in. Note: a baked path can
/// outlive the tree it points into — when it goes dead, consumers must
/// set `EDDA_STDLIB_ROOT` (see [`env_stdlib_override`]) to recover.
/// The `edda` launcher always supplies
/// `EDDA_STDLIB_ROOT=<worktree>/std` in normal operation, so this baked
/// fallback only ever fires for a bare binary in the dev layout.
pub(crate) fn vendored_stdlib_root() -> Option<PathBuf> {
    let baked = option_env!("EDDA_DEV_STDLIB_ROOT")?;
    if baked.is_empty() {
        return None;
    }
    let from_env = PathBuf::from(baked);
    Some(std::path::absolute(&from_env).unwrap_or(from_env))
}

/// Compute the release-layout vendored stdlib path: the sibling `std/`
/// directory next to the running executable's containing directory
/// (`<exe_dir>/../std`). The install layout this targets is
/// `<install_root>/bin/edda(.exe)` beside `<install_root>/std/` and
/// `<install_root>/runes/` — the shape the release packer produces.
/// Returns `None` when [`std::env::current_exe`] fails or its path has
/// no parent directory; downstream [`resolve_stdlib_source`] performs
/// the `is_dir()` check before honouring it, same as
/// [`vendored_stdlib_root`].
pub(crate) fn exe_relative_stdlib_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;
    let candidate = exe_dir.join("..").join("std");
    Some(std::path::absolute(&candidate).unwrap_or(candidate))
}

/// Decide whether `top` looks like a stdlib worktree.
///
/// Returns `true` iff ALL of the following hold:
///   1. `top/package.toml` parses as [`LoadedManifest::WorkspaceOnly`].
///   2. Its `[workspace]` table sets `discover = true` (i.e. matches
///      [`WorkspaceDiscover::LibRoot`]).
///   3. `top/lib/core/option/src/option.ea` exists as a file.
///   4. `top/lib/io/stream/` exists as a directory.
///
/// Clauses 3 and 4 are universal stdlib leaves; requiring both prevents
/// a non-stdlib workspace with a coincidental `lib/core/` from being
/// mis-identified. The manifest peek uses a private [`SourceMap`] so
/// it never enters any driver-side diagnostic take.
pub(crate) fn looks_like_stdlib_worktree(top: &Path) -> bool {
    let manifest_path = top.join("package.toml");
    if !manifest_path.is_file() {
        return false;
    }
    let source_map = SourceMap::new();
    let mut diags = Diagnostics::new();
    let lint_cfg = LintConfig::new();
    let Some(loaded) = edda_manifest::load_any(&manifest_path, &source_map, &mut diags, &lint_cfg)
    else {
        return false;
    };
    let LoadedManifest::WorkspaceOnly(ws) = loaded else {
        return false;
    };
    if !matches!(ws.workspace.discover, Some(WorkspaceDiscover::LibRoot)) {
        return false;
    }
    let option_ea = top.join("lib").join("core").join("option").join("src").join("option.ea");
    if !option_ea.is_file() {
        return false;
    }
    let io_stream_dir = top.join("lib").join("io").join("stream");
    if !io_stream_dir.is_dir() {
        return false;
    }
    true
}

/// Maximum depth the worktree-ancestor walk climbs before giving up.
/// 16 is more than enough for any realistic project layout (workspace
/// top → subsystem → leaf is 2-3 levels in practice).
pub(super) const MAX_WORKTREE_ANCESTOR_DEPTH: u32 = 16;

/// Walk up from `start` checking each ancestor with
/// [`looks_like_stdlib_worktree`]; return the first ancestor that
/// matches (or `start` itself). Returns `None` if no ancestor in the
/// walk satisfies the heuristic.
pub(super) fn find_stdlib_worktree_ancestor(start: &Path) -> Option<PathBuf> {
    let mut cur: Option<&Path> = Some(start);
    let mut depth: u32 = 0;
    while let Some(dir) = cur {
        if depth >= MAX_WORKTREE_ANCESTOR_DEPTH {
            return None;
        }
        if looks_like_stdlib_worktree(dir) {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
        depth += 1;
    }
    None
}

/// Candidate sibling directory names searched by
/// [`find_stdlib_sibling_under_ancestor`].
const STDLIB_SIBLING_NAMES: &[&str] = &["edda-stdlibs", "stdlib"];

/// Walk up from `start` checking each ancestor for a sibling stdlib
/// worktree. At every ancestor, for every candidate name in
/// [`STDLIB_SIBLING_NAMES`], test whether `<ancestor>/<name>/` matches
/// [`looks_like_stdlib_worktree`]; return the first sibling that
/// matches. Returns `None` if no ancestor / sibling combination in the
/// walk satisfies the heuristic.
///
/// Covers the user-facing repro of
/// invoking the bootstrap from a nested worktree under an umbrella
/// checkout where `edda-stdlibs/` lives as a sibling submodule. The
/// compile-time-baked vendored path is unhelpful in this layout (it
/// points at the original build tree, which is unrelated
/// to the consumer's worktree); the sibling-discovery seam gives the
/// resolver a path forward beyond the `EDDA_STDLIB_ROOT` env var.
pub(super) fn find_stdlib_sibling_under_ancestor(start: &Path) -> Option<PathBuf> {
    let mut cur: Option<&Path> = Some(start);
    let mut depth: u32 = 0;
    while let Some(dir) = cur {
        if depth >= MAX_WORKTREE_ANCESTOR_DEPTH {
            return None;
        }
        for name in STDLIB_SIBLING_NAMES {
            let candidate = dir.join(name);
            if looks_like_stdlib_worktree(&candidate) {
                return Some(candidate);
            }
        }
        cur = dir.parent();
        depth += 1;
    }
    None
}
