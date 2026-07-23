//! Global cache (`~/.edda/global-cache/codegen/`).
//!
//! Per `build-system.md` §6, the global cache is per-user, opt-in,
//! read-write content-addressed storage. Two gates must both pass for
//! a project to consult it:
//!
//!   1. The project's `package.toon` sets `codegen: { global_cache:
//!      enabled }`.
//!   2. The user's environment sets `EDDA_GLOBAL_CACHE=1`.
//!
//! If both pass, the project's build copies global-cache entries into
//! its local cache on hit and writes through to the global cache on
//! miss. Coherence across machines is mechanical (content-addressing).
//!
//! Scope for this wave:
//!   - Path computation (delegated to [`crate::paths::global_cache_path`]).
//!   - The two-gate enablement check.
//!   - `~/.edda/global-cache/projects.toon` registry of projects that
//!     consult the global cache (so `edda gc --global` later knows
//!     which to walk).
//!
//! Out of scope (deferred):
//!   - The walk-all-projects global-GC pass — lives in `edda-cli`.
//!   - Compression / `.epack` packing of global entries.

use std::fs;
use std::path::{Path, PathBuf};

use smol_str::SmolStr;

use crate::error::CacheError;
use crate::hash::ArtifactHash;
use crate::name::ArtifactName;
use crate::paths;
use crate::toon::{self, Writer};

/// Env var the user must set for global-cache participation.
const ENABLE_ENV_VAR: &str = "EDDA_GLOBAL_CACHE";

/// Subpath of the user's home directory holding the projects registry.
const PROJECTS_REGISTRY_NAME: &str = "projects.toon";

/// Read-only descriptor of the user's global cache. Construct via
/// [`resolve`](Self::resolve), which returns `None` unless both gates
/// pass.
#[derive(Clone, Debug)]
pub struct GlobalCache {
    home: PathBuf,
}

impl GlobalCache {
    /// Resolve a global cache, applying both enablement gates.
    /// `manifest_opt_in` is the `codegen.global_cache: enabled` flag
    /// from the project's `package.toon`. `env_value` is the literal
    /// value of `EDDA_GLOBAL_CACHE` in the user's environment, or
    /// `None` if the variable is unset.
    pub fn resolve(
        home: impl AsRef<Path>,
        manifest_opt_in: bool,
        env_value: Option<&str>,
    ) -> Option<Self> {
        if !manifest_opt_in {
            return None;
        }
        if !env_value_is_truthy(env_value) {
            return None;
        }
        Some(GlobalCache {
            home: home.as_ref().to_path_buf(),
        })
    }

    /// Resolve a global cache by reading the env var from `std::env`.
    /// Convenience wrapper around [`resolve`](Self::resolve).
    pub fn resolve_from_env(home: impl AsRef<Path>, manifest_opt_in: bool) -> Option<Self> {
        let value = std::env::var(ENABLE_ENV_VAR).ok();
        Self::resolve(home, manifest_opt_in, value.as_deref())
    }

    /// Compute the path of an artifact in the global cache.
    pub fn artifact_path(&self, hash: &ArtifactHash, name: &ArtifactName) -> PathBuf {
        paths::global_cache_path(&self.home, hash, name)
    }

    /// Root of the global cache (`<home>/.edda/global-cache/`).
    pub fn root(&self) -> PathBuf {
        let mut path = self.home.clone();
        for seg in "/.edda/global-cache".trim_start_matches('/').split('/') {
            path.push(seg);
        }
        path
    }

    /// Path of the global projects registry (`projects.toon`).
    pub fn projects_registry_path(&self) -> PathBuf {
        self.root().join(PROJECTS_REGISTRY_NAME)
    }

    /// Read the registry of projects that consult this global cache.
    pub fn read_projects(&self) -> Result<Vec<SmolStr>, CacheError> {
        let path = self.projects_registry_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let text = fs::read_to_string(&path)
            .map_err(|e| CacheError::io("read", &path, e))?;
        let value = toon::parse(&text).map_err(|e| CacheError::ManifestParse {
            path: path.clone(),
            line: e.line,
            message: e.message,
        })?;
        let list = value
            .get("projects")
            .and_then(|v| v.as_list())
            .unwrap_or(&[]);
        let mut out = Vec::with_capacity(list.len());
        for entry in list {
            if let Some(s) = entry.as_str() {
                out.push(SmolStr::new(s));
            }
        }
        Ok(out)
    }

