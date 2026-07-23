//! Integration: hash reproducibility across two independent computations.
//!
//! Verifies §4 (independence) and §5 (canonical encoding) properties from
//! `corpus/edda-codex/language/08-packages.md`:
//! - Two calls with identical inputs produce bit-identical hashes.
//! - Input ordering of unordered fields does not affect the hash.
//! - Each of the three hash walkers (rune_hash, surface_hash, effect_hash)
//!   is tested independently.
//!
//! Corresponds to slice I, Test 1.

use edda_mimir_archive::{build_hashes_toon, RuneLayout};
use edda_mimir_hash::{compute_archive_hashes, compute_effect_hash, compute_surface_hash, compute_rune_hash};

// ---------------------------------------------------------------------------
// Shared fixture helpers
// ---------------------------------------------------------------------------

/// Construct a minimal but non-trivial RuneLayout for testing.
fn make_layout() -> RuneLayout {
    let manifest = b"[package]\nname = \"myhash-test\"\nversion = \"1.0.0\"\n".to_vec();
    let surface = vec![
        (
            "alpha".to_string(),
            b"schema_version: 3\nmodule: alpha\n\nstable_items[2]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  push,(s: mutable Vec_T) -> () with {panic},,,Push.,\n  pop,(s: mutable Vec_T) -> () with {},,,Pop.,\n\nunstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  experimental,() -> () with {Filesystem},,,Exp.,\n".to_vec(),
        ),
        (
            "beta".to_string(),
            b"schema_version: 3\nmodule: beta\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  connect,(host: String) -> () with {Network err: alloc.AllocError},,,Connect.,\n".to_vec(),
        ),
    ];
    let mir = vec![
        ("alpha".to_string(), b"# alpha mir bytes\n".to_vec()),
        ("beta".to_string(), b"# beta mir bytes\n".to_vec()),
    ];
    let objects = vec![
        ("x86_64-pc-windows-msvc".to_string(), "alpha.o".to_string(), b"alpha-object-bytes".to_vec()),
    ];
    let index = b"schema_version: 3\nmodule: myhash-test\n".to_vec();
    let signature = b"placeholder-signature".to_vec();
    let publisher_key = b"ed25519:placeholder-key\nfingerprint: ed25519:abc123\n".to_vec();
    let hashes = build_hashes_toon(&manifest, &surface, &mir, &objects, &index);

    RuneLayout {
        manifest_toml: manifest,
        surface,
        mir,
        objects,
        index_toon: index,
        hashes_toon: hashes,
        signature_bin: signature,
        publisher_key,
    }
}

/// Build the `all_files` vec that `compute_archive_hashes` expects from a layout.
fn layout_all_files(layout: &RuneLayout) -> Vec<(String, Vec<u8>)> {
    let mut all: Vec<(String, Vec<u8>)> = Vec::new();
    all.push(("manifest.toml".to_string(), layout.manifest_toml.clone()));
    for (leaf, bytes) in &layout.surface {
        all.push((format!("surface/{}.toon", leaf), bytes.clone()));
    }
    for (leaf, bytes) in &layout.mir {
        all.push((format!("mir/{}.mir", leaf), bytes.clone()));
    }
    for (triple, leaf, bytes) in &layout.objects {
        all.push((format!("objects/{}/{}", triple, leaf), bytes.clone()));
    }
    all.push(("index.toon".to_string(), layout.index_toon.clone()));
    all.push(("hashes.toon".to_string(), layout.hashes_toon.clone()));
    all.push(("signature.bin".to_string(), layout.signature_bin.clone()));
    all.push(("publisher.key".to_string(), layout.publisher_key.clone()));
    all
}

// ---------------------------------------------------------------------------
// Test 1a: rune_hash reproducibility (same bytes → identical hash twice)
// ---------------------------------------------------------------------------

#[test]
fn rune_hash_is_reproducible() {
    let archive_bytes = b"deterministic archive bytes for reproducibility test";
    let h1 = compute_rune_hash(archive_bytes);
    let h2 = compute_rune_hash(archive_bytes);
    assert_eq!(h1, h2, "rune_hash must be bit-identical across two calls with the same input");
    assert!(h1.starts_with("blake3:"), "rune_hash must carry blake3: prefix");
    assert_eq!(h1.len(), 71, "blake3: prefix (7) + 64 hex chars = 71");
}

// ---------------------------------------------------------------------------
// Test 1b: surface_hash reproducibility (same layout → identical hash twice)
// ---------------------------------------------------------------------------

#[test]
fn surface_hash_is_reproducible() {
    let layout = make_layout();
    let h1 = compute_surface_hash(&layout.surface).expect("first call should succeed");
    let h2 = compute_surface_hash(&layout.surface).expect("second call should succeed");
    assert_eq!(h1, h2, "surface_hash must be bit-identical across two calls with the same input");
}

// ---------------------------------------------------------------------------
// Test 1c: effect_hash reproducibility
// ---------------------------------------------------------------------------

#[test]
fn effect_hash_is_reproducible() {
    let layout = make_layout();
    let h1 = compute_effect_hash(&layout.surface).expect("first call should succeed");
    let h2 = compute_effect_hash(&layout.surface).expect("second call should succeed");
    assert_eq!(h1, h2, "effect_hash must be bit-identical across two calls with the same input");
}

