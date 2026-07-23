//! Workspace-root resolution.
//!
//! When a top-level `package.toml` declares `[workspace]`, this module
//! peeks at the manifest, walks the declared members under `lib/<name>/`,
//! parses each member's own `package.toml`, and returns the per-member
//! resolution the driver uses to dispatch a cascade per member.
//!
//! `[workspace]` is Rust-style optional: a manifest with no `[workspace]`
//! table is a single-package build and the driver follows its
//! pre-workspace path unchanged.

use std::fs;
use std::path::{Path, PathBuf};

use edda_diag::{Diagnostics, LintConfig};
use edda_manifest::{PackageKind, PackageManifest, WorkspaceDiscover, WorkspaceTable};
use edda_span::SourceMap;

use crate::error::DriverError;

/// Result of peeking at a top-level manifest. Empty `members` ⇒
/// single-package build (the driver follows the existing path).
#[derive(Debug)]
pub struct WorkspaceResolution {
    /// Per-member metadata. Order matches `[workspace].members`.
    pub members: Vec<MemberHandle>,
    /// The top manifest's resolved `[structmap] descendant_tree` flag
    /// (already B-011-resolved: `[workspace] discover` forces this
    /// `true` regardless of the manifest's explicit `[structmap]` block).
    /// Consumed by `run_workspace_in_process` to pick between
    /// `edda_structmap::emit_workspace_aggregator` (shallow router) and
    /// `emit_workspace_descendant_tree` (full inlined tree) at the
    /// workspace root.
    pub descendant_tree: bool,
}

/// One workspace member resolved to its on-disk manifest.
#[derive(Clone, Debug)]
pub struct MemberHandle {
    /// Directory containing the member's `package.toml`.
    pub manifest_dir: PathBuf,
    /// Member's `package.toml` path.
    pub manifest_path: PathBuf,
    /// Member's parsed manifest. Owned so callers can rebuild a
    /// [`edda_resolve::PackageLayout`] without re-reading from disk.
    pub manifest: PackageManifest,
}

/// Peek at `top_manifest_path` to decide between single-package and
/// workspace dispatch. Uses a private [`SourceMap`] so peeked diagnostics
/// never enter the driver's own take.
///
/// # Errors
///
/// Returns [`DriverError::ManifestLoad`] when either the top manifest or
/// any member's manifest is missing or unparseable. Members are resolved
/// against `<top_manifest_dir>/lib/<member-name>/package.toml`.
pub fn resolve(top_manifest_path: &Path) -> Result<WorkspaceResolution, DriverError> {
    let source_map = SourceMap::new();
    let mut diags = Diagnostics::new();
    let lint_cfg = LintConfig::new();

    let top =
        edda_manifest::load_any(top_manifest_path, &source_map, &mut diags, &lint_cfg)
            .ok_or_else(|| DriverError::ManifestLoad {
                path: top_manifest_path.to_path_buf(),
            })?;

    let descendant_tree = top.structmap().descendant_tree;

    let workspace: &WorkspaceTable = match top.workspace_table() {
        Some(ws) => ws,
        None => {
            return Ok(WorkspaceResolution {
                members: Vec::new(),
                descendant_tree: false,
            });
        }
    };

    let top_dir = top_manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let member_paths = member_dirs_for_workspace(&top_dir, workspace)?;

    let mut members = Vec::with_capacity(member_paths.len());
    for manifest_dir in member_paths {
        let manifest_path = manifest_dir.join("package.toml");
        let member_source_map = SourceMap::new();
        let mut member_diags = Diagnostics::new();
        let manifest = edda_manifest::load(
            &manifest_path,
            &member_source_map,
            &mut member_diags,
            &lint_cfg,
        )
        .ok_or_else(|| DriverError::ManifestLoad {
            path: manifest_path.clone(),
        })?;
        members.push(MemberHandle {
            manifest_dir,
            manifest_path,
            manifest,
        });
    }

    Ok(WorkspaceResolution {
        members,
        descendant_tree,
    })
}