    /// Register `project_path` (an absolute path) into the global
    /// projects registry. Idempotent: a re-registration is a no-op.
    pub fn register_project(&self, project_path: &Path) -> Result<(), CacheError> {
        let mut current = self.read_projects()?;
        let project_str = SmolStr::new(path_to_str(project_path));
        if current.iter().any(|p| p == &project_str) {
            return Ok(());
        }
        current.push(project_str);
        self.write_projects(&current)
    }

    /// Serialise the projects list to `projects.toon` via stage-rename.
    fn write_projects(&self, projects: &[SmolStr]) -> Result<(), CacheError> {
        let final_path = self.projects_registry_path();
        if let Some(parent) = final_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| CacheError::io("create_dir_all", parent, e))?;
        }
        let staged_path = staged_registry_path(&final_path);
        let mut w = Writer::new();
        w.comment("projects.toon — global cache project registry");
        w.comment("Generated automatically; do not edit by hand.");
        w.blank_line();
        if projects.is_empty() {
            w.empty_list("projects");
        } else {
            w.block("projects", |w| {
                for p in projects {
                    w.list_item_scalar(p.as_str());
                }
            });
        }
        let text = w.finish();
        fs::write(&staged_path, &text)
            .map_err(|e| CacheError::io("write", &staged_path, e))?;
        fs::rename(&staged_path, &final_path)
            .map_err(|e| CacheError::io("rename", &final_path, e))?;
        Ok(())
    }
}

/// Apply the locked truthiness rule for `EDDA_GLOBAL_CACHE`. The
/// `build-system.md` §6 description says the var is "init" — we
/// interpret that as any of the canonical truthy spellings.
fn env_value_is_truthy(value: Option<&str>) -> bool {
    let Some(v) = value else { return false };
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Build the staged-registry path (sibling of the real file, suffixed
/// with `.new`).
fn staged_registry_path(final_path: &Path) -> PathBuf {
    let mut name = final_path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from(PROJECTS_REGISTRY_NAME));
    name.push(".new");
    final_path.with_file_name(name)
}

/// Convert a `Path` to a string for `projects.toon` storage. On
/// platforms where paths are not UTF-8 (Windows extended paths,
/// Unix non-UTF-8 paths) the result is lossy; this is acceptable
/// because the registry is informational.
fn path_to_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::hash_bytes;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!(
                "edda-cache-global-test-{}-{}",
                std::process::id(),
                TEST_COUNTER.fetch_add(1, Ordering::Relaxed),
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

    #[test]
    fn resolve_requires_both_gates() {
        let home = Path::new("/home/alice");
        assert!(GlobalCache::resolve(home, false, Some("1")).is_none());
        assert!(GlobalCache::resolve(home, true, None).is_none());
        assert!(GlobalCache::resolve(home, true, Some("0")).is_none());
        assert!(GlobalCache::resolve(home, true, Some("1")).is_some());
    }

    #[test]
    fn resolve_accepts_truthy_spellings() {
        let home = Path::new("/home/alice");
        for spelling in &["1", "true", "TRUE", "yes", "YES", "on", " 1 ", "On"] {
            assert!(
                GlobalCache::resolve(home, true, Some(spelling)).is_some(),
                "expected `{}` to be truthy",
                spelling,
            );
        }
        for spelling in &["0", "false", "no", "off", "", "maybe"] {
            assert!(
                GlobalCache::resolve(home, true, Some(spelling)).is_none(),
                "expected `{}` to be falsy",
                spelling,
            );
        }
    }

    #[test]
    fn artifact_path_uses_shared_layout() {
        let home = Path::new("/home/alice");
        let gc = GlobalCache::resolve(home, true, Some("1")).unwrap();
        let hash = hash_bytes(b"hello");
        let name = ArtifactName::new("Option_i32", hash.prefix()).unwrap();
        let global_path = gc.artifact_path(&hash, &name);
        let direct = paths::global_cache_path(home, &hash, &name);
        assert_eq!(global_path, direct);
    }

    #[test]
    fn projects_registry_round_trip() {
        let td = TempDir::new();
        let gc = GlobalCache::resolve(&td.path, true, Some("1")).unwrap();
        assert!(gc.read_projects().unwrap().is_empty());

        gc.register_project(Path::new("/path/to/project_a")).unwrap();
        gc.register_project(Path::new("/path/to/project_b")).unwrap();
        // Idempotent re-register.
        gc.register_project(Path::new("/path/to/project_a")).unwrap();

        let projects = gc.read_projects().unwrap();
        assert_eq!(projects.len(), 2);
        // Insertion order is preserved.
        assert!(projects[0].contains("project_a"));
        assert!(projects[1].contains("project_b"));
    }
}