// ---------------------------------------------------------------------------
// Test 1d: compute_archive_hashes reproducibility (all three hashes at once)
// ---------------------------------------------------------------------------

#[test]
fn compute_archive_hashes_is_reproducible() {
    let layout = make_layout();
    let packed = edda_mimir_archive::pack(&layout).expect("pack should succeed");
    let all_files = layout_all_files(&layout);

    let h1 = compute_archive_hashes(&packed, &layout.surface, &all_files)
        .expect("first call should succeed");
    let h2 = compute_archive_hashes(&packed, &layout.surface, &all_files)
        .expect("second call should succeed");

    assert_eq!(h1.rune_hash,    h2.rune_hash,    "rune_hash must be reproducible via compute_archive_hashes");
    assert_eq!(h1.surface_hash, h2.surface_hash,  "surface_hash must be reproducible via compute_archive_hashes");
    assert_eq!(h1.effect_hash,  h2.effect_hash,   "effect_hash must be reproducible via compute_archive_hashes");
    assert_eq!(h1.files,        h2.files,          "per-file hash table must be reproducible");
}

// ---------------------------------------------------------------------------
// Test 1e: surface_hash is invariant to surface-file input ordering
// ---------------------------------------------------------------------------

#[test]
fn surface_hash_invariant_to_input_ordering() {
    let file_alpha = (
        "alpha".to_string(),
        b"schema_version: 3\nmodule: alpha\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  fn_a,() -> () with {},,,Doc.,\n".to_vec(),
    );
    let file_beta = (
        "beta".to_string(),
        b"schema_version: 3\nmodule: beta\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  fn_b,() -> () with {},,,Doc.,\n".to_vec(),
    );
    let file_gamma = (
        "gamma".to_string(),
        b"schema_version: 3\nmodule: gamma\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  fn_c,() -> () with {},,,Doc.,\n".to_vec(),
    );

    // Three different orderings of the same three files.
    let order_abc = vec![file_alpha.clone(), file_beta.clone(), file_gamma.clone()];
    let order_cab = vec![file_gamma.clone(), file_alpha.clone(), file_beta.clone()];
    let order_bca = vec![file_beta.clone(), file_gamma.clone(), file_alpha.clone()];

    let h_abc = compute_surface_hash(&order_abc).expect("abc order should succeed");
    let h_cab = compute_surface_hash(&order_cab).expect("cab order should succeed");
    let h_bca = compute_surface_hash(&order_bca).expect("bca order should succeed");

    assert_eq!(h_abc, h_cab, "surface_hash must be invariant to input ordering (abc vs cab)");
    assert_eq!(h_abc, h_bca, "surface_hash must be invariant to input ordering (abc vs bca)");
}

// ---------------------------------------------------------------------------
// Test 1f: effect_hash is invariant to surface-file input ordering
// ---------------------------------------------------------------------------

#[test]
fn effect_hash_invariant_to_input_ordering() {
    let file_alpha = (
        "alpha".to_string(),
        b"schema_version: 3\nmodule: alpha\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  fn_a,() -> () with {panic},,,Doc.,\n".to_vec(),
    );
    let file_beta = (
        "beta".to_string(),
        b"schema_version: 3\nmodule: beta\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  fn_b,() -> () with {Filesystem},,,Doc.,\n".to_vec(),
    );

    let order_ab = vec![file_alpha.clone(), file_beta.clone()];
    let order_ba = vec![file_beta.clone(), file_alpha.clone()];

    let h_ab = compute_effect_hash(&order_ab).expect("ab order should succeed");
    let h_ba = compute_effect_hash(&order_ba).expect("ba order should succeed");

    assert_eq!(h_ab, h_ba, "effect_hash must be invariant to input ordering");
}

// ---------------------------------------------------------------------------
// Test 1g: Three-hash independence — a pure patch changes only rune_hash
// ---------------------------------------------------------------------------

#[test]
fn hash_independence_patch_changes_only_rune_hash() {
    // Same surface files for both "versions".
    let surface = vec![(
        "core".to_string(),
        b"schema_version: 3\nmodule: core\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  init,() -> () with {},,,Init.,\n".to_vec(),
    )];

    // Two different archive byte-strings representing v1 and a patch v2.
    let archive_v1 = b"archive-v1-bytes".as_slice();
    let archive_v2 = b"archive-v2-patch-bytes".as_slice();

    let rune_v1 = compute_rune_hash(archive_v1);
    let rune_v2 = compute_rune_hash(archive_v2);
    let surface_v1 = compute_surface_hash(&surface).unwrap();
    let surface_v2 = compute_surface_hash(&surface).unwrap();
    let effect_v1 = compute_effect_hash(&surface).unwrap();
    let effect_v2 = compute_effect_hash(&surface).unwrap();

    // A patch changes only the archive bytes; the surface and effect stay identical.
    assert_ne!(rune_v1, rune_v2, "rune_hash must differ between patch versions");
    assert_eq!(surface_v1, surface_v2, "surface_hash must be unchanged by a pure patch");
    assert_eq!(effect_v1, effect_v2, "effect_hash must be unchanged by a pure patch");
}
