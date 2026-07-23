//! Integration: lockfile disk round-trip and tamper detection (§7 + class α).
//!
//! Verifies two cross-crate properties from
//! `corpus/edda-codex/language/08-packages.md` §7:
//!
//! 1. **Round-trip**: write a `Lockfile` to disk via `edda_manifest::lockfile::write`,
//!    read it back via `edda_manifest::lockfile::load`, and assert the in-memory
//!    representation is identical.
//!
//! 2. **Tamper detection**: hand-edit one `rune_hash` field in the written file
//!    (leaving the `lockfile_hash` trailer stale), then re-load. Assert that
//!    `load` returns `Err(LockfileError::Tampered)` and pushes exactly one
//!    `DiagnosticClass::LockfileTampered` diagnostic into `diags`.
//!
//! Slice ζ already has unit tests for the in-memory parse path (in
//! `edda-manifest/src/lockfile.rs`). This integration test adds the disk
//! round-trip via `tempfile` and exercises the cross-crate flow between
//! `edda-manifest` and `edda-diag`.
//!
//! Corresponds to slice I, Test 5.

use edda_diag::{DiagnosticClass, Diagnostics, LintConfig};
use edda_manifest::lockfile::{compute_trailer, load, write, Lockfile, LockfileEntry, LockfileError, LockfileMeta};
use edda_span::SourceMap;

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

/// Build a minimal `LockfileEntry` for testing.
fn make_entry(name: &str) -> LockfileEntry {
    LockfileEntry {
        name: name.to_owned(),
        version: "1.2.3".to_owned(),
        source: "mimir.edda.dev".to_owned(),
        rune_hash: "blake3:0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
        surface_hash: "blake3:1111111111111111111111111111111111111111111111111111111111111111".to_owned(),
        effect_hash: "blake3:2222222222222222222222222222222222222222222222222222222222222222".to_owned(),
        publisher_key: "ed25519:aabbccdd".to_owned(),
        publisher_sig: "ed25519:eeff0011".to_owned(),
        deps: vec![],
    }
}

/// Build a `Lockfile` from a slice of entries with correct trailer.
fn make_lockfile(entries: &[LockfileEntry]) -> Lockfile {
    Lockfile {
        runes: entries.to_vec(),
        meta: LockfileMeta {
            lockfile_hash: compute_trailer(entries),
        },
    }
}

/// Construct a `LintConfig` that uses every class's default severity.
fn default_lint() -> LintConfig {
    LintConfig::new()
}

// ---------------------------------------------------------------------------
// Test 5a: disk round-trip with one rune entry
// ---------------------------------------------------------------------------

#[test]
fn lockfile_disk_roundtrip_one_entry() {
    let entry = make_entry("anyhow");
    let lf = make_lockfile(&[entry.clone()]);

    let dir = tempfile::tempdir().expect("tempdir should succeed");
    let path = dir.path().join("package.lock.toml");

    write(&lf, &path).expect("write must succeed");

    let sm = SourceMap::new();
    let mut diags = Diagnostics::new();
    let lint_cfg = default_lint();

    let loaded = load(&path, &sm, &mut diags, &lint_cfg)
        .expect("load of a freshly written lockfile must succeed");

    assert!(
        diags.is_empty(),
        "no diagnostics expected on a valid lockfile; got {}",
        diags.len()
    );
    assert_eq!(
        loaded.runes, lf.runes,
        "loaded runes must equal the originally written runes"
    );
    assert_eq!(
        loaded.meta, lf.meta,
        "loaded lockfile_meta must equal the originally written meta"
    );
}

// ---------------------------------------------------------------------------
// Test 5b: disk round-trip with two rune entries and deps link
// ---------------------------------------------------------------------------

#[test]
fn lockfile_disk_roundtrip_two_entries_with_deps() {
    let a = make_entry("thiserror");
    let mut b = make_entry("serde");
    b.deps = vec!["thiserror".to_owned()];

    let lf = make_lockfile(&[a.clone(), b.clone()]);

    let dir = tempfile::tempdir().expect("tempdir should succeed");
    let path = dir.path().join("package.lock.toml");

    write(&lf, &path).expect("write must succeed");

    let sm = SourceMap::new();
    let mut diags = Diagnostics::new();
    let lint_cfg = default_lint();

    let loaded = load(&path, &sm, &mut diags, &lint_cfg)
        .expect("load of two-entry lockfile must succeed");

    assert!(diags.is_empty(), "no diagnostics expected; got {}", diags.len());
    assert_eq!(loaded.runes.len(), 2, "must have two rune entries");
    assert_eq!(loaded.runes[0].name, "thiserror");
    assert_eq!(loaded.runes[1].name, "serde");
    assert_eq!(loaded.runes[1].deps, vec!["thiserror"]);
}

// ---------------------------------------------------------------------------
// Test 5c: tamper — replace one rune_hash → Tampered + LockfileTampered diag
// ---------------------------------------------------------------------------

