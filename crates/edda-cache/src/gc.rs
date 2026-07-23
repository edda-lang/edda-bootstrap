//! Garbage collection.
//!
//! Per `build-system.md` §7, GC is scheduled by named tag in
//! `package.toon` (`never`, `on_ci`, `daily`, `weekly`,
//! `on_promote_revoke`) and runs against the manifest's `reachable_from`
//! sets per `migration.md` §7.
//!
//! This module owns the schedule enum, the marker-file lifecycle, the
//! "should we run now?" trigger logic, and the manifest-walk-and-prune
//! step. The *computation* of `reachable_from` (the source graph + spec
//! invocation walk) lives in `edda-driver`; cache executes the
//! deletions.
//!
//! Scope cuts for this wave (deferred):
//!   - Global-cache GC walk-all-projects (`edda gc --global`) — belongs
//!     to `edda-cli`.
//!   - Compressed-artifact awareness (zstd) — `storage.md` §10.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use time::OffsetDateTime;

use crate::error::CacheError;
use crate::hash::ArtifactHash;
use crate::manifest::Manifest;
use crate::paths;

/// Filename of the per-tier GC marker, written into
/// `.edda/cache/codegen/` for the duration of a run.
const MARKER_FILENAME: &str = ".gc-in-progress";

/// `codegen.gc_schedule` tag.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Default)]
pub enum GcSchedule {
    /// GC never runs automatically; user invokes `edda gc` manually.
    #[default]
    Never,
    /// GC runs once per CI build (detected via `CI=1` env var).
    OnCi,
    /// GC runs on the first build of each calendar day (UTC).
    Daily,
    /// GC runs on the first build of each calendar week (Monday-anchored UTC).
    Weekly,
    /// GC runs after every `edda demote` action.
    OnPromoteRevoke,
}

impl GcSchedule {
    /// Lowercase tag used in `package.toon` and the manifest's
    /// `gc_schedule` field.
    pub const fn name(self) -> &'static str {
        match self {
            GcSchedule::Never => "never",
            GcSchedule::OnCi => "on_ci",
            GcSchedule::Daily => "daily",
            GcSchedule::Weekly => "weekly",
            GcSchedule::OnPromoteRevoke => "on_promote_revoke",
        }
    }

    /// Parse a schedule tag from `package.toon`.
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "never" => Some(GcSchedule::Never),
            "on_ci" => Some(GcSchedule::OnCi),
            "daily" => Some(GcSchedule::Daily),
            "weekly" => Some(GcSchedule::Weekly),
            "on_promote_revoke" => Some(GcSchedule::OnPromoteRevoke),
            _ => None,
        }
    }

    /// Decide whether GC should run on this build invocation.
    ///
    /// `now` is the current wall-clock time (UTC). `last_run` is
    /// `manifest.last_gc_run` (may be `None` if GC has never run).
    /// `ci_active` is true iff the `CI` environment variable is set to
    /// a truthy value.
    pub fn should_run(
        self,
        now: OffsetDateTime,
        last_run: Option<OffsetDateTime>,
        ci_active: bool,
    ) -> bool {
        match self {
            GcSchedule::Never | GcSchedule::OnPromoteRevoke => false,
            GcSchedule::OnCi => ci_active,
            GcSchedule::Daily => match last_run {
                Some(prev) => !same_utc_day(now, prev),
                None => true,
            },
            GcSchedule::Weekly => match last_run {
                Some(prev) => !same_utc_iso_week(now, prev),
                None => true,
            },
        }
    }
}

impl std::fmt::Display for GcSchedule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Which tier `Gc::run` operates on.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum GcTier {
    /// Walk and prune the repo tier (`<project>/codegen/`).
    Repo,
    /// Walk and prune the cache tier (`<project>/.edda/cache/codegen/`).
    Cache,
}

/// Summary of a GC pass.
#[derive(Clone, Debug, Default)]
pub struct GcSummary {
    /// Number of artifacts removed.
    pub artifacts_removed: usize,
    /// Total bytes recovered.
    pub bytes_recovered: u64,
    /// Whether the run was a dry run (no filesystem changes).
    pub dry_run: bool,
}

/// GC operator. Holds the project root path; methods take the manifest
/// and the reachable set computed by the driver.
pub struct Gc {
    project_root: PathBuf,
}

