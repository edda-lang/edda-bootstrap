//! Pure path computation for repo-tier, cache-tier, global-cache, and
//! staging directories.
//!
//! Every function here is referentially transparent: same inputs always
//! produce the same [`PathBuf`]. No filesystem access, no environment
//! reads. The store module composes these with I/O.
//!
//! Path shapes (per `storage.md` §7 and `build-system.md` §5–6):
//!
//! ```text
//! repo-tier:   <project>/codegen/<spec/qualified/path>/<name>.ea
//! cache-tier:  <project>/.edda/cache/codegen/<XXXX>/<name>.ea
//! global:      <home>/.edda/global-cache/codegen/<XXXX>/<name>.ea
//! staging:     <project>/.edda/cache/codegen/.staging/<uuid>/
//! ```
//!
//! where `<XXXX>` is the first 4 hex characters of the artifact's full
//! BLAKE3 hash.

use std::path::{Path, PathBuf};

use crate::hash::ArtifactHash;
use crate::name::ArtifactName;

/// Top-level build-state directory inside a project.
pub(crate) const EDDA_DIR: &str = ".edda";

/// The repo-tier root directory inside a project.
pub(crate) const REPO_TIER_ROOT: &str = "codegen";

/// The cache-tier root directory under `.edda/`.
pub(crate) const CACHE_TIER_ROOT: &str = "cache";

/// The codegen subdirectory under both cache-tier roots.
pub(crate) const CODEGEN_SUBDIR: &str = "codegen";

/// The typecheck subdirectory under `.edda/cache/`.
pub(crate) const TYPECHECK_SUBDIR: &str = "typecheck";

/// The objects subdirectory under `.edda/cache/`.
pub(crate) const OBJECTS_SUBDIR: &str = "objects";

/// The staging subdirectory used during cascade commits.
pub(crate) const STAGING_SUBDIR: &str = ".staging";

/// The global-cache root under the user's home directory.
pub(crate) const GLOBAL_CACHE_DIR: &str = ".edda/global-cache";

/// File extension for generated artifacts.
const ARTIFACT_EXT: &str = "ea";

/// Compute the repo-tier path for an artifact.
///
/// `spec_qualified` is the spec's fully qualified name split on dots —
/// `std.option.Option` → `["std", "option", "Option"]`. The returned path
/// is `<project_root>/codegen/std/option/Option/<name>.ea`. Per
/// `storage.md` §7's worked examples the convention is to use the spec's
/// *parent* qualified path (e.g. `std/option/`) rather than including the
/// spec leaf as a directory; callers pass `["std", "option"]` for the
/// `Option_i32` artifact. This function does not enforce that distinction —
/// it joins exactly the segments it is given.
pub fn repo_tier_path(
    project_root: &Path,
    spec_qualified: &[&str],
    name: &ArtifactName,
) -> PathBuf {
    let mut path = project_root.to_path_buf();
    path.push(REPO_TIER_ROOT);
    for seg in spec_qualified {
        debug_assert!(
            !seg.is_empty(),
            "repo_tier_path: empty spec-qualified segment"
        );
        path.push(seg);
    }
    path.push(artifact_filename(name));
    path
}

/// Compute the cache-tier path for an artifact, sharded by the first 4 hex
/// characters of `hash` per `storage.md` §7.
pub fn cache_tier_path(
    project_root: &Path,
    hash: &ArtifactHash,
    name: &ArtifactName,
) -> PathBuf {
    let mut path = project_root.to_path_buf();
    path.push(EDDA_DIR);
    path.push(CACHE_TIER_ROOT);
    path.push(CODEGEN_SUBDIR);
    path.push(shard_str(hash));
    path.push(artifact_filename(name));
    path
}

/// Compute the global-cache path for an artifact.
///
/// Per `build-system.md` §6 the global cache uses the same shard layout as
/// the per-project cache tier, with no per-project subdirectory.
/// `home` is typically `$HOME` on POSIX or `%USERPROFILE%` on Windows; the
/// caller is responsible for sourcing it (see [`crate::GlobalCache`]).
pub fn global_cache_path(home: &Path, hash: &ArtifactHash, name: &ArtifactName) -> PathBuf {
    let mut path = home.to_path_buf();
    for seg in GLOBAL_CACHE_DIR.split('/') {
        path.push(seg);
    }
    path.push(CODEGEN_SUBDIR);
    path.push(shard_str(hash));
    path.push(artifact_filename(name));
    path
}

/// Compute the staging directory for a cascade with id `uuid`. Per
/// `migration.md` §3, the build writes regenerated artifacts here and then
/// atomically renames them to their final tier locations.
pub fn staging_dir(project_root: &Path, uuid: &str) -> PathBuf {
    let mut path = project_root.to_path_buf();
    path.push(EDDA_DIR);
    path.push(CACHE_TIER_ROOT);
    path.push(CODEGEN_SUBDIR);
    path.push(STAGING_SUBDIR);
    path.push(uuid);
    path
}

/// Path of the codegen-tier manifest file.
pub(crate) fn codegen_manifest_path(project_root: &Path) -> PathBuf {
    let mut path = project_root.to_path_buf();
    path.push(EDDA_DIR);
    path.push(CACHE_TIER_ROOT);
    path.push(CODEGEN_SUBDIR);
    path.push("manifest.toon");
    path
}

