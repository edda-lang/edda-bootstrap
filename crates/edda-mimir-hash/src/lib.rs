//! Three Mímir hash walkers for the `.rune` trust chain.
//!
//! Implements §4 of `corpus/edda-codex/language/08-packages.md`:
//! - §4.1 `rune_hash` — BLAKE3 of the `.tar.zst` archive bytes.
//! - §4.2 `surface_hash` — BLAKE3 over stable-only surface/*.toon canonical bytes.
//! - §4.3 `effect_hash` — BLAKE3 over the sorted, deduplicated union of all
//!   effect-row entries reachable from public functions in the surface.
//!
//! Each hash answers a different question about the dependency; their
//! independence is load-bearing. A legitimate patch changes only `rune_hash`;
//! an API change bumps `surface_hash`; effect-surface growth bumps
//! `effect_hash`.
//!
//! This crate does NOT perform disk I/O. Callers (slice H: `edda publish` and
//! `edda update`) handle reading/writing and provide the raw bytes.
//!
//! # Hash routing
//!
//! All BLAKE3 computation routes through [`edda_cache::hash_bytes`] per the
//! workspace single-BLAKE3-binding rule.

mod effect;
mod surface;
mod rune;

pub use effect::compute_effect_hash;
pub use surface::compute_surface_hash;
pub use rune::compute_rune_hash;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors that the hash walkers can report.
#[derive(Debug)]
pub enum HashError {
    /// A `surface/*.toon` file could not be parsed or filtered.
    SurfaceParse {
        /// Archive-relative path of the offending file.
        file: String,
        /// Human-readable description of the parse failure.
        msg: String,
    },
    /// Canonical encoding of a surface file's stable subset failed.
    CanonicalEncode(String),
}

impl std::fmt::Display for HashError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HashError::SurfaceParse { file, msg } => {
                write!(f, "surface parse error in {file:?}: {msg}")
            }
            HashError::CanonicalEncode(msg) => write!(f, "canonical encode error: {msg}"),
        }
    }
}

impl std::error::Error for HashError {}

// ---------------------------------------------------------------------------
// ArchiveHashes
// ---------------------------------------------------------------------------

/// The three rollup hashes plus per-file BLAKE3s for `hashes.toon`.
///
/// This is the shape that the `hashes.toon` file inside a `.rune` serialises.
/// Produced by [`compute_archive_hashes`] on the `edda publish` path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveHashes {
    /// BLAKE3 of the `.tar.zst` archive bytes (`"blake3:<hex>"`).
    pub rune_hash: String,
    /// BLAKE3 over the canonical stable-only surface/*.toon bytes (`"blake3:<hex>"`).
    pub surface_hash: String,
    /// BLAKE3 over the sorted deduplicated effect-row entries (`"blake3:<hex>"`).
    pub effect_hash: String,
    /// Per-file BLAKE3s `(path, "blake3:<hex>")`, sorted lex by path.
    pub files: Vec<(String, String)>,
}

// ---------------------------------------------------------------------------
// compute_archive_hashes — convenience wrapper for edda publish
// ---------------------------------------------------------------------------