impl Gc {
    /// Construct a GC operator rooted at `project_root` (the same root
    /// passed to `CacheRoots`).
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Gc {
            project_root: project_root.into(),
        }
    }

    /// Whether a previous GC pass was interrupted and left its marker
    /// file behind. The next build resumes GC by calling
    /// [`run`](Self::run) again; the manifest's content-addressed model
    /// makes resume idempotent.
    pub fn is_resuming(&self) -> bool {
        marker_path(&self.project_root).exists()
    }

    /// Walk the cache and remove every artifact whose hash is not in
    /// `live_hashes`. The driver computes `live_hashes` by walking the
    /// source graph from the current command's root set (per
    /// `build-system.md` §5's reachability rule).
    ///
    /// Note: the manifest *itself* is not mutated by this method; the
    /// caller is expected to follow up with a manifest update that
    /// drops the same artifacts (via the normal `StagingHandle` flow).
    /// This separation keeps GC composable with the cascade-commit
    /// atomicity contract (`migration.md` §3).
    pub fn run(
        &self,
        manifest: &Manifest,
        tier: GcTier,
        live_hashes: &HashSet<ArtifactHash>,
        dry_run: bool,
    ) -> Result<GcSummary, CacheError> {
        let codegen_root = paths::codegen_cache_root(&self.project_root);
        let marker = marker_path(&self.project_root);
        if !dry_run {
            if let Some(parent) = marker.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| CacheError::io("create_dir_all", parent, e))?;
            }
            fs::write(&marker, b"")
                .map_err(|e| CacheError::io("write", &marker, e))?;
        }

        let mut summary = GcSummary {
            dry_run,
            ..GcSummary::default()
        };

        for entry in &manifest.artifacts {
            if !entry_in_tier(entry.tier, tier) {
                continue;
            }
            if live_hashes.contains(&entry.hash) {
                continue;
            }
            let path = self.project_root.join(entry.path.as_str());
            if let Some(bytes) = file_size_if_exists(&path) {
                summary.bytes_recovered = summary.bytes_recovered.saturating_add(bytes);
            }
            summary.artifacts_removed += 1;
            if !dry_run {
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(CacheError::io("remove", &path, e)),
                }
            }
        }

        if !dry_run && matches!(tier, GcTier::Cache) {
            remove_empty_shards(&codegen_root)?;
        }

        if !dry_run {
            match fs::remove_file(&marker) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(CacheError::io("remove", &marker, e)),
            }
        }

        Ok(summary)
    }
}

/// Whether a manifest entry's tier matches the requested GC tier.
fn entry_in_tier(entry_tier: crate::tier::Tier, gc_tier: GcTier) -> bool {
    matches!(
        (entry_tier, gc_tier),
        (crate::tier::Tier::Repo, GcTier::Repo) | (crate::tier::Tier::Cache, GcTier::Cache)
    )
}

/// Try to read a file's size. Returns `None` if the file does not
/// exist or its metadata is unreadable.
fn file_size_if_exists(path: &Path) -> Option<u64> {
    fs::metadata(path).ok().map(|m| m.len())
}

/// Walk `<project>/.edda/cache/codegen/` and remove any shard
/// subdirectories that have no remaining `.ea` files. Shard names
/// are 4 lowercase hex characters; we use that as a discriminator so
/// we never touch the staging directory or the manifest file.
fn remove_empty_shards(codegen_root: &Path) -> Result<(), CacheError> {
    let entries = match fs::read_dir(codegen_root) {
        Ok(iter) => iter,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(CacheError::io("read_dir", codegen_root, e)),
    };
    for entry in entries {
        let entry = entry.map_err(|e| CacheError::io("read_dir", codegen_root, e))?;
        if !entry
            .file_type()
            .map(|t| t.is_dir())
            .unwrap_or(false)
        {
            continue;
        }
        let name_os = entry.file_name();
        let Some(name) = name_os.to_str() else {
            continue;
        };
        if !is_shard_name(name) {
            continue;
        }
        let path = entry.path();
        let is_empty = match fs::read_dir(&path) {
            Ok(mut iter) => iter.next().is_none(),
            Err(e) => return Err(CacheError::io("read_dir", &path, e)),
        };
        if is_empty {
            match fs::remove_dir(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(CacheError::io("remove", &path, e)),
            }
        }
    }
    Ok(())
}

