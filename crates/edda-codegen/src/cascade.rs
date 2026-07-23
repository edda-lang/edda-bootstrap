//! Cascade orchestrator — drives [`edda_cache::Store::begin_cascade`]
//! through the §3 atomic stage-rename commit lifecycle.
//!
//! The session is a thin codegen-side layer over [`StagingHandle`].
//! Per [`StageRequest`] it hashes the [`CanonicalForm`], mangles the
//! short name, composes the full [`ArtifactName`], computes the
//! tier-specific final path, builds the generated artifact's header
//! bytes, writes header+body to the cascade's staging directory, and
//! accumulates a [`ArtifactEntry`] for the new manifest. At
//! [`commit`](CodegenSession::commit) the entries are merged into the
//! store's prior manifest and handed to [`StagingHandle::commit`].
//!
//! # Atomicity contract
//!
//! Per `docs/codegen/migration.md` §3: either every staged artifact
//! is committed (renamed to its final path, manifest updated
//! atomically) or every staged artifact is discarded (staging
//! directory removed, no final path touched). The session does not
//! add atomicity guarantees on top of what [`StagingHandle`] already
//! provides — it composes them with codegen-specific shape.
//!
//! # Scope cuts (deferred)
//!
//! - **Cascade dependency-graph walker.** The §3 step-3 traversal of
//!   downstream consumers that follows a hash change is a separate
//!   piece; this module provides only the commit-side orchestrator, not
//!   the graph traversal that decides which artifacts to regenerate.
//!   The driver will compose them once monomorphization lands.
//! - **Repo-tier orphan removal.** §3 step 3 requires removing the
//!   old file at a regenerated artifact's prior repo-tier path when
//!   the new hash produces a different filename. The session keeps
//!   both entries in the merged manifest for now; GC removes the
//!   orphan on its own schedule.
//! - **Reachability scoping** (`build-system.md` §5). The session
//!   accepts pre-resolved [`ReachableFrom`] data; the driver computes
//!   it from the active command's root set.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use edda_cache::{
    ArtifactEntry, ArtifactHash, ArtifactName, BodyVersion, Manifest, NestedDep, ReachableFrom,
    StagingHandle, Store, Tier, cache_tier_path, repo_tier_path,
};
use smol_str::SmolStr;
use time::OffsetDateTime;

use crate::canonical::CanonicalForm;
use crate::error::CodegenError;
use crate::hash::compute_hash;
use crate::manifest::{to_artifact_header, to_artifact_inputs};
use crate::mangle::artifact_name;

/// One-cascade write transaction at the codegen layer.
///
/// Lifetime is tied to the underlying [`Store`]: the session holds a
/// [`StagingHandle`] (which borrows the store immutably) and an
/// independent `&'a Store` it consults at commit time to read the
/// prior manifest. Drop without `commit`/`abort` acts as abort (via
/// [`StagingHandle::drop`]).
pub struct CodegenSession<'a> {
    store: &'a Store,
    staging: StagingHandle<'a>,
    project_root: PathBuf,
    body_version: BodyVersion,
    generator: SmolStr,
    pending_entries: Vec<ArtifactEntry>,
    staged_artifacts: BTreeMap<ArtifactHash, StagedArtifact>,
}

/// One artifact's worth of input to [`CodegenSession::stage`].
///
/// The caller supplies the hash inputs ([`form`](Self::form)) and
/// the rendered body source ([`body_source`](Self::body_source)).
/// The session computes the hash, name, and final path internally.
#[derive(Debug)]
pub struct StageRequest<'a> {
    /// Hash inputs (`storage.md` §2).
    pub form: &'a CanonicalForm,
    /// Repo-tier vs cache-tier placement (`storage.md` §1, chain-
    /// origin rule).
    pub tier: Tier,
    /// Generated artifact body — the parseable Edda source the
    /// artifact will contain *below* the `\ @generated` header. The
    /// session prepends the header bytes itself.
    pub body_source: &'a [u8],
    /// Display form of the spec invocation for the artifact header
    /// (e.g. `"std.option.Option(i32)"`). Not load-bearing for the
    /// hash; the canonical body bytes are the hash's only spec-
    /// identifying input.
    pub spec_invocation: &'a str,
    /// Caller-resolved nested-dependency list in the cache layer's
    /// `NestedDep` shape. The session writes these into both the
    /// header (verbatim) and the manifest entry's `nested_deps` (as
    /// short-name strings derived from `NestedDep::artifact.short`).
    pub nested_for_header: &'a [NestedDep],
    /// Sources / consuming artifacts that transitively reach this
    /// artifact, for GC reachability tracking (`migration.md` §4,
    /// `storage.md` §7).
    pub reachable_from: ReachableFrom,
}