#[test]
fn lockfile_tampered_rune_hash_triggers_tampered_error() {
    let entry = make_entry("tokio");
    let lf = make_lockfile(&[entry.clone()]);

    let dir = tempfile::tempdir().expect("tempdir should succeed");
    let path = dir.path().join("package.lock.toml");

    write(&lf, &path).expect("write must succeed");

    // Read the file, tamper the rune_hash, write it back (leaving the trailer stale).
    let content = std::fs::read_to_string(&path).expect("read file must succeed");
    // The original rune_hash value; replace it with a different blake3 string.
    let tampered_content = content.replace(
        "blake3:0000000000000000000000000000000000000000000000000000000000000000",
        "blake3:FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF0000",
    );
    // Sanity: the tampering must have changed the content.
    assert_ne!(
        content, tampered_content,
        "replacement must have occurred (test precondition)"
    );
    std::fs::write(&path, tampered_content.as_bytes()).expect("write tampered file must succeed");

    // Now load the tampered file — must return Err(Tampered).
    let sm = SourceMap::new();
    let mut diags = Diagnostics::new();
    let lint_cfg = default_lint();

    let result = load(&path, &sm, &mut diags, &lint_cfg);

    assert!(
        matches!(result, Err(LockfileError::Tampered)),
        "load of tampered lockfile must return Err(Tampered); got {:?}",
        result
    );

    // Exactly one LockfileTampered diagnostic must be in diags.
    assert_eq!(
        diags.len(),
        1,
        "exactly one diagnostic expected; got {}",
        diags.len()
    );
    let diag = diags.iter().next().unwrap();
    assert_eq!(
        diag.class,
        DiagnosticClass::LockfileTampered,
        "diagnostic class must be LockfileTampered; got {:?}",
        diag.class
    );
}

// ---------------------------------------------------------------------------
// Test 5d: tamper — add an rune entry after write → Tampered
// ---------------------------------------------------------------------------

#[test]
fn lockfile_injected_entry_triggers_tampered_error() {
    let entry = make_entry("futures");
    let lf = make_lockfile(&[entry]);

    let dir = tempfile::tempdir().expect("tempdir should succeed");
    let path = dir.path().join("package.lock.toml");

    write(&lf, &path).expect("write must succeed");

    // Inject a second [[rune]] block before [lockfile_meta] to simulate injection.
    let content = std::fs::read_to_string(&path).expect("read file must succeed");
    let injected_block = "[[rune]]\n\
        name           = \"tokio\"\n\
        version        = \"1.2.3\"\n\
        source         = \"mimir.edda.dev\"\n\
        rune_hash      = \"blake3:abcd\"\n\
        surface_hash   = \"blake3:1234\"\n\
        effect_hash    = \"blake3:5678\"\n\
        publisher_key  = \"ed25519:pk\"\n\
        publisher_sig  = \"ed25519:sig\"\n\
        deps           = []\n\n";
    let tampered_content = content.replace("[lockfile_meta]", &format!("{}\n[lockfile_meta]", injected_block));
    assert_ne!(content, tampered_content, "injection must have occurred");
    std::fs::write(&path, tampered_content.as_bytes()).expect("write must succeed");

    let sm = SourceMap::new();
    let mut diags = Diagnostics::new();
    let lint_cfg = default_lint();

    let result = load(&path, &sm, &mut diags, &lint_cfg);

    assert!(
        matches!(result, Err(LockfileError::Tampered)),
        "injected entry must cause Err(Tampered); got {:?}",
        result
    );
    assert_eq!(diags.len(), 1, "exactly one diagnostic expected; got {}", diags.len());
    assert_eq!(
        diags.iter().next().unwrap().class,
        DiagnosticClass::LockfileTampered
    );
}

// ---------------------------------------------------------------------------
// Test 5e: empty lockfile round-trips correctly
// ---------------------------------------------------------------------------

#[test]
fn empty_lockfile_disk_roundtrip() {
    let lf = make_lockfile(&[]);

    let dir = tempfile::tempdir().expect("tempdir should succeed");
    let path = dir.path().join("package.lock.toml");

    write(&lf, &path).expect("write must succeed");

    let sm = SourceMap::new();
    let mut diags = Diagnostics::new();
    let lint_cfg = default_lint();

    let loaded = load(&path, &sm, &mut diags, &lint_cfg)
        .expect("load of empty lockfile must succeed");

    assert!(diags.is_empty(), "no diagnostics for empty lockfile; got {}", diags.len());
    assert!(loaded.runes.is_empty(), "loaded runes must be empty");
    assert_eq!(
        loaded.meta.lockfile_hash, lf.meta.lockfile_hash,
        "trailer must match"
    );
}

// ---------------------------------------------------------------------------
// Test 5f: compute_trailer is stable across two calls (determinism)
// ---------------------------------------------------------------------------

#[test]
fn compute_trailer_is_deterministic() {
    let entries = vec![make_entry("async-std"), make_entry("futures")];
    let h1 = compute_trailer(&entries);
    let h2 = compute_trailer(&entries);
    assert_eq!(h1, h2, "compute_trailer must be deterministic");
    assert!(h1.starts_with("blake3:"), "trailer must have blake3: prefix");
}
