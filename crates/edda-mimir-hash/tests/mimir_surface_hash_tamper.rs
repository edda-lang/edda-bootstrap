//! Integration: surface_hash tamper detection (§4.2 + §3.3 trust chain).
//!
//! Verifies the property from `corpus/edda-codex/language/08-packages.md` §4.2
//! and §3.3: a tampered `surface/*.toon` file causes `unpack` to return
//! `UnpackError::HashMismatch` because the per-file BLAKE3 recorded in
//! `hashes.toon` no longer matches the tampered bytes.
//!
//! Two approaches are tested:
//! 1. **Payload tamper via layout mismatch**: build an `RuneLayout` where one
//!    surface file's bytes differ from what was recorded in `hashes_toon`. Pack
//!    and call `unpack` — must return `HashMismatch`.
//! 2. **Surface-file byte flip after pack-unpack**: pack normally, then verify
//!    that a recomputed `surface_hash` over tampered surface bytes differs from
//!    the original, confirming the hash correctly detects the change.
//!
//! Corresponds to slice I, Test 3.

use edda_mimir_archive::{build_hashes_toon, pack, unpack, UnpackError, RuneLayout};
use edda_mimir_hash::compute_surface_hash;

// ---------------------------------------------------------------------------
// Shared fixture helpers
// ---------------------------------------------------------------------------

/// Build a valid `RuneLayout` whose `hashes_toon` correctly records all files.
fn make_valid_layout() -> RuneLayout {
    let manifest = b"[package]\nname = \"tamper-test\"\nversion = \"1.0.0\"\n".to_vec();
    let surface = vec![
        (
            "core".to_string(),
            b"schema_version: 3\nmodule: core\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  init,() -> () with {},,,Init the module.,\n".to_vec(),
        ),
    ];
    let mir = vec![
        ("core".to_string(), b"# core mir\n".to_vec()),
    ];
    let objects = vec![
        ("x86_64-pc-windows-msvc".to_string(), "core.o".to_string(), b"core-object-bytes".to_vec()),
    ];
    let index = b"schema_version: 3\nmodule: tamper-test\n".to_vec();
    let signature = b"sig-placeholder".to_vec();
    let publisher_key = b"ed25519:pubkey\nfingerprint: ed25519:abcd\n".to_vec();
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

// ---------------------------------------------------------------------------
// Test 3a: tampered surface bytes trigger HashMismatch on unpack
//
// Strategy: build a valid layout (so hashes_toon is correct for the original
// surface bytes), then replace the surface bytes with different content before
// packing. The hashes_toon still records the original BLAKE3; the tampered
// bytes produce a different BLAKE3 at unpack time → HashMismatch.
// ---------------------------------------------------------------------------

#[test]
fn unpack_rejects_tampered_surface_file() {
    let valid_layout = make_valid_layout();

    // Tamper: replace surface bytes AFTER hashes_toon was computed.
    let mut tampered_layout = valid_layout;
    // The original surface content for "core" starts with "schema_version: 3\n..."
    // Replace it with completely different bytes so BLAKE3 will differ.
    tampered_layout.surface[0].1 = b"TAMPERED SURFACE FILE - NOT THE ORIGINAL CONTENT\n".to_vec();

    // Pack the tampered layout (hashes_toon still has the original BLAKE3).
    let packed = pack(&tampered_layout).expect("pack should succeed even with mismatched bytes");

    // Unpack must reject — the per-file BLAKE3 in hashes_toon will not match.
    let result = unpack(&packed);
    assert!(
        result.is_err(),
        "unpack must reject a layout with tampered surface bytes"
    );

    let err = result.unwrap_err();
    assert!(
        matches!(err, UnpackError::HashMismatch { .. }),
        "expected UnpackError::HashMismatch, got {:?}",
        err
    );

    // Also verify the HashMismatch names a surface/ file path.
    if let UnpackError::HashMismatch { file, .. } = &err {
        assert!(
            file.starts_with("surface/") || file == "core",
            "HashMismatch file path should identify a surface file; got {:?}",
            file
        );
    }
}

// ---------------------------------------------------------------------------
// Test 3b: surface_hash itself detects tampered surface content
//
// Even without going through pack/unpack, the hash walker must produce a
// different `surface_hash` for the tampered bytes.
// ---------------------------------------------------------------------------

#[test]
fn surface_hash_differs_for_tampered_content() {
    let original = vec![(
        "core".to_string(),
        b"schema_version: 3\nmodule: core\n\nstable_items[1]{name,signature,effect_row,refinements,doc_high,doc_med}:\n  init,() -> () with {},,,Init.,\n".to_vec(),
    )];

    // Single-byte flip in the content.
    let mut tampered_bytes = original[0].1.clone();
    tampered_bytes[0] ^= 0x01; // flip the first byte
    let tampered = vec![("core".to_string(), tampered_bytes)];

    let h_original = compute_surface_hash(&original).expect("original should hash");
    let h_tampered = compute_surface_hash(&tampered).expect("tampered should hash");

    assert_ne!(
        h_original, h_tampered,
        "surface_hash must differ when surface file bytes are tampered"
    );
}

// ---------------------------------------------------------------------------
// Test 3c: unpack accepts a valid round-trip (positive control)
// ---------------------------------------------------------------------------

#[test]
fn unpack_accepts_valid_layout() {
    let layout = make_valid_layout();
    let packed = pack(&layout).expect("pack should succeed");
    let unpacked = unpack(&packed).expect("unpack of a valid layout must succeed");

    // Surface content round-trips correctly.
    assert_eq!(
        unpacked.surface, layout.surface,
        "surface content must survive pack/unpack round-trip"
    );
}

// ---------------------------------------------------------------------------
// Test 3d: tampered manifest bytes trigger HashMismatch on unpack
//
// Not a surface-hash test per se, but validates the general hashes.toon
// integrity check which is what protects the surface_hash trust chain.
// ---------------------------------------------------------------------------

#[test]
fn unpack_rejects_tampered_manifest() {
    let valid_layout = make_valid_layout();

    // Tamper: replace manifest bytes AFTER hashes_toon was computed.
    let mut tampered_layout = valid_layout;
    tampered_layout.manifest_toml = b"TAMPERED MANIFEST\n".to_vec();

    let packed = pack(&tampered_layout).expect("pack should succeed");
    let result = unpack(&packed);

    assert!(
        result.is_err(),
        "unpack must reject a layout with tampered manifest bytes"
    );
    assert!(
        matches!(result.unwrap_err(), UnpackError::HashMismatch { .. }),
        "expected HashMismatch on tampered manifest"
    );
}