/// Result of [`CodegenSession::stage`] — the resolved identity of
/// the just-staged artifact.
#[derive(Clone, Debug)]
pub struct StagedArtifact {
    /// Full BLAKE3 hash of the canonical form.
    pub hash: ArtifactHash,
    /// Full `<short>__<hash-prefix>` name.
    pub name: ArtifactName,
    /// Final path the artifact will move to at commit. Absolute path
    /// under [`CodegenSession::project_root`].
    pub final_path: PathBuf,
}

/// Result of [`CodegenSession::commit`].
#[derive(Clone, Debug)]
pub struct CommitOutcome {
    /// Total artifacts written by this cascade. Includes both new
    /// and replaced entries (the cascade does not distinguish them
    /// at this layer).
    pub artifacts_committed: usize,
}

impl<'a> CodegenSession<'a> {
    /// Begin a cascade.
    ///
    /// Creates the staging directory via [`Store::begin_cascade`] and
    /// returns a session the caller drives through
    /// [`stage`](Self::stage) and [`commit`](Self::commit) (or
    /// [`abort`](Self::abort)).
    ///
    /// `project_root` MUST match the project root the [`Store`] was
    /// opened against; the session uses it to compute repo-tier and
    /// cache-tier paths through [`edda_cache::repo_tier_path`] and
    /// [`edda_cache::cache_tier_path`]. The session does not
    /// re-validate this — passing a different root is a contract
    /// violation that produces a corrupt cache.
    ///
    /// `generator` is the human-readable build-tool identification
    /// stamped into every staged artifact's header
    /// (e.g. `"edda-codegen 0.0.0"`).
    pub fn begin(
        store: &'a Store,
        project_root: impl Into<PathBuf>,
        body_version: BodyVersion,
        generator: impl Into<SmolStr>,
    ) -> Result<Self, CodegenError> {
        let staging = store.begin_cascade()?;
        Ok(CodegenSession {
            store,
            staging,
            project_root: project_root.into(),
            body_version,
            generator: generator.into(),
            pending_entries: Vec::new(),
            staged_artifacts: BTreeMap::new(),
        })
    }

    /// Stage one artifact for commit.
    ///
    /// Computes the artifact's hash, name, and final path from
    /// `req`, builds the `\ @generated` header, writes
    /// `header + body_source` to the cascade's staging directory,
    /// and accumulates an [`ArtifactEntry`] for the manifest.
    ///
    /// **Content-addressed dedup (B15).** When the same hash has
    /// already been staged in this session, returns the cached
    /// [`StagedArtifact`] from the first stage instead of erroring.
    /// Two roots demanding the same `(spec_qualified, args)` thereby
    /// route through one canonical materialisation — the user-side
    /// `spec Option(T)` and a sibling spec's transitively-demanded
    /// `Option(T)` both resolve through the same on-disk module so
    /// pass-2 lowering binds them to one [`edda_resolve::BindingId`].
    /// [`CodegenError::DuplicateStaged`] is retained as a variant for
    /// callers that previously matched on it but is no longer
    /// returned by this method.
    pub fn stage(
        &mut self,
        req: StageRequest<'_>,
        now: OffsetDateTime,
    ) -> Result<StagedArtifact, CodegenError> {
        let hash = compute_hash(req.form, self.body_version);
        if let Some(cached) = self.staged_artifacts.get(&hash) {
            return Ok(cached.clone());
        }
        let name =
            artifact_name(&req.form.spec_qualified, &req.form.argument_tuple, &hash).ok_or_else(
                || CodegenError::InvalidArtifactName {
                    spec_qualified: req.form.spec_qualified.clone(),
                },
            )?;
        let final_path = self.final_path_for(req.tier, &req.form.spec_qualified, &hash, &name);
        let manifest_relative = relative_to_project(&self.project_root, &final_path);

        let header = to_artifact_header(
            req.spec_invocation,
            self.body_version,
            &hash,
            &self.generator,
            req.nested_for_header,
        );
        let header_text = header.to_text();
        let mut bytes = Vec::with_capacity(header_text.len() + req.body_source.len());
        bytes.extend_from_slice(header_text.as_bytes());
        bytes.extend_from_slice(req.body_source);

        self.staging
            .write(req.tier, &name, final_path.clone(), &bytes)?;

        let nested_short_names: Vec<SmolStr> = req
            .nested_for_header
            .iter()
            .map(|n| SmolStr::new(n.artifact.short()))
            .collect();
        let inputs = to_artifact_inputs(req.form, self.body_version, &nested_short_names);
        let entry = ArtifactEntry {
            path: SmolStr::new(manifest_relative.to_string_lossy().as_ref()),
            hash,
            short_name: SmolStr::new(name.short()),
            tier: req.tier,
            inputs,
            reachable_from: req.reachable_from,
            generated_at: now,
        };
        self.pending_entries.push(entry);

        let staged = StagedArtifact {
            hash,
            name,
            final_path,
        };
        self.staged_artifacts.insert(hash, staged.clone());
        Ok(staged)
    }