/// Resolve `[workspace] default_run` for a bare `edda run` at the workspace
/// root `top_manifest_path`, mapping the designated member to its own
/// `package.toml`.
///
/// Returns `Ok(None)` when the manifest declares no `[workspace]` table or
/// no `default_run` key — the caller keeps its original manifest, so the
/// existing single-package run or workspace-run rejection is unchanged.
/// Returns `Ok(Some(path))` when `default_run` names a resolved member:
/// `path` is `lib/<default_run>/package.toml`, exactly the manifest
/// `edda run <default_run>` would select. Returns `Err(message)` when
/// `default_run` is set but matches no member; `message` names the members.
///
/// A manifest that fails to load, or whose member walk fails, yields
/// `Ok(None)` rather than an error so the caller's normal build path
/// re-attempts the load and surfaces the canonical diagnostic once, instead
/// of a duplicate emitted here.
pub fn resolve_default_run(top_manifest_path: &Path) -> Result<Option<PathBuf>, String> {
    let source_map = SourceMap::new();
    let mut diags = Diagnostics::new();
    let lint_cfg = LintConfig::new();

    let Some(top) = edda_manifest::load_any(top_manifest_path, &source_map, &mut diags, &lint_cfg)
    else {
        return Ok(None);
    };
    let Some(workspace) = top.workspace_table() else {
        return Ok(None);
    };
    let Some(default_run) = workspace.default_run.as_deref() else {
        return Ok(None);
    };

    let top_dir = top_manifest_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let lib_dir = top_dir.join("lib");
    let target_dir = join_member_path(&lib_dir, default_run);

    let member_dirs = match member_dirs_for_workspace(&top_dir, workspace) {
        Ok(dirs) => dirs,
        Err(_) => return Ok(None),
    };
    if member_dirs.iter().any(|dir| *dir == target_dir) {
        return Ok(Some(target_dir.join("package.toml")));
    }

    let names: Vec<String> = member_dirs
        .iter()
        .filter_map(|dir| dir.strip_prefix(&lib_dir).ok())
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        .collect();
    Err(format!(
        "`[workspace] default_run` names `{default_run}`, which is not a workspace member \
         (members: {})",
        names.join(", ")
    ))
}

/// Auto-detect the unique workspace member requesting `[package] kind =
/// "executable"`, for a bare `edda run` at a workspace root once
/// [`resolve_default_run`] comes up empty.
///
/// Returns `Ok(None)` when the manifest declares no `[workspace]` table,
/// or when zero members set `kind = "executable"` — the caller falls
/// through to the next precedence tier unchanged. Returns
/// `Ok(Some(path))` when exactly one member sets it: `path` is that
/// member's own `package.toml`, exactly as `edda run <member>` would
/// resolve. Returns `Err(message)` when more than one member sets it —
/// ambiguous, so `run` must still reject; `message` names the candidates.
///
/// A manifest that fails to load, or whose member walk fails, yields
/// `Ok(None)` rather than an error, matching [`resolve_default_run`]'s
/// own deferral so the caller's normal build path surfaces the canonical
/// diagnostic once instead of a duplicate emitted here.
pub fn resolve_unique_runnable(top_manifest_path: &Path) -> Result<Option<PathBuf>, String> {
    let resolution = match resolve(top_manifest_path) {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };
    if resolution.members.is_empty() {
        return Ok(None);
    }

    let mut candidates: Vec<&MemberHandle> = resolution
        .members
        .iter()
        .filter(|m| m.manifest.kind == Some(PackageKind::Executable))
        .collect();

    match candidates.len() {
        0 => Ok(None),
        1 => Ok(Some(candidates.remove(0).manifest_path.clone())),
        _ => {
            let names: Vec<String> = candidates
                .iter()
                .filter_map(|m| m.manifest_dir.file_name())
                .map(|s| s.to_string_lossy().into_owned())
                .collect();
            Err(format!(
                "`edda run` at the workspace root is ambiguous: {} members request \
                 `[package] kind = \"executable\"` (candidates: {}). Run one explicitly \
                 with `edda run <member>`.",
                candidates.len(),
                names.join(", ")
            ))
        }
    }
}