/// Root of `<project>/.edda/cache/codegen/`.
pub(crate) fn codegen_cache_root(project_root: &Path) -> PathBuf {
    let mut path = project_root.to_path_buf();
    path.push(EDDA_DIR);
    path.push(CACHE_TIER_ROOT);
    path.push(CODEGEN_SUBDIR);
    path
}

/// Root of the typecheck cache (`<project>/.edda/cache/typecheck/`).
/// Per `build-system.md` §5; the per-module typecheck digests live
/// under this directory.
pub fn typecheck_cache_root(project_root: &Path) -> PathBuf {
    let mut path = project_root.to_path_buf();
    path.push(EDDA_DIR);
    path.push(CACHE_TIER_ROOT);
    path.push(TYPECHECK_SUBDIR);
    path
}

/// Root of the objects cache (`<project>/.edda/cache/objects/`). Per
/// `build-system.md` §5; per-target, per-module object files live
/// under this directory.
pub fn objects_cache_root(project_root: &Path) -> PathBuf {
    let mut path = project_root.to_path_buf();
    path.push(EDDA_DIR);
    path.push(CACHE_TIER_ROOT);
    path.push(OBJECTS_SUBDIR);
    path
}

/// Convert a hash's 4-byte ASCII shard array to a `&str` for `Path::push`.
fn shard_str(hash: &ArtifactHash) -> String {
    let bytes = hash.shard();
    // SAFETY: hash.shard() returns ASCII hex, which is valid UTF-8 — but
    // PathBuf::push wants an owned/borrowed OsStr-compatible value; we
    // simply copy into a small String to avoid any platform-specific
    // OsStr surface.
    String::from(std::str::from_utf8(&bytes).expect("ArtifactHash::shard returns ASCII hex"))
}

/// Build the on-disk filename for an artifact: `<short>__<prefix>.ea`.
fn artifact_filename(name: &ArtifactName) -> String {
    format!("{}.{}", name, ARTIFACT_EXT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::hash_bytes;

    fn sample_name(short: &str) -> ArtifactName {
        let prefix = hash_bytes(short.as_bytes()).prefix();
        ArtifactName::new(short, prefix).unwrap()
    }

    #[test]
    fn repo_tier_path_joins_dotted_segments() {
        let root = Path::new("/p");
        let name = sample_name("Option_i32");
        let path = repo_tier_path(root, &["std", "option"], &name);
        let mut expected = PathBuf::from("/p");
        expected.push("codegen");
        expected.push("std");
        expected.push("option");
        expected.push(format!("{}.ea", name));
        assert_eq!(path, expected);
    }

    #[test]
    fn cache_tier_path_uses_4_hex_shard() {
        let root = Path::new("/p");
        let hash = hash_bytes(b"any");
        let name = sample_name("Stack_i32");
        let path = cache_tier_path(root, &hash, &name);
        let shard = String::from_utf8(hash.shard().to_vec()).unwrap();
        let mut expected = PathBuf::from("/p");
        expected.push(".edda");
        expected.push("cache");
        expected.push("codegen");
        expected.push(shard);
        expected.push(format!("{}.ea", name));
        assert_eq!(path, expected);
    }

    #[test]
    fn cache_tier_shard_matches_hash() {
        let hash = hash_bytes(b"x");
        let name = sample_name("X");
        let path = cache_tier_path(Path::new("/p"), &hash, &name);
        let parent = path.parent().unwrap();
        let shard_dir = parent.file_name().unwrap().to_str().unwrap();
        assert_eq!(shard_dir.len(), 4);
        assert!(shard_dir.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    #[test]
    fn global_cache_path_omits_project_segment() {
        let home = Path::new("/home/alice");
        let hash = hash_bytes(b"g");
        let name = sample_name("Option_String");
        let path = global_cache_path(home, &hash, &name);
        let shard = String::from_utf8(hash.shard().to_vec()).unwrap();
        let mut expected = PathBuf::from("/home/alice");
        expected.push(".edda");
        expected.push("global-cache");
        expected.push("codegen");
        expected.push(shard);
        expected.push(format!("{}.ea", name));
        assert_eq!(path, expected);
    }

    #[test]
    fn staging_dir_includes_uuid() {
        let root = Path::new("/p");
        let path = staging_dir(root, "abc123");
        let mut expected = PathBuf::from("/p");
        expected.push(".edda");
        expected.push("cache");
        expected.push("codegen");
        expected.push(".staging");
        expected.push("abc123");
        assert_eq!(path, expected);
    }

    #[test]
    fn manifest_path_is_in_codegen_cache() {
        let path = codegen_manifest_path(Path::new("/p"));
        let mut expected = PathBuf::from("/p");
        expected.push(".edda");
        expected.push("cache");
        expected.push("codegen");
        expected.push("manifest.toon");
        assert_eq!(path, expected);
    }

    #[test]
    fn cache_roots_distinct() {
        let p = Path::new("/p");
        let cg = codegen_cache_root(p);
        let tc = typecheck_cache_root(p);
        let ob = objects_cache_root(p);
        assert_ne!(cg, tc);
        assert_ne!(tc, ob);
        assert_ne!(cg, ob);
    }
}
