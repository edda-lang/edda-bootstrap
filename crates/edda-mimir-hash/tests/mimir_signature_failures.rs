//! Integration: signature verification failure modes (§3.3 + slice E).
//!
//! Verifies three distinct `verify` rejection paths from slice E
//! (`edda-mimir-crypto`) per `08-packages.md` §3.3:
//!
//! 1. **Wrong key** — sign with key A, verify with key B's pubkey.
//!    `verify` must return `VerifyError::BadSignature`.
//!
//! 2. **Tampered bytes** — sign canonical `hashes.toon` bytes with key A,
//!    then verify against modified bytes with key A's pubkey.
//!    `verify` must return `VerifyError::BadSignature`.
//!
//! 3. **Malformed key** — call `PublicKey::from_pem` with bytes that are not
//!    a valid PEM block; must return `ParseError::BadPemHeader` (or another
//!    parse variant), confirming the API surface rejects garbage input.
//!
//! Each failure mode is a separate `#[test]` to give independent attribution
//! in the test report.
//!
//! Corresponds to slice I, Test 4.

use edda_mimir_crypto::{generate, sign, verify, ParseError, PublicKey, VerifyError};

// ---------------------------------------------------------------------------
// Canonical `hashes.toon` content used as the message to sign.
// In production this would be the actual bytes from the .rune archive's
// hashes.toon file after the rollup hashes have been filled in.
// ---------------------------------------------------------------------------

const SAMPLE_HASHES_TOON: &[u8] = b"rune_hash = \"blake3:aabb\"\nsurface_hash = \"blake3:ccdd\"\neffect_hash = \"blake3:eeff\"\n";

// ---------------------------------------------------------------------------
// Test 4a: Wrong key → BadSignature
// ---------------------------------------------------------------------------

#[test]
fn verify_wrong_key_returns_bad_signature() {
    // Key A signs the message.
    let kp_a = generate();
    let sig = sign(&kp_a, SAMPLE_HASHES_TOON);

    // Key B's pubkey is used to verify — must fail.
    let kp_b = generate();
    let pk_b = kp_b.public_key();

    let result = verify(&pk_b, SAMPLE_HASHES_TOON, &sig);

    assert_eq!(
        result,
        Err(VerifyError::BadSignature),
        "verifying with wrong key must return BadSignature; got {:?}",
        result
    );
}

// ---------------------------------------------------------------------------
// Test 4b: Tampered bytes → BadSignature
// ---------------------------------------------------------------------------

#[test]
fn verify_tampered_bytes_returns_bad_signature() {
    // Sign the original message with key A.
    let kp_a = generate();
    let pk_a = kp_a.public_key();
    let sig = sign(&kp_a, SAMPLE_HASHES_TOON);

    // Flip one byte in the message to simulate a tampered hashes.toon.
    let mut tampered = SAMPLE_HASHES_TOON.to_vec();
    tampered[0] ^= 0x01;

    let result = verify(&pk_a, &tampered, &sig);

    assert_eq!(
        result,
        Err(VerifyError::BadSignature),
        "verifying tampered bytes must return BadSignature; got {:?}",
        result
    );
}

// ---------------------------------------------------------------------------
// Test 4c: Malformed PEM key → ParseError
//
// We test the `PublicKey::from_pem` entry point because it is the path that
// callers take when loading a publisher key from a `.key` file or from the
// `publisher.key` entry in a `.rune` archive. Garbage input must produce a
// parse error rather than panic.
// ---------------------------------------------------------------------------

#[test]
fn malformed_pem_key_returns_parse_error() {
    let garbage_inputs: &[&str] = &[
        "",                      // completely empty
        "not a pem at all",      // no PEM structure
        "-----BEGIN RSA PRIVATE KEY-----\nYWJj\n-----END RSA PRIVATE KEY-----\n", // wrong label
        "-----BEGIN PUBLIC KEY-----\n!!!notbase64!!!\n-----END PUBLIC KEY-----\n", // bad base64
        "-----BEGIN PUBLIC KEY-----\naGVsbG8=\n-----END PUBLIC KEY-----\n", // valid base64 but wrong length (5 bytes, not 32)
    ];

    for input in garbage_inputs {
        let result = PublicKey::from_pem(input);
        assert!(
            result.is_err(),
            "from_pem must return Err for malformed input {:?}; got Ok",
            input
        );
    }
}

// ---------------------------------------------------------------------------
// Test 4d: Malformed PEM → specific ParseError variants
// ---------------------------------------------------------------------------

#[test]
fn malformed_pem_header_returns_bad_pem_header() {
    // Missing PEM header entirely.
    let result = PublicKey::from_pem("just garbage text");
    assert!(
        matches!(result, Err(ParseError::BadPemHeader)),
        "missing PEM header must produce BadPemHeader"
    );
}

#[test]
fn malformed_pem_bad_base64_returns_bad_base64() {
    // Valid PEM structure but invalid base64 inside.
    let input = "-----BEGIN PUBLIC KEY-----\n!!!!invalid!!!!\n-----END PUBLIC KEY-----\n";
    let result = PublicKey::from_pem(input);
    assert!(
        matches!(result, Err(ParseError::BadBase64)),
        "invalid base64 in PEM must produce BadBase64"
    );
}

#[test]
fn malformed_pem_wrong_length_returns_bad_key_bytes() {
    // Valid PEM structure and valid base64, but not 32 bytes (ed25519 key size).
    // "hello" in base64 is "aGVsbG8=" (5 bytes decoded).
    let input = "-----BEGIN PUBLIC KEY-----\naGVsbG8=\n-----END PUBLIC KEY-----\n";
    let result = PublicKey::from_pem(input);
    assert!(
        matches!(result, Err(ParseError::BadKeyBytes)),
        "32-byte length mismatch must produce BadKeyBytes"
    );
}

// ---------------------------------------------------------------------------
// Test 4e: Round-trip positive control (verify succeeds with correct key)
// ---------------------------------------------------------------------------

#[test]
fn verify_succeeds_with_correct_key_and_original_bytes() {
    let kp = generate();
    let pk = kp.public_key();
    let sig = sign(&kp, SAMPLE_HASHES_TOON);

    let result = verify(&pk, SAMPLE_HASHES_TOON, &sig);
    assert!(
        result.is_ok(),
        "verify must succeed with the correct key and original bytes; got {:?}",
        result
    );
}

// ---------------------------------------------------------------------------
// Test 4f: PEM round-trip — to_pem followed by from_pem recovers the key
// ---------------------------------------------------------------------------

#[test]
fn pem_round_trip_recovers_public_key_for_verification() {
    let kp = generate();
    let pk_original = kp.public_key();

    // Serialise and deserialise the public key.
    let pem = pk_original.to_pem();
    let pk_recovered = PublicKey::from_pem(&pem)
        .expect("valid PEM produced by to_pem must round-trip through from_pem");

    // Sign a message with the original keypair; verify with the recovered pubkey.
    let msg = b"round-trip verification test for the Mimir trust chain";
    let sig = sign(&kp, msg);
    let result = verify(&pk_recovered, msg, &sig);
    assert!(
        result.is_ok(),
        "verification with a PEM-round-tripped public key must succeed; got {:?}",
        result
    );
}