    /// `true` when `hash` has already been staged in this session.
    ///
    /// Wired by `drive_codegen` to dedupe artifact-path bookkeeping:
    /// pass-2 wants every materialised artifact in its entry-files set
    /// exactly once, regardless of how many roots demanded it.
    pub fn is_staged(&self, hash: &ArtifactHash) -> bool {
        self.staged_artifacts.contains_key(hash)
    }

    /// Commit the cascade.
    ///
    /// Assembles a new manifest by merging the pending adds into the
    /// store's prior manifest (replacing entries with the same hash)
    /// and atomically swaps it via [`StagingHandle::commit`]. Per
    /// `migration.md` §3, the rename of staged files happens before
    /// the manifest swap; both steps are atomic on their own
    /// filesystem level.
    pub fn commit(self, now: OffsetDateTime) -> Result<CommitOutcome, CodegenError> {
        let count = self.pending_entries.len();
        let new_manifest = merge_manifest(self.store, &self.pending_entries, now);
        self.staging.commit(new_manifest)?;
        Ok(CommitOutcome {
            artifacts_committed: count,
        })
    }

    /// Abort the cascade. Discards every staged artifact and leaves
    /// the manifest untouched.
    pub fn abort(self) {
        self.staging.abort();
    }

    /// Number of artifacts pending commit.
    pub fn pending_count(&self) -> usize {
        self.pending_entries.len()
    }

    /// Cascade identifier (forwarded from the staging handle). Useful
    /// for log lines and error attribution; the staging directory is
    /// `<project>/.edda/cache/codegen/.staging/<uuid>/`.
    pub fn uuid(&self) -> &str {
        self.staging.uuid()
    }

    /// Resolve the absolute final path for an artifact under its tier.
    ///
    /// Repo-tier paths use the spec's **parent** qualified path
    /// (segments before the leaf, per `paths::repo_tier_path`'s doc
    /// convention). Cache-tier paths are hash-sharded and ignore the
    /// spec qualified path.
    fn final_path_for(
        &self,
        tier: Tier,
        spec_qualified: &str,
        hash: &ArtifactHash,
        name: &ArtifactName,
    ) -> PathBuf {
        match tier {
            Tier::Repo => {
                let parent = parent_segments(spec_qualified);
                let refs: Vec<&str> = parent.iter().copied().collect();
                repo_tier_path(&self.project_root, &refs, name)
            }
            Tier::Cache => cache_tier_path(&self.project_root, hash, name),
        }
    }
}

/// Merge the cascade's pending entries into the store's prior
/// manifest and stamp `generated_at = now`. Returns the new manifest
/// (the caller hands this to `StagingHandle::commit`).
fn merge_manifest(store: &Store, pending: &[ArtifactEntry], now: OffsetDateTime) -> Manifest {
    let (schema_version, project, last_gc_run, mut artifacts) = {
        let guard = store.manifest();
        (
            guard.schema_version,
            guard.project.clone(),
            guard.last_gc_run,
            guard.artifacts.clone(),
        )
    };
    for entry in pending {
        if let Some(existing) = artifacts.iter_mut().find(|e| e.hash == entry.hash) {
            *existing = entry.clone();
        } else {
            artifacts.push(entry.clone());
        }
    }
    Manifest {
        schema_version,
        project,
        generated_at: now,
        last_gc_run,
        artifacts,
    }
}

/// Split a dotted qualified name into its **parent** segments
/// (everything before the final `.`-separated leaf).
///
/// `"std.option.Option"` → `["std", "option"]`
/// `"Option"`            → `[]`
fn parent_segments(qualified: &str) -> Vec<&str> {
    let mut iter = qualified.split('.');
    let mut all: Vec<&str> = iter.by_ref().collect();
    all.pop();
    all
}

/// Project `path` onto `project_root` if possible, otherwise return
/// it unchanged. The manifest schema expects repo-relative paths; a
/// failure to strip indicates the caller passed a `project_root` that
/// does not match the `Store`'s — a contract violation, but one this
/// function does not enforce.
fn relative_to_project(project_root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(project_root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}
