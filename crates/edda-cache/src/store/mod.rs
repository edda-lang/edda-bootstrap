//! Cache-tier read/write gateway.
//!
//! [`Store`] owns the filesystem-rooted view of a project's
//! `.edda/cache/` directory and the in-memory `manifest.toon`. Reads
//! consult the manifest; writes go through a per-cascade staging
//! directory and commit atomically by `rename(2)` (`MoveFileExW` on
//! Windows). The staging contract is `migration.md` §3.
//!
//! `Store` is `Send + Sync`. The in-memory manifest is guarded by a
//! [`parking_lot::RwLock`]; reads are the common case (every artifact
//! write looks up `manifest.artifacts` for staleness) and writes
//! happen once per cascade-commit.

mod staging;

pub use staging::StagingHandle;

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;

use parking_lot::RwLock;
use time::OffsetDateTime;

use crate::error::CacheError;
use crate::hash::ArtifactHash;
use crate::manifest::Manifest;
use crate::paths;

/// Counter used to disambiguate concurrent cascade ids inside one
/// process. Combined with the process id and a high-resolution
/// timestamp it produces unique staging directory names without
/// pulling in a `uuid` crate.
pub(super) static CASCADE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Filesystem roots a `Store` operates on.
#[derive(Clone, Debug)]
pub struct CacheRoots {
    /// Project root (where `.edda/` lives).
    pub project_root: PathBuf,
}

impl CacheRoots {
    /// Construct from a project root.
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        CacheRoots {
            project_root: project_root.into(),
        }
    }
}

/// Read/write gateway over `.edda/cache/`. `Send + Sync`; the daemon
/// owns one per project and hands `&Store` into worker threads.
pub struct Store {
    pub(super) roots: CacheRoots,
    pub(super) manifest: RwLock<Manifest>,
}

impl Store {
    /// Open a `Store` rooted at `project_root`. Reads `manifest.toon`
    /// from disk if present; otherwise constructs an empty manifest
    /// (the build's first pass will populate it).
    ///
    /// `project_name` is copied into the manifest for new (empty)
    /// manifests; existing manifests preserve their own value. `now`
    /// is recorded as the empty manifest's `generated_at`.
    pub fn open(
        roots: CacheRoots,
        project_name: &str,
        now: OffsetDateTime,
    ) -> Result<Self, CacheError> {
        let manifest_path = paths::codegen_manifest_path(&roots.project_root);
        let manifest = if manifest_path.exists() {
            let text = fs::read_to_string(&manifest_path)
                .map_err(|e| CacheError::io("read", &manifest_path, e))?;
            Manifest::parse(&manifest_path, &text)?
        } else {
            Manifest::empty(project_name, now)
        };
        Ok(Store {
            roots,
            manifest: RwLock::new(manifest),
        })
    }

    /// Borrow the in-memory manifest for read.
    pub fn manifest(&self) -> parking_lot::RwLockReadGuard<'_, Manifest> {
        self.manifest.read()
    }

    /// Read the contents of an artifact file from disk.
    ///
    /// Looks up the artifact's recorded `path` in the manifest, then
    /// reads from that path. Returns `None` if the manifest does not
    /// record this hash.
    pub fn read(&self, hash: &ArtifactHash) -> Result<Option<Vec<u8>>, CacheError> {
        let guard = self.manifest.read();
        let entry = guard.artifacts.iter().find(|e| e.hash == *hash);
        let Some(entry) = entry else {
            return Ok(None);
        };
        let path = self.roots.project_root.join(entry.path.as_str());
        drop(guard);
        let bytes = fs::read(&path).map_err(|e| CacheError::io("read", &path, e))?;
        Ok(Some(bytes))
    }

    /// Begin a cascade. Creates the staging directory and returns a
    /// handle the caller writes artifacts into. The cascade ends with
    /// [`StagingHandle::commit`] or [`StagingHandle::abort`].
    pub fn begin_cascade(&self) -> Result<StagingHandle<'_>, CacheError> {
        let uuid = staging::next_cascade_uuid();
        let dir = paths::staging_dir(&self.roots.project_root, &uuid);
        fs::create_dir_all(&dir).map_err(|e| CacheError::io("create_dir_all", &dir, e))?;
        Ok(StagingHandle {
            store: self,
            uuid,
            staging_dir: dir,
            pending: Vec::new(),
            committed: false,
        })
    }
}