/// Walk upward from `manifest_path`'s parent looking for an enclosing
/// `[workspace]` manifest. When found AND the active manifest's
/// directory is one of the workspace's listed members, return every
/// *other* member so the caller can register them as implicit deps in
/// the active driver's [`edda_resolve::DepIndex`].
///
/// Returns an empty Vec when no enclosing workspace is reached within
/// [`MAX_DISCOVER_DEPTH`] parent steps, or when the active dir is not
/// listed in the workspace's members (e.g., a sibling-of-workspace
/// directory, or a deeply-nested non-member path). This mirrors the
/// implicit-workspace-sibling rule documented in CLAUDE.md ("Layout —
/// single package vs workspace"): members are registered as deps of
/// each other by `root_namespace`.
///
/// # Errors
///
/// Returns [`DriverError::ManifestLoad`] when a discovered member's
/// `package.toml` cannot be parsed; otherwise the upward walk silently
/// short-circuits.
pub fn resolve_enclosing(manifest_path: &Path) -> Result<Vec<MemberHandle>, DriverError> {
    // Canonicalize the full manifest path first so relative invocations
    // like `edda check` (which pass `manifest_path = "package.toml"`)
    // walk upward from the actual cwd rather than from an empty parent.
    let canonical_manifest = manifest_path
        .canonicalize()
        .unwrap_or_else(|_| manifest_path.to_path_buf());
    let active_canonical = match canonical_manifest.parent() {
        Some(p) => p.to_path_buf(),
        None => return Ok(Vec::new()),
    };

    let source_map = SourceMap::new();
    let mut diags = Diagnostics::new();
    let lint_cfg = LintConfig::new();

    let mut depth: u32 = 0;
    let mut search = active_canonical.clone();
    while depth < MAX_DISCOVER_DEPTH {
        depth += 1;
        let parent = match search.parent() {
            Some(p) => p.to_path_buf(),
            None => break,
        };
        let candidate = parent.join("package.toml");
        if candidate.is_file()
            && let Some(loaded) =
                edda_manifest::load_any(&candidate, &source_map, &mut diags, &lint_cfg)
            && let Some(workspace) = loaded.workspace_table()
        {
            let member_paths = member_dirs_for_workspace(&parent, workspace)?;
            let mut siblings = Vec::with_capacity(member_paths.len().saturating_sub(1));
            let mut found_active = false;
            for manifest_dir in member_paths {
                let canonical = manifest_dir
                    .canonicalize()
                    .unwrap_or_else(|_| manifest_dir.clone());
                if canonical == active_canonical {
                    found_active = true;
                    continue;
                }
                let sibling_manifest_path = manifest_dir.join("package.toml");
                let member_source_map = SourceMap::new();
                let mut member_diags = Diagnostics::new();
                let manifest = edda_manifest::load(
                    &sibling_manifest_path,
                    &member_source_map,
                    &mut member_diags,
                    &lint_cfg,
                )
                .ok_or_else(|| DriverError::ManifestLoad {
                    path: sibling_manifest_path.clone(),
                })?;
                siblings.push(MemberHandle {
                    manifest_dir,
                    manifest_path: sibling_manifest_path,
                    manifest,
                });
            }
            if !found_active {
                return Ok(Vec::new());
            }
            return Ok(siblings);
        }
        search = parent;
    }
    Ok(Vec::new())
}

/// Resolve a [`WorkspaceTable`] into the list of member-directory paths
/// under `top_dir`. Honours both membership-source forms admitted by
/// B-005/B-010/B-011: explicit `members = [...]` (POSIX-relative paths
/// under `lib/`) and `discover = true | "<path>"` (filesystem walk for
/// `package.toml`).
///
/// Shared between [`resolve`] (workspace-build dispatch) and
/// [`crate::context::walk_stdlib_packages`] (stdlib path-table
/// construction) — B-012 fix-shape requirement that the two stdlib-
/// discovery paths cannot drift apart.
///
/// # Errors
///
/// Returns [`DriverError::ManifestLoad`] keyed at the discovery root when
/// a `discover` walk fails (the root itself is missing or unreadable).
/// Explicit `members` lists never produce an error here — missing member
/// manifests surface later when the caller tries to load each one.
pub(crate) fn member_dirs_for_workspace(
    top_dir: &Path,
    workspace: &WorkspaceTable,
) -> Result<Vec<PathBuf>, DriverError> {
    let lib_dir = top_dir.join("lib");
    Ok(match &workspace.discover {
        Some(WorkspaceDiscover::LibRoot) => discover_member_dirs(&lib_dir)?,
        Some(WorkspaceDiscover::Path(rel)) => {
            let root = join_member_path(top_dir, rel.as_ref());
            discover_member_dirs(&root)?
        }
        None => workspace
            .members
            .iter()
            .map(|name| join_member_path(&lib_dir, name.as_ref()))
            .collect(),
    })
}

