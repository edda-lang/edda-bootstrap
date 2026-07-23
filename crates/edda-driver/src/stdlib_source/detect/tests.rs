//! Unit tests for the stdlib-source precedence resolver, worktree
//! heuristic, and operator-diagnostic emission.

use super::*;
use super::discover::looks_like_stdlib_worktree;
use crate::stdlib_source::StdlibSource;
use edda_diag::{Diagnostics, LintConfig, Severity};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Tiny `TempDir` helper — mirrors the pattern in `edda-cache` to
/// avoid a `tempfile` dev-dep just for these tests. The directory
/// is removed at drop.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(tag: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "edda-driver-stdlib-source-{}-{}-{}",
            tag,
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Lay down a workspace-only `package.toml` with `discover = true`
/// plus the two heuristic-trigger leaves: `lib/core/option/src/option.ea`
/// and `lib/io/stream/`. Returns the prepared top directory.
fn make_stdlib_worktree(td: &TempDir) -> PathBuf {
    let top = td.path().to_path_buf();
    // Workspace-only manifest with auto-discovery.
    fs::write(
        top.join("package.toml"),
        "[workspace]\ndiscover = true\n",
    )
    .unwrap();
    // Required leaf 1: lib/core/option/src/option.ea
    let core_option = top.join("lib").join("core").join("option").join("src");
    fs::create_dir_all(&core_option).unwrap();
    fs::write(core_option.join("option.ea"), "module std.core.option\n").unwrap();
    // Per-member manifest so the stdlib-walker treats it as a leaf.
    fs::write(
        top.join("lib").join("core").join("option").join("package.toml"),
        "[package]\nname = \"option\"\nversion = \"0.0.0\"\nroot_namespace = \"option\"\n",
    )
    .unwrap();
    // Required leaf 2: lib/io/stream/ as a directory.
    let io_stream = top.join("lib").join("io").join("stream").join("src");
    fs::create_dir_all(&io_stream).unwrap();
    top
}

/// Lay down a bog-standard consumer package (single `[package]`,
/// `src/` next to manifest). The heuristic must NOT match.
fn make_consumer_package(td: &TempDir) -> PathBuf {
    let top = td.path().to_path_buf();
    fs::write(
        top.join("package.toml"),
        "[package]\nname = \"app\"\nversion = \"0.0.0\"\nroot_namespace = \"app\"\n",
    )
    .unwrap();
    let src = top.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::write(src.join("main.ea"), "module app.main\n").unwrap();
    top
}

