//! Per-cascade staging handle and the atomic stage-rename primitives.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use crate::error::CacheError;
use crate::manifest::Manifest;
use crate::name::ArtifactName;
use crate::paths;
use crate::tier::Tier;

use super::{CASCADE_COUNTER, Store};

/// One-cascade write transaction. Drops cleanly: on drop without
/// `commit`, the staging directory is deleted and no final paths are
/// touched (matching `migration.md` §3 step's atomicity contract).
pub struct StagingHandle<'a> {
    pub(super) store: &'a Store,
    pub(super) uuid: String,
    pub(super) staging_dir: PathBuf,
    pub(super) pending: Vec<PendingArtifact>,
    pub(super) committed: bool,
}

#[derive(Debug)]
pub(super) struct PendingArtifact {
    /// Tier the artifact is destined for. Currently unused by `commit`
    /// (it derives the target path from `final_path`) but reserved for
    /// repo-tier orphan removal once that step lands (`migration.md`
    /// §3 step 3).
    #[allow(dead_code)]
    tier: Tier,
    /// Final path (under repo or cache tier) the staged file will move
    /// to at commit.
    final_path: PathBuf,
    /// Staging-local filename. Always within `staging_dir`.
    staging_name: String,
}

impl<'a> StagingHandle<'a> {
    /// Identifier of this cascade. Surfaced for logging; the staging
    /// directory is `<project>/.edda/cache/codegen/.staging/<uuid>/`.
    pub fn uuid(&self) -> &str {
        &self.uuid
    }

    /// Write artifact bytes into staging. `final_path` is the path the
    /// artifact will move to at commit (the caller computes this with
    /// [`crate::paths::repo_tier_path`] or
    /// [`crate::paths::cache_tier_path`]).
    pub fn write(
        &mut self,
        tier: Tier,
        artifact_name: &ArtifactName,
        final_path: PathBuf,
        bytes: &[u8],
    ) -> Result<(), CacheError> {
        let staging_name = format!("{}.{}", self.pending.len(), artifact_name);
        let staged = self.staging_dir.join(&staging_name);
        fs::write(&staged, bytes).map_err(|e| CacheError::io("write", &staged, e))?;
        self.pending.push(PendingArtifact {
            tier,
            final_path,
            staging_name,
        });
        Ok(())
    }

    /// Commit the cascade: atomically install every staged artifact at
    /// its final path, then atomically swap the manifest. Consumes the
    /// handle.
    ///
    /// `new_manifest` is the manifest the caller has built up by
    /// applying the cascade's adds/removes; it replaces the in-memory
    /// manifest on success.
    pub fn commit(mut self, new_manifest: Manifest) -> Result<(), CacheError> {
        // Step 1: move each artifact to its final path.
        for pending in &self.pending {
            if let Some(parent) = pending.final_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| CacheError::io("create_dir_all", parent, e))?;
            }
            let src = self.staging_dir.join(&pending.staging_name);
            atomic_rename(&src, &pending.final_path)?;
        }
        // Step 2: stage-rename the manifest.
        let final_manifest_path = paths::codegen_manifest_path(&self.store.roots.project_root);
        let staged_manifest_path = staged_manifest_path(&final_manifest_path);
        if let Some(parent) = final_manifest_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| CacheError::io("create_dir_all", parent, e))?;
        }
        let text = new_manifest.to_text();
        // serialised size exceeds a generous per-entry budget. The
        // threshold is intentionally loose — order-of-magnitude blowups
        // (216 KB → 22 GB is 5 orders of magnitude) are what this
        // catches, not legitimate-but-large manifests. The corrupt
        // manifest that motivated this check
        // would have surfaced here at write time rather than as an
        // opaque OOM hours later in a different session.
        const PER_ENTRY_BUDGET: usize = 32 * 1024;
        const HEADER_BUDGET: usize = 4 * 1024;
        let artifact_count = new_manifest.artifacts.len();
        let expected_max = PER_ENTRY_BUDGET
            .saturating_mul(artifact_count)
            .saturating_add(HEADER_BUDGET);
        if text.len() > expected_max {
            return Err(CacheError::ManifestSizeAnomaly {
                path: staged_manifest_path,
                artifact_count,
                actual_bytes: text.len(),
                expected_max,
            });
        }
        fs::write(&staged_manifest_path, &text)
            .map_err(|e| CacheError::io("write", &staged_manifest_path, e))?;
        atomic_rename(&staged_manifest_path, &final_manifest_path)?;
        // Step 3: replace in-memory manifest.
        *self.store.manifest.write() = new_manifest;
        // Step 4: remove staging dir.
        let _ = fs::remove_dir_all(&self.staging_dir);
        self.committed = true;
        Ok(())
    }

    /// Abort the cascade: delete the staging directory without
    /// installing any artifacts. Consumes the handle.
    pub fn abort(mut self) {
        let _ = fs::remove_dir_all(&self.staging_dir);
        self.committed = true; // suppress Drop cleanup attempt
    }

    /// Number of artifacts written to staging so far.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

impl Drop for StagingHandle<'_> {
    fn drop(&mut self) {
        if !self.committed {
            // Dropped without commit/abort — treat as abort.
            let _ = fs::remove_dir_all(&self.staging_dir);
        }
    }
}

/// Atomic rename across paths on the same volume. On Windows
/// `fs::rename` calls `MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`;
/// on POSIX it calls `rename(2)`. Both are atomic when source and
/// destination are on the same filesystem volume — which is true by
/// construction here, because the staging directory is created under
/// `.edda/cache/codegen/.staging/`, the same root the final paths live
/// under.
fn atomic_rename(src: &Path, dst: &Path) -> Result<(), CacheError> {
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            // Some Windows builds reject `rename` if the destination
            // exists; remove and retry.
            fs::remove_file(dst).map_err(|e2| CacheError::io("remove", dst, e2))?;
            fs::rename(src, dst).map_err(|e2| CacheError::io("rename", dst, e2))
        }
        Err(e) => Err(CacheError::io("rename", dst, e)),
    }
}

/// Build the staged-manifest path (sibling of the real manifest,
/// suffixed with `.new`).
fn staged_manifest_path(final_path: &Path) -> PathBuf {
    let mut name = final_path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_else(|| std::ffi::OsString::from("manifest.toon"));
    name.push(".new");
    final_path.with_file_name(name)
}

/// Build a unique cascade-uuid string. Combines the process id, a
/// monotonically-increasing counter, and a high-resolution timestamp.
pub(super) fn next_cascade_uuid() -> String {
    let pid = std::process::id();
    let counter = CASCADE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{}-{}", pid, counter, nanos)
}