/// Join `relative` onto `base` component-by-component so the OS-native
/// path separator is applied uniformly. `Path::join` keeps an embedded
/// forward slash inside `relative` verbatim, producing mixed-separator
/// paths like `lib\sub/foo` on Windows; downstream string-prefix checks
/// (e.g. resolver's "source under package root" guard) then fail. Splitting
/// on `/` first guarantees a canonical native-separator path.
fn join_member_path(base: &Path, relative: &str) -> PathBuf {
    let mut out = base.to_path_buf();
    for component in relative.split('/') {
        if !component.is_empty() {
            out.push(component);
        }
    }
    out
}

/// Recursively walk `root` and collect every directory containing a
/// `package.toml`. Used by `[workspace] discover = …` (B-011) to fill
/// the members list from the filesystem instead of from a hand-maintained
/// list. Symlink loops are bounded by `MAX_DISCOVER_DEPTH`.
///
/// Returns `Err` only when `root` itself is missing or unreadable; an
/// empty `root` (no `package.toml` descendants) yields `Ok(empty)`.
fn discover_member_dirs(root: &Path) -> Result<Vec<PathBuf>, DriverError> {
    let mut out = Vec::new();
    walk_for_members(root, &mut out, 0).map_err(|_| DriverError::ManifestLoad {
        path: root.to_path_buf(),
    })?;
    out.sort();
    Ok(out)
}

const MAX_DISCOVER_DEPTH: u32 = 64;