/// A vendored-override path that exists on disk — used so the
/// production code path (`is_dir()` check) returns `Some(Vendored)`.
fn make_vendored_override(td: &TempDir) -> PathBuf {
    let p = td.path().join("vendored");
    fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn resolve_returns_working_tree_for_stdlib_worktree() {
    let td = TempDir::new("ws-ok");
    let top = make_stdlib_worktree(&td);
    // Even with a vendored override available, the worktree wins.
    let vendored_td = TempDir::new("vendored-ok");
    let vendored = make_vendored_override(&vendored_td);

    let source = resolve_stdlib_source(&top, Some(vendored.clone()));
    match source {
        StdlibSource::WorkingTree(chosen) => assert_eq!(chosen, top),
        other => panic!("expected WorkingTree, got {:?}", other),
    }
}

#[test]
fn resolve_returns_vendored_for_package_consumer() {
    let td = TempDir::new("consumer-ok");
    let top = make_consumer_package(&td);
    let vendored_td = TempDir::new("vendored-cons");
    let vendored = make_vendored_override(&vendored_td);

    let source = resolve_stdlib_source(&top, Some(vendored.clone()));
    match source {
        StdlibSource::Vendored(p) => assert_eq!(p, vendored),
        other => panic!("expected Vendored, got {:?}", other),
    }
}

#[test]
fn resolve_returns_vendored_when_lib_core_option_absent() {
    let td = TempDir::new("ws-no-option");
    let top = td.path().to_path_buf();
    fs::write(
        top.join("package.toml"),
        "[workspace]\ndiscover = true\n",
    )
    .unwrap();
    // Workspace shape matches, BUT lib/core/option/src/option.ea is missing.
    // lib/io/stream/ exists.
    fs::create_dir_all(top.join("lib").join("io").join("stream").join("src")).unwrap();

    let vendored_td = TempDir::new("vendored-no-opt");
    let vendored = make_vendored_override(&vendored_td);
    let source = resolve_stdlib_source(&top, Some(vendored.clone()));
    match source {
        StdlibSource::Vendored(p) => assert_eq!(p, vendored),
        other => panic!("expected Vendored, got {:?}", other),
    }
}

#[test]
fn resolve_returns_vendored_when_lib_io_stream_absent() {
    let td = TempDir::new("ws-no-stream");
    let top = td.path().to_path_buf();
    fs::write(
        top.join("package.toml"),
        "[workspace]\ndiscover = true\n",
    )
    .unwrap();
    // Workspace shape matches, lib/core/option/src/option.ea exists, but lib/io/stream/ does NOT.
    let core_option = top.join("lib").join("core").join("option").join("src");
    fs::create_dir_all(&core_option).unwrap();
    fs::write(core_option.join("option.ea"), "module std.core.option\n").unwrap();

    let vendored_td = TempDir::new("vendored-no-stream");
    let vendored = make_vendored_override(&vendored_td);
    let source = resolve_stdlib_source(&top, Some(vendored.clone()));
    match source {
        StdlibSource::Vendored(p) => assert_eq!(p, vendored),
        other => panic!("expected Vendored, got {:?}", other),
    }
}

#[test]
fn resolve_returns_none_when_neither_exists() {
    let td = TempDir::new("nothing");
    let top = make_consumer_package(&td);
    // Vendored override pointing at a non-existent path → None.
    let bogus_vendored = td.path().join("does-not-exist");
    let source = resolve_stdlib_source(&top, Some(bogus_vendored));
    assert_eq!(source, StdlibSource::None);
}

#[test]
fn looks_like_stdlib_worktree_rejects_explicit_members_workspace() {
    let td = TempDir::new("ws-explicit");
    let top = td.path().to_path_buf();
    // [workspace] with an explicit members list (not discover = true) — heuristic must reject.
    fs::write(
        top.join("package.toml"),
        "[workspace]\nmembers = [\"core/option\"]\n",
    )
    .unwrap();
    let core_option = top.join("lib").join("core").join("option").join("src");
    fs::create_dir_all(&core_option).unwrap();
    fs::write(core_option.join("option.ea"), "module std.core.option\n").unwrap();
    fs::create_dir_all(top.join("lib").join("io").join("stream").join("src")).unwrap();

    assert!(!looks_like_stdlib_worktree(&top));
}

#[test]
fn resolve_returns_working_tree_when_active_root_is_a_member_under_stdlib_worktree() {
    // Active package root is a member subdirectory (lib/core/option/), but
    // the ancestor walk should detect the stdlib worktree top and select
    // it. This is the workspace-decomposition case: when the driver runs
    // once per member, each member's `package_root` is a sibling under
    // `lib/`, and the worktree-detection must climb to the workspace top.
    let td = TempDir::new("ws-member-active");
    let top = make_stdlib_worktree(&td);
    let member = top.join("lib").join("core").join("option");
    // Sanity: member is a per-package directory, not the stdlib worktree top.
    assert!(member.join("package.toml").is_file());

    let vendored_td = TempDir::new("vendored-member");
    let vendored = make_vendored_override(&vendored_td);
    let source = resolve_stdlib_source(&member, Some(vendored));
    match source {
        StdlibSource::WorkingTree(chosen) => assert_eq!(chosen, top),
        other => panic!("expected WorkingTree ancestor, got {:?}", other),
    }
}

#[test]
fn emit_stdlib_source_selection_pushes_expected_diagnostic_per_source() {
    let lint_cfg = LintConfig::new();

    // WorkingTree with a non-empty walk → one info diagnostic.
    let mut diags = Diagnostics::new();
    emit_stdlib_source_selection(
        &StdlibSource::WorkingTree(PathBuf::from("C:/some/worktree")),
        42,
        &mut diags,
        &lint_cfg,
    );
    assert_eq!(diags.iter().count(), 1);
    let only = diags.iter().next().unwrap();
    assert_eq!(only.severity, Severity::Info);
    assert!(only.message.contains("worktree"));
    assert!(only.message.contains("vendored"));

    // Vendored with a non-empty walk → silent (normal consumer build).
    let mut diags = Diagnostics::new();
    emit_stdlib_source_selection(
        &StdlibSource::Vendored(PathBuf::from("C:/vendored")),
        42,
        &mut diags,
        &lint_cfg,
    );
    assert_eq!(diags.iter().count(), 0);

    // None → one warn diagnostic naming EDDA_STDLIB_ROOT so the
    // operator sees the upstream cause before the per-import errors.
    let mut diags = Diagnostics::new();
    emit_stdlib_source_selection(&StdlibSource::None, 0, &mut diags, &lint_cfg);
    assert_eq!(diags.iter().count(), 1);
    let only = diags.iter().next().unwrap();
    assert_eq!(only.severity, Severity::Warn);
    assert!(only.message.contains("could not locate the stdlib source root"));
    assert!(only.message.contains("EDDA_STDLIB_ROOT"));
}

#[test]
fn emit_stdlib_source_selection_warns_when_vendored_walk_is_empty() {
    // Vendored selected but the walk registered zero modules — the
    // silent-failure case: live root but
    // unusable contents (uninitialised submodule, mis-archived
    // bootstrap, etc.). The Warn must fire so the operator gets the
    // upstream signal before the per-import cascade.
    let lint_cfg = LintConfig::new();
    let mut diags = Diagnostics::new();
    emit_stdlib_source_selection(
        &StdlibSource::Vendored(PathBuf::from("C:/empty-vendored")),
        0,
        &mut diags,
        &lint_cfg,
    );
    assert_eq!(diags.iter().count(), 1);
    let only = diags.iter().next().unwrap();
    assert_eq!(only.severity, Severity::Warn);
    assert!(only.message.contains("registered zero modules"));
    assert!(only.message.contains("vendored stdlib root"));
    assert!(only.message.contains("C:/empty-vendored") || only.message.contains("C:\\empty-vendored"));
}

#[test]
fn emit_stdlib_source_selection_warns_when_working_tree_walk_is_empty() {
    // Same shape as the Vendored-empty case but the live root was a
    // selected working-tree path that turned out to register zero
    // modules. The role label in the message changes to make the two
    // distinguishable in operator output.
    let lint_cfg = LintConfig::new();
    let mut diags = Diagnostics::new();
    emit_stdlib_source_selection(
        &StdlibSource::WorkingTree(PathBuf::from("C:/empty-worktree")),
        0,
        &mut diags,
        &lint_cfg,
    );
    assert_eq!(diags.iter().count(), 1);
    let only = diags.iter().next().unwrap();
    assert_eq!(only.severity, Severity::Warn);
    assert!(only.message.contains("registered zero modules"));
    assert!(only.message.contains("active worktree"));
}

#[test]
fn resolve_picks_edda_stdlibs_sibling_under_ancestor() {
    // Umbrella checkout shape:
    // `<umbrella>/edda-stdlibs/` is a stdlib worktree; the active
    // package lives elsewhere under `<umbrella>/.local/worktrees/<hex>/`.
    // The sibling-autodiscovery step must climb to `<umbrella>` and
    // pick up `<umbrella>/edda-stdlibs/` even though the active
    // ancestor chain never satisfies looks_like_stdlib_worktree
    // directly.
    let td = TempDir::new("umbrella-sibling");
    let umbrella = td.path().to_path_buf();
    let sibling_td = TempDir::new("umbrella-sibling-stdlibs");
    let sibling_top = make_stdlib_worktree(&sibling_td);
    // Re-home the stdlib worktree under the umbrella as `edda-stdlibs/`.
    // (Done by rename rather than constructing in-place so the helper
    // `make_stdlib_worktree` can be reused unchanged.)
    let stdlib_sibling = umbrella.join("edda-stdlibs");
    fs::rename(&sibling_top, &stdlib_sibling).unwrap();
    // Active package root sits a couple ancestors below the umbrella.
    let active = umbrella.join(".local").join("worktrees").join("dead-beef");
    fs::create_dir_all(&active).unwrap();
    // Consumer manifest on the active package so the ancestor walk
    // does not falsely match `looks_like_stdlib_worktree` on it.
    fs::write(
        active.join("package.toml"),
        "[package]\nname = \"app\"\nversion = \"0.0.0\"\nroot_namespace = \"app\"\n",
    )
    .unwrap();

    let source = resolve_stdlib_source(&active, None);
    match source {
        StdlibSource::WorkingTree(chosen) => assert_eq!(chosen, stdlib_sibling),
        other => panic!("expected WorkingTree sibling, got {:?}", other),
    }
}

#[test]
fn resolve_picks_stdlib_sibling_under_ancestor() {
    // Same shape as the edda-stdlibs case but the sibling is named
    // `stdlib/` — the layout this repo's own stdlib checkout uses.
    // The second entry in `STDLIB_SIBLING_NAMES` covers it.
    let td = TempDir::new("umbrella-sibling-stdlib");
    let umbrella = td.path().to_path_buf();
    let sibling_td = TempDir::new("umbrella-sibling-stdlib-src");
    let sibling_top = make_stdlib_worktree(&sibling_td);
    let stdlib_sibling = umbrella.join("stdlib");
    fs::rename(&sibling_top, &stdlib_sibling).unwrap();
    let active = umbrella.join("crate-x");
    fs::create_dir_all(&active).unwrap();
    fs::write(
        active.join("package.toml"),
        "[package]\nname = \"x\"\nversion = \"0.0.0\"\nroot_namespace = \"x\"\n",
    )
    .unwrap();

    let source = resolve_stdlib_source(&active, None);
    match source {
        StdlibSource::WorkingTree(chosen) => assert_eq!(chosen, stdlib_sibling),
        other => panic!("expected WorkingTree sibling, got {:?}", other),
    }
}

#[test]
fn sibling_autodiscovery_is_skipped_when_env_override_is_supplied() {
    // The env-override seam stays the canonical user-facing escape
    // hatch — when set to an existing path, it short-circuits the
    // sibling walk so power users keep deterministic control.
    let td = TempDir::new("umbrella-env-wins");
    let umbrella = td.path().to_path_buf();
    let sibling_td = TempDir::new("umbrella-env-wins-stdlibs");
    let sibling_top = make_stdlib_worktree(&sibling_td);
    let stdlib_sibling = umbrella.join("edda-stdlibs");
    fs::rename(&sibling_top, &stdlib_sibling).unwrap();
    let active = umbrella.join("crate-y");
    fs::create_dir_all(&active).unwrap();
    fs::write(
        active.join("package.toml"),
        "[package]\nname = \"y\"\nversion = \"0.0.0\"\nroot_namespace = \"y\"\n",
    )
    .unwrap();
    let vendored_td = TempDir::new("vendored-env-wins");
    let vendored = make_vendored_override(&vendored_td);

    let source = resolve_stdlib_source(&active, Some(vendored.clone()));
    match source {
        StdlibSource::Vendored(p) => assert_eq!(p, vendored),
        other => panic!("expected Vendored from env override, got {:?}", other),
    }
}
