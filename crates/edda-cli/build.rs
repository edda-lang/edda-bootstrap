//! Build-time provenance injection for the `edda` (bootstrap-rust) binary.
//!
//! Stamps the compiler identity record (COMPILER-PROVENANCE.md §Mechanism 1)
//! into the binary as `cargo:rustc-env` values that [`crate::version`] reads
//! back through `env!()`. The native compiler receives the same fields through
//! a build-orchestrator-generated source unit; the two share one schema so
//! `edda version --json` is impl-agnostic.

use std::process::Command;

fn main() {
    emit("EDDA_IMPL", "bootstrap-rust".to_string());
    emit("EDDA_IMPL_VERSION", env_or("CARGO_PKG_VERSION", "0.0.0"));
    // Defaults to the public repo name. Dev trees pin `EDDA_DEV_SOURCE_REPO`
    // in the workspace `.cargo/config.toml` `[env]` so machine-local
    // provenance tooling can resolve the actual checkout by this name.
    emit("EDDA_SOURCE_REPO", env_or("EDDA_DEV_SOURCE_REPO", "edda-bootstrap"));
    emit("EDDA_SOURCE_SHA", git_sha());
    emit("EDDA_SOURCE_DIRTY", git_dirty());
    emit("EDDA_TARGET", target_triple());

    emit_rerun_triggers();
}

/// Declare the git-metadata inputs that force this script to re-run so the
/// sha/dirty stamp re-bakes whenever the source tree's identity changes.
///
/// On an ordinary commit the `HEAD` *file* is unchanged — it still holds
/// `ref: refs/heads/<branch>` — so watching `HEAD` alone never re-runs the
/// script. The ref that actually moves is the
/// loose ref `HEAD` points to; that path is the load-bearing trigger. `HEAD`
/// is still watched for the detached-HEAD case (where it holds the sha
/// directly) and for branch switches; `packed-refs` covers a packed branch
/// tip; `index` re-bakes the dirty bit on staged changes. Residual gap:
/// purely unstaged working-tree edits have no git-watchable file, so a `(dirty)`
/// flip from an unstaged edit alone re-bakes only on the next commit/stage —
/// the launcher's `tc sync` is the belt-and-braces guard for that window.
fn emit_rerun_triggers() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=EDDA_DEV_SOURCE_REPO");
    if let Some(head) = git_path("HEAD") {
        println!("cargo:rerun-if-changed={head}");
    }
    if let Some(branch_ref) = current_branch_ref() {
        if let Some(ref_path) = git_path(&branch_ref) {
            println!("cargo:rerun-if-changed={ref_path}");
        }
    }
    if let Some(refs) = git_path("packed-refs") {
        println!("cargo:rerun-if-changed={refs}");
    }
    if let Some(index) = git_path("index") {
        println!("cargo:rerun-if-changed={index}");
    }
}

/// The symbolic ref `HEAD` points to (e.g. `refs/heads/main`), or `None` when
/// `HEAD` is detached or git is unavailable.
fn current_branch_ref() -> Option<String> {
    git(&["symbolic-ref", "--quiet", "HEAD"])
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
}

/// Emit one `cargo:rustc-env` pair.
fn emit(key: &str, value: String) {
    println!("cargo:rustc-env={key}={value}");
}

/// Read a build-time env var or fall back to `default`.
fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Short git sha of the source tree, or `"unknown"` outside a checkout.
fn git_sha() -> String {
    git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_string())
}

/// `"true"` if the working tree has uncommitted changes, else `"false"`.
/// `"unknown"` when git is unavailable.
fn git_dirty() -> String {
    match git(&["status", "--porcelain"]) {
        Some(out) if out.trim().is_empty() => "false".to_string(),
        Some(_) => "true".to_string(),
        None => "unknown".to_string(),
    }
}

/// The Edda-grammar `<arch>-<os>-<abi>` triple this binary is built for,
/// composed from cargo's `CARGO_CFG_TARGET_*` build-script vars.
fn target_triple() -> String {
    let arch = env_or("CARGO_CFG_TARGET_ARCH", "unknown");
    let os = env_or("CARGO_CFG_TARGET_OS", "unknown");
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let abi = match env.as_str() {
        "" if os == "macos" => "darwin",
        "" => "none",
        other => other,
    };
    format!("{arch}-{os}-{abi}")
}

/// Resolve a git metadata path (handles worktrees, where `.git` is a file).
fn git_path(name: &str) -> Option<String> {
    git(&["rev-parse", "--git-path", name])
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
}

/// Run `git <args>` and return trimmed stdout on success.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