// SAFETY: The store has a `RwLock`-guarded `Manifest` and otherwise
// immutable fields, all of which are themselves `Send + Sync`.
// `parking_lot::RwLock<T>: Send + Sync` whenever `T: Send + Sync`, so
// the auto-derived bounds suffice; this comment documents intent.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::hash_bytes;
    use crate::manifest::{ArtifactEntry, ArtifactInputs, ReachableFrom};
    use crate::hash::BodyVersion;
    use crate::name::ArtifactName;
    use crate::tier::Tier;
    use smol_str::SmolStr;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;
    use std::thread;
    use time::macros::datetime;

    /// Build a tiny temporary directory under the OS temp area. We
    /// avoid the `tempfile` crate dependency by manually constructing
    /// a unique subdir and cleaning up at drop.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!(
                "edda-cache-test-{}-{}",
                std::process::id(),
                CASCADE_COUNTER.fetch_add(1, Ordering::Relaxed),
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

    fn open_empty_store(td: &TempDir) -> Store {
        let roots = CacheRoots::new(&td.path);
        Store::open(roots, "my_project", datetime!(2026-05-11 00:00:00 UTC)).unwrap()
    }

    fn sample_artifact_entry() -> ArtifactEntry {
        let hash = hash_bytes(b"sample-artifact-bytes");
        ArtifactEntry {
            path: SmolStr::new("codegen/sample.ea"),
            hash,
            short_name: SmolStr::new("Sample"),
            tier: Tier::Repo,
            inputs: ArtifactInputs {
                body_version: BodyVersion::CURRENT,
                spec_qualified_name: SmolStr::new("sample.Sample"),
                argument_tuple: Vec::new(),
                nested_deps: Vec::new(),
            },
            reachable_from: ReachableFrom::default(),
            generated_at: datetime!(2026-05-11 14:55:00 UTC),
        }
    }

    #[test]
    fn open_creates_empty_manifest_when_missing() {
        let td = TempDir::new();
        let store = open_empty_store(&td);
        assert_eq!(store.manifest().artifacts.len(), 0);
        assert_eq!(store.manifest().project, "my_project");
    }

    #[test]
    fn read_missing_hash_returns_none() {
        let td = TempDir::new();
        let store = open_empty_store(&td);
        let missing = hash_bytes(b"never written");
        let bytes = store.read(&missing).unwrap();
        assert!(bytes.is_none());
    }

    #[test]
    fn cascade_commit_writes_artifact_and_manifest() {
        let td = TempDir::new();
        let store = open_empty_store(&td);
        let entry = sample_artifact_entry();
        let body = b"\\ @generated\nfn body() {}\n";

        let final_path = td.path.join(entry.path.as_str());
        let name = ArtifactName::new("Sample", entry.hash.prefix()).unwrap();

        let mut handle = store.begin_cascade().unwrap();
        handle.write(Tier::Repo, &name, final_path.clone(), body).unwrap();

        let new_manifest = Manifest {
            schema_version: crate::manifest::SCHEMA_VERSION,
            project: SmolStr::new("my_project"),
            generated_at: datetime!(2026-05-11 14:55:00 UTC),
            last_gc_run: None,
            artifacts: vec![entry.clone()],
        };
        handle.commit(new_manifest).unwrap();

        // Artifact installed at final path.
        let read_back = fs::read(&final_path).unwrap();
        assert_eq!(read_back, body);

        // Manifest on disk records the new entry.
        let manifest_path = paths::codegen_manifest_path(&td.path);
        assert!(manifest_path.exists());
        let reopened = Store::open(
            CacheRoots::new(&td.path),
            "my_project",
            datetime!(2026-05-11 00:00:00 UTC),
        )
        .unwrap();
        assert_eq!(reopened.manifest().artifacts.len(), 1);
        assert_eq!(reopened.manifest().artifacts[0].hash, entry.hash);

        // Staging directory removed.
        let staging_root = td
            .path
            .join(".edda/cache/codegen/.staging");
        if staging_root.exists() {
            let entries: Vec<_> = fs::read_dir(&staging_root)
                .unwrap()
                .filter_map(Result::ok)
                .collect();
            assert!(entries.is_empty(), "staging dirs left behind: {:?}", entries);
        }
    }

    #[test]
    fn cascade_abort_leaves_state_unchanged() {
        let td = TempDir::new();
        let store = open_empty_store(&td);
        let entry = sample_artifact_entry();
        let name = ArtifactName::new("Sample", entry.hash.prefix()).unwrap();
        let final_path = td.path.join(entry.path.as_str());

        let mut handle = store.begin_cascade().unwrap();
        handle
            .write(Tier::Repo, &name, final_path.clone(), b"body")
            .unwrap();
        handle.abort();

        // Artifact NOT installed.
        assert!(!final_path.exists());
        // Manifest still empty.
        assert!(store.manifest().artifacts.is_empty());
    }

    #[test]
    fn cascade_drop_without_commit_acts_as_abort() {
        let td = TempDir::new();
        let store = open_empty_store(&td);
        let final_path = td.path.join("codegen/dropped.ea");
        let entry = sample_artifact_entry();
        let name = ArtifactName::new("Sample", entry.hash.prefix()).unwrap();
        {
            let mut handle = store.begin_cascade().unwrap();
            handle
                .write(Tier::Repo, &name, final_path.clone(), b"body")
                .unwrap();
            // drop without commit/abort
        }
        assert!(!final_path.exists());
    }

    #[test]
    fn store_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Store>();
    }

    #[test]
    fn concurrent_reads_do_not_block() {
        let td = TempDir::new();
        let store = Arc::new(open_empty_store(&td));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let store = Arc::clone(&store);
                thread::spawn(move || {
                    for _ in 0..100 {
                        let _ = store.manifest().artifacts.len();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }
}