fn walk_for_members(
    dir: &Path,
    out: &mut Vec<PathBuf>,
    depth: u32,
) -> std::io::Result<()> {
    if depth >= MAX_DISCOVER_DEPTH {
        return Ok(());
    }
    if dir.join("package.toml").is_file() {
        out.push(dir.to_path_buf());
        return Ok(());
    }
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            walk_for_members(&entry.path(), out, depth + 1)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Tiny `TempDir` helper (removed at drop) — mirrors the pattern in
    /// `stdlib_source::detect::tests` to avoid a `tempfile` dev-dep.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!(
                "edda-driver-default-run-{}-{}-{}",
                tag,
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed),
            ));
            fs::create_dir_all(&path).unwrap();
            TempDir { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    /// Write a top `package.toml` with the given `[workspace]` body.
    fn write_top(td: &TempDir, workspace_body: &str) -> PathBuf {
        let top = td.path.join("package.toml");
        fs::write(
            &top,
            format!(
                "[package]\nname = \"root\"\nversion = \"0.1.0\"\nroot_namespace = \"root\"\n\n\
                 [workspace]\n{workspace_body}"
            ),
        )
        .unwrap();
        top
    }

    #[test]
    fn default_run_names_member_resolves_to_member_manifest() {
        let td = TempDir::new("ok");
        let top = write_top(&td, "members = [\"foo\", \"bar\"]\ndefault_run = \"foo\"\n");
        let got = resolve_default_run(&top).unwrap();
        assert_eq!(got, Some(td.path.join("lib").join("foo").join("package.toml")));
    }

    #[test]
    fn default_run_nested_member_resolves() {
        let td = TempDir::new("nested");
        let top = write_top(
            &td,
            "members = [\"core/option\", \"io/stream\"]\ndefault_run = \"core/option\"\n",
        );
        let got = resolve_default_run(&top).unwrap();
        let expected = td
            .path
            .join("lib")
            .join("core")
            .join("option")
            .join("package.toml");
        assert_eq!(got, Some(expected));
    }

    #[test]
    fn default_run_naming_non_member_errors_and_lists_members() {
        let td = TempDir::new("bad");
        let top = write_top(&td, "members = [\"foo\", \"bar\"]\ndefault_run = \"nope\"\n");
        let err = resolve_default_run(&top).unwrap_err();
        assert!(err.contains("nope"), "message names the bad value: {err}");
        assert!(err.contains("foo") && err.contains("bar"), "lists members: {err}");
    }

    #[test]
    fn no_default_run_key_is_none() {
        let td = TempDir::new("nokey");
        let top = write_top(&td, "members = [\"foo\"]\n");
        assert_eq!(resolve_default_run(&top).unwrap(), None);
    }

    #[test]
    fn non_workspace_manifest_is_none() {
        let td = TempDir::new("single");
        let top = td.path.join("package.toml");
        fs::write(
            &top,
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nroot_namespace = \"app\"\n",
        )
        .unwrap();
        assert_eq!(resolve_default_run(&top).unwrap(), None);
    }

    /// Write a trivial member manifest at `<td>/lib/<name>/package.toml`
    /// so `resolve()` can load it without erroring.
    fn write_member(td: &TempDir, name: &str) {
        let dir = td.path.join("lib").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("package.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nroot_namespace = \"{name}\"\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn descendant_tree_false_by_default() {
        let td = TempDir::new("descendant-default");
        write_member(&td, "foo");
        let top = write_top(&td, "members = [\"foo\"]\n");
        let resolution = resolve(&top).unwrap();
        assert!(!resolution.descendant_tree);
    }

    #[test]
    fn descendant_tree_resolves_from_explicit_structmap_block() {
        let td = TempDir::new("descendant-explicit");
        write_member(&td, "foo");
        let top = td.path.join("package.toml");
        fs::write(
            &top,
            "[package]\nname = \"root\"\nversion = \"0.1.0\"\nroot_namespace = \"root\"\n\n\
             [workspace]\nmembers = [\"foo\"]\n\n[structmap]\ndescendant_tree = true\n",
        )
        .unwrap();
        let resolution = resolve(&top).unwrap();
        assert!(resolution.descendant_tree);
    }

    #[test]
    fn descendant_tree_implied_by_discover_true() {
        // B-011: `[workspace] discover` forces `descendant_tree = true`
        // regardless of any explicit `[structmap]` block.
        let td = TempDir::new("descendant-discover");
        write_member(&td, "foo");
        let top = write_top(&td, "discover = true\n");
        let resolution = resolve(&top).unwrap();
        assert!(resolution.descendant_tree);
    }

    /// Write a member manifest at `<td>/lib/<name>/package.toml` with
    /// `[package] kind = "executable"` set.
    fn write_member_executable(td: &TempDir, name: &str) {
        let dir = td.path.join("lib").join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("package.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n\
                 root_namespace = \"{name}\"\nkind = \"executable\"\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn unique_runnable_member_resolves_to_member_manifest() {
        let td = TempDir::new("unique-ok");
        write_member_executable(&td, "foo");
        write_member(&td, "bar");
        let top = write_top(&td, "members = [\"foo\", \"bar\"]\n");
        let got = resolve_unique_runnable(&top).unwrap();
        assert_eq!(got, Some(td.path.join("lib").join("foo").join("package.toml")));
    }

    #[test]
    fn no_executable_member_is_none() {
        let td = TempDir::new("unique-none");
        write_member(&td, "foo");
        write_member(&td, "bar");
        let top = write_top(&td, "members = [\"foo\", \"bar\"]\n");
        assert_eq!(resolve_unique_runnable(&top).unwrap(), None);
    }

    #[test]
    fn multiple_executable_members_errors_and_lists_candidates() {
        let td = TempDir::new("unique-ambiguous");
        write_member_executable(&td, "foo");
        write_member_executable(&td, "bar");
        write_member(&td, "baz");
        let top = write_top(&td, "members = [\"foo\", \"bar\", \"baz\"]\n");
        let err = resolve_unique_runnable(&top).unwrap_err();
        assert!(err.contains("foo") && err.contains("bar"), "lists candidates: {err}");
        assert!(!err.contains("baz"), "excludes the non-executable member: {err}");
    }

    #[test]
    fn unique_runnable_non_workspace_manifest_is_none() {
        let td = TempDir::new("unique-single");
        let top = td.path.join("package.toml");
        fs::write(
            &top,
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nroot_namespace = \"app\"\n",
        )
        .unwrap();
        assert_eq!(resolve_unique_runnable(&top).unwrap(), None);
    }
}