/// Compute all three rollup hashes plus per-file BLAKE3s for `hashes.toon`.
///
/// Convenience wrapper for the `edda publish` path. Combines the three
/// independent hash walkers and the per-file hash table into one call.
///
/// - `archive_bytes`: the freshly-packed `.tar.zst` bytes (for `rune_hash`).
/// - `surface_files`: `(module-leaf, .toon bytes)` pairs extracted from the
///   `RuneLayout.surface` field (for `surface_hash` and `effect_hash`).
/// - `all_files`: `(archive-relative-path, bytes)` pairs for every file in
///   the layout (for the per-file hash table in `hashes.toon`).
pub fn compute_archive_hashes(
    archive_bytes: &[u8],
    surface_files: &[(String, Vec<u8>)],
    all_files: &[(String, Vec<u8>)],
) -> Result<ArchiveHashes, HashError> {
    let rune_hash = compute_rune_hash(archive_bytes);
    let surface_hash = compute_surface_hash(surface_files)?;
    let effect_hash = compute_effect_hash(surface_files)?;

    let mut files: Vec<(String, String)> = all_files
        .iter()
        .map(|(path, bytes)| {
            let hash = format!("blake3:{}", edda_cache::hash_bytes(bytes));
            (path.clone(), hash)
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));

    Ok(ArchiveHashes {
        rune_hash,
        surface_hash,
        effect_hash,
        files,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -------------------------------------------------------------------
    // Test 1: rune_hash determinism
    // -------------------------------------------------------------------

    #[test]
    fn rune_hash_is_deterministic_over_bytes() {
        let bytes = b"some archive bytes";
        let h1 = compute_rune_hash(bytes);
        let h2 = compute_rune_hash(bytes);
        assert_eq!(h1, h2);
        assert!(h1.starts_with("blake3:"));
        assert_eq!(h1.len(), 7 + 64, "blake3: prefix + 64 hex chars");
    }

    // -------------------------------------------------------------------
    // Test 2: rune_hash changes on bit flip
    // -------------------------------------------------------------------

    #[test]
    fn rune_hash_differs_on_one_byte_flip() {
        let mut bytes = b"archive bytes for flip test".to_vec();
        let h1 = compute_rune_hash(&bytes);
        bytes[0] ^= 0x01;
        let h2 = compute_rune_hash(&bytes);
        assert_ne!(h1, h2);
    }

    // -------------------------------------------------------------------
    // Test 3: surface_hash excludes unstable items
    // -------------------------------------------------------------------

    #[test]
    fn surface_hash_excludes_unstable_items() {
        // Two surface files: one with stable + unstable items, one with only stable.
        // The hashes should agree when comparing the mixed file (stable-only subset)
        // against a file that was authored with only the stable item.
        let mixed = b"schema_version: 3\nmodule: foo\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  push,(s: mutable Vec_T) -> () with {panic},,,Push.,\n\nunstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  experimental_op,(s: mutable Vec_T) -> () with {},,,Experimental.,\n";
        let stable_only = b"schema_version: 3\nmodule: foo\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  push,(s: mutable Vec_T) -> () with {panic},,,Push.,\n";

        let files_mixed = vec![("mod/foo".to_string(), mixed.to_vec())];
        let files_stable_only = vec![("mod/foo".to_string(), stable_only.to_vec())];

        let h_mixed = compute_surface_hash(&files_mixed).expect("mixed should succeed");
        let h_stable_only = compute_surface_hash(&files_stable_only).expect("stable-only should succeed");
        assert_eq!(
            h_mixed, h_stable_only,
            "surface_hash must exclude unstable items: hashes should match"
        );
    }

    // -------------------------------------------------------------------
    // Test 4: surface_hash is lex-order invariant (caller order doesn't matter)
    // -------------------------------------------------------------------

    #[test]
    fn surface_hash_is_lex_order_invariant() {
        let file_a = b"schema_version: 3\nmodule: alpha\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  fn_a,() -> () with {},,,Doc.,\n".to_vec();
        let file_b = b"schema_version: 3\nmodule: beta\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  fn_b,() -> () with {},,,Doc.,\n".to_vec();

        let order1 = vec![
            ("alpha".to_string(), file_a.clone()),
            ("beta".to_string(), file_b.clone()),
        ];
        let order2 = vec![
            ("beta".to_string(), file_b.clone()),
            ("alpha".to_string(), file_a.clone()),
        ];

        let h1 = compute_surface_hash(&order1).expect("order1 should succeed");
        let h2 = compute_surface_hash(&order2).expect("order2 should succeed");
        assert_eq!(h1, h2, "surface_hash must be invariant to caller input order");
    }

    // -------------------------------------------------------------------
    // Test 5: surface_hash changes on stable signature edit
    // -------------------------------------------------------------------

    #[test]
    fn surface_hash_changes_on_stable_signature_edit() {
        let file_v1 = b"schema_version: 3\nmodule: foo\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  push,(s: mutable Vec_T) -> () with {panic},,,Push.,\n";
        let file_v2 = b"schema_version: 3\nmodule: foo\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  push,(s: mutable Vec_T, extra: i64) -> () with {panic},,,Push.,\n";

        let files_v1 = vec![("mod/foo".to_string(), file_v1.to_vec())];
        let files_v2 = vec![("mod/foo".to_string(), file_v2.to_vec())];

        let h1 = compute_surface_hash(&files_v1).expect("v1 should succeed");
        let h2 = compute_surface_hash(&files_v2).expect("v2 should succeed");
        assert_ne!(h1, h2, "surface_hash must change when a stable signature changes");
    }

    // -------------------------------------------------------------------
    // Test 6: effect_hash unions and deduplicates entries
    // -------------------------------------------------------------------

    #[test]
    fn effect_hash_unions_dedups() {
        // Two public functions with overlapping effect-row entries.
        let file = b"schema_version: 3\nmodule: foo\n\nstable_items[2]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  fn_a,() -> () with {panic Filesystem},,,Doc.,\n  fn_b,() -> () with {panic Network},,,Doc.,\n";
        // The union is {Filesystem, Network, panic} — 3 distinct entries.
        // Hash of the file with only the union (each entry once).
        let file_deduped = b"schema_version: 3\nmodule: foo\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  fn_only,() -> () with {Filesystem Network panic},,,Doc.,\n";

        let files = vec![("mod/foo".to_string(), file.to_vec())];
        let files_deduped = vec![("mod/foo".to_string(), file_deduped.to_vec())];

        let h1 = compute_effect_hash(&files).expect("should succeed");
        let h2 = compute_effect_hash(&files_deduped).expect("should succeed");
        assert_eq!(h1, h2, "effect_hash must union and deduplicate entries");
    }

    // -------------------------------------------------------------------
    // Test 7: effect_hash is sorted regardless of input order
    // -------------------------------------------------------------------

    #[test]
    fn effect_hash_is_sorted() {
        // File with effects in one order.
        let file_order1 = b"schema_version: 3\nmodule: foo\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  fn_a,() -> () with {panic Filesystem Network},,,Doc.,\n";
        // File with effects in a different order — same logical set.
        let file_order2 = b"schema_version: 3\nmodule: foo\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  fn_a,() -> () with {Network panic Filesystem},,,Doc.,\n";

        let files1 = vec![("mod/foo".to_string(), file_order1.to_vec())];
        let files2 = vec![("mod/foo".to_string(), file_order2.to_vec())];

        let h1 = compute_effect_hash(&files1).expect("should succeed");
        let h2 = compute_effect_hash(&files2).expect("should succeed");
        assert_eq!(h1, h2, "effect_hash must produce identical output regardless of effect-row entry order in input");
    }

    // -------------------------------------------------------------------
    // Test 8: compute_archive_hashes round-trip
    // -------------------------------------------------------------------

    #[test]
    fn compute_archive_hashes_round_trip() {
        let archive_bytes = b"fake tar.zst bytes for test";
        let surface_files = vec![
            (
                "alpha".to_string(),
                b"schema_version: 3\nmodule: alpha\n\nstable_items[0]{name,signature,effect_row,refinements,doc_high,doc_med}:\n".to_vec(),
            ),
        ];
        let all_files: Vec<(String, Vec<u8>)> = vec![
            ("manifest.toml".to_string(), b"[package]\nname = \"foo\"\n".to_vec()),
            ("surface/alpha.toon".to_string(), b"alpha surface content".to_vec()),
            ("index.toon".to_string(), b"schema_version: 3\n".to_vec()),
        ];

        let hashes = compute_archive_hashes(archive_bytes, &surface_files, &all_files)
            .expect("should succeed");

        assert!(hashes.rune_hash.starts_with("blake3:"), "rune_hash must have blake3: prefix");
        assert!(hashes.surface_hash.starts_with("blake3:"), "surface_hash must have blake3: prefix");
        assert!(hashes.effect_hash.starts_with("blake3:"), "effect_hash must have blake3: prefix");

        // All files present in the files table.
        assert_eq!(hashes.files.len(), all_files.len());
        for (path, _) in &all_files {
            assert!(
                hashes.files.iter().any(|(p, _)| p == path),
                "files table must contain {path:?}"
            );
        }

        // Files are sorted lex by path.
        let mut sorted_paths: Vec<&str> = hashes.files.iter().map(|(p, _)| p.as_str()).collect();
        sorted_paths.sort();
        let actual_paths: Vec<&str> = hashes.files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(sorted_paths, actual_paths, "files must be sorted lex by path");

        // Each file hash is the correct BLAKE3 of its bytes.
        for (path, bytes) in &all_files {
            let expected = format!("blake3:{}", edda_cache::hash_bytes(bytes));
            let found = hashes.files.iter().find(|(p, _)| p == path).unwrap();
            assert_eq!(found.1, expected, "per-file BLAKE3 mismatch for {path:?}");
        }
    }

    // -------------------------------------------------------------------
    // Test 9: rune_hash independence — same surface, different archive bytes
    // -------------------------------------------------------------------

    #[test]
    fn rune_hash_independence() {
        let surface_files = vec![
            (
                "mod/foo".to_string(),
                b"schema_version: 3\nmodule: foo\n\nstable_items[0]{name,signature,effect_row,refinements,doc_high,doc_med}:\n".to_vec(),
            ),
        ];

        let archive_v1 = b"archive bytes version 1";
        let archive_v2 = b"archive bytes version 2 with different content";

        let h1 = compute_rune_hash(archive_v1);
        let h2 = compute_rune_hash(archive_v2);
        let s1 = compute_surface_hash(&surface_files).unwrap();
        let e1 = compute_effect_hash(&surface_files).unwrap();

        // rune_hash changes.
        assert_ne!(h1, h2, "rune_hash must differ when archive bytes differ");

        // surface_hash and effect_hash are the same for both — they don't depend on archive bytes.
        let s2 = compute_surface_hash(&surface_files).unwrap();
        let e2 = compute_effect_hash(&surface_files).unwrap();
        assert_eq!(s1, s2, "surface_hash must not depend on archive bytes");
        assert_eq!(e1, e2, "effect_hash must not depend on archive bytes");
    }

    // -------------------------------------------------------------------
    // Test 10: surface_hash independence — same archive, different stable signature
    // -------------------------------------------------------------------

    #[test]
    fn surface_hash_independence() {
        let archive_bytes = b"same archive bytes";
        let rune_hash = compute_rune_hash(archive_bytes);

        let surface_v1 = vec![(
            "mod/foo".to_string(),
            b"schema_version: 3\nmodule: foo\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  push,(s: mutable Vec_T) -> () with {},,,Push.,\n".to_vec(),
        )];
        let surface_v2 = vec![(
            "mod/foo".to_string(),
            b"schema_version: 3\nmodule: foo\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  push,(s: mutable Vec_T, extra: i64) -> () with {},,,Push.,\n".to_vec(),
        )];

        let s1 = compute_surface_hash(&surface_v1).unwrap();
        let s2 = compute_surface_hash(&surface_v2).unwrap();

        // surface_hash differs between the two surface versions.
        assert_ne!(s1, s2, "surface_hash must differ when stable signature changes");

        // rune_hash is unchanged — it only depends on archive bytes, not surface.
        let rune_hash2 = compute_rune_hash(archive_bytes);
        assert_eq!(rune_hash, rune_hash2, "rune_hash must not depend on surface contents");
    }
}