/// Recognise a hash-shard subdirectory name.
fn is_shard_name(name: &str) -> bool {
    name.len() == 4
        && name
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Compute the marker-file path.
fn marker_path(project_root: &Path) -> PathBuf {
    paths::codegen_cache_root(project_root).join(MARKER_FILENAME)
}

/// Whether two `OffsetDateTime`s fall on the same UTC calendar day.
fn same_utc_day(a: OffsetDateTime, b: OffsetDateTime) -> bool {
    let a = a.to_offset(time::UtcOffset::UTC);
    let b = b.to_offset(time::UtcOffset::UTC);
    a.date() == b.date()
}

/// Whether two `OffsetDateTime`s fall in the same Monday-anchored UTC
/// week.
fn same_utc_iso_week(a: OffsetDateTime, b: OffsetDateTime) -> bool {
    let a = a.to_offset(time::UtcOffset::UTC).date();
    let b = b.to_offset(time::UtcOffset::UTC).date();
    let (year_a, week_a, _) = a.to_iso_week_date();
    let (year_b, week_b, _) = b.to_iso_week_date();
    year_a == year_b && week_a == week_b
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn schedule_name_round_trip() {
        for &s in &[
            GcSchedule::Never,
            GcSchedule::OnCi,
            GcSchedule::Daily,
            GcSchedule::Weekly,
            GcSchedule::OnPromoteRevoke,
        ] {
            assert_eq!(GcSchedule::from_name(s.name()), Some(s));
        }
    }

    #[test]
    fn schedule_default_is_never() {
        assert_eq!(GcSchedule::default(), GcSchedule::Never);
    }

    #[test]
    fn schedule_unknown_name_is_none() {
        assert!(GcSchedule::from_name("hourly").is_none());
        assert!(GcSchedule::from_name("").is_none());
    }

    #[test]
    fn never_never_runs() {
        let now = datetime!(2026-05-11 14:55:00 UTC);
        assert!(!GcSchedule::Never.should_run(now, None, true));
        assert!(!GcSchedule::Never.should_run(now, Some(now), true));
    }

    #[test]
    fn on_promote_revoke_is_event_driven() {
        let now = datetime!(2026-05-11 14:55:00 UTC);
        assert!(!GcSchedule::OnPromoteRevoke.should_run(now, None, false));
        assert!(!GcSchedule::OnPromoteRevoke.should_run(now, None, true));
    }

    #[test]
    fn on_ci_requires_ci_env() {
        let now = datetime!(2026-05-11 14:55:00 UTC);
        assert!(GcSchedule::OnCi.should_run(now, None, true));
        assert!(!GcSchedule::OnCi.should_run(now, None, false));
    }

    #[test]
    fn daily_runs_on_first_build_of_day() {
        let day1_first = datetime!(2026-05-11 00:30:00 UTC);
        let day1_later = datetime!(2026-05-11 23:00:00 UTC);
        let day2 = datetime!(2026-05-12 01:00:00 UTC);
        assert!(GcSchedule::Daily.should_run(day1_first, None, false));
        assert!(!GcSchedule::Daily.should_run(day1_later, Some(day1_first), false));
        assert!(GcSchedule::Daily.should_run(day2, Some(day1_first), false));
    }

    #[test]
    fn weekly_runs_on_first_build_of_week() {
        // ISO-8601 weeks: 2026-05-11 (Mon) starts a week; the Saturday
        // 5 days later is in the same week; the next Monday is the
        // next week.
        let mon_a = datetime!(2026-05-11 12:00:00 UTC);
        let sat_a = datetime!(2026-05-16 12:00:00 UTC);
        let mon_b = datetime!(2026-05-18 12:00:00 UTC);
        assert!(GcSchedule::Weekly.should_run(mon_a, None, false));
        assert!(!GcSchedule::Weekly.should_run(sat_a, Some(mon_a), false));
        assert!(GcSchedule::Weekly.should_run(mon_b, Some(mon_a), false));
    }

    #[test]
    fn is_shard_name_validates_four_lowercase_hex() {
        assert!(is_shard_name("a3f2"));
        assert!(is_shard_name("0000"));
        assert!(is_shard_name("ffff"));
        assert!(!is_shard_name("ABCD"));
        assert!(!is_shard_name("a3f"));
        assert!(!is_shard_name("a3f23"));
        assert!(!is_shard_name(".staging"));
        assert!(!is_shard_name("manifest.toon"));
    }
}
