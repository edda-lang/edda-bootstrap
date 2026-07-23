//! ed25519 publisher-key signing, verification, and fingerprinting.
//!
//! Implements the cryptographic surface required by the Mímir package-
//! management layer (codex `08-packages.md` §3.3 trust chain, §6.5
//! publisher-key fingerprint, §8.4 `edda publish` signing).
//!
//! # Design constraints
//! - Routes BLAKE3 through `edda_cache::hash_bytes` — the workspace's single
//!   BLAKE3 binding.  No direct `blake3` dependency.
//! - Sync-only API; no async.  Matches the edda-daemon concurrency model
//!   (sync core + parking_lot + crossbeam channels).
//! - Does NOT create the keystore directory; that is `edda key generate`'s job
//!   (slice F of the CLI surface).

use std::path::PathBuf;

use ed25519_dalek::{Signer as _, Verifier as _};
use rand::rngs::OsRng;

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// An ed25519 keypair (signing key + verifying key).
pub struct Keypair {
    /// Inner dalek signing key (contains both private scalar and public point).
    inner: ed25519_dalek::SigningKey,
}

/// An ed25519 public key used to verify publisher signatures.
pub struct PublicKey(pub ed25519_dalek::VerifyingKey);

/// An ed25519 signature over an arbitrary byte sequence.
pub struct Signature(pub ed25519_dalek::Signature);

/// Canonical text fingerprint for a publisher public key.
///
/// Format: `"ed25519:<hex>"` where `<hex>` is the lowercase hex encoding of
/// the first 12 bytes of the BLAKE3 hash of the public key's 32-byte
/// representation.
pub struct Fingerprint(pub String);

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Verification failure modes.
#[derive(Debug, PartialEq, Eq)]
pub enum VerifyError {
    /// ed25519 verification failed — signature does not match.
    BadSignature,
    /// The supplied public key bytes did not parse into a valid ed25519 key.
    MalformedKey,
    /// The supplied signature bytes did not parse into a valid ed25519 signature.
    MalformedSignature,
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifyError::BadSignature => f.write_str("ed25519 signature verification failed"),
            VerifyError::MalformedKey => f.write_str("malformed ed25519 public key"),
            VerifyError::MalformedSignature => f.write_str("malformed ed25519 signature bytes"),
        }
    }
}

impl std::error::Error for VerifyError {}

/// Parse / deserialisation failure modes.
#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// PEM header or footer was not the expected ed25519 public key form.
    BadPemHeader,
    /// Base64 body inside the PEM block did not decode correctly.
    BadBase64,
    /// Decoded bytes had the wrong length or did not form a valid curve point.
    BadKeyBytes,
    /// Signature byte slice had the wrong length (must be 64).
    BadSignatureLength,
    /// Signature bytes did not form a valid ed25519 signature.
    BadSignatureBytes,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::BadPemHeader => f.write_str("expected PUBLIC KEY PEM header/footer"),
            ParseError::BadBase64 => f.write_str("PEM body is not valid base64"),
            ParseError::BadKeyBytes => f.write_str("decoded bytes are not a valid ed25519 public key"),
            ParseError::BadSignatureLength => f.write_str("signature must be exactly 64 bytes"),
            ParseError::BadSignatureBytes => f.write_str("bytes are not a valid ed25519 signature"),
        }
    }
}

impl std::error::Error for ParseError {}

// ---------------------------------------------------------------------------
// Functions
// ---------------------------------------------------------------------------

/// Generate a new random ed25519 keypair using the OS random source.
pub fn generate() -> Keypair {
    let signing_key = ed25519_dalek::SigningKey::generate(&mut OsRng);
    Keypair { inner: signing_key }
}

/// Sign `bytes` with `privkey` and return the resulting signature.
pub fn sign(privkey: &Keypair, bytes: &[u8]) -> Signature {
    let sig = privkey.inner.sign(bytes);
    Signature(sig)
}

/// Verify that `sig` is a valid signature of `bytes` under `pubkey`.
///
/// Returns `Ok(())` on success; an appropriate `VerifyError` variant on
/// failure.  Never panics.
pub fn verify(pubkey: &PublicKey, bytes: &[u8], sig: &Signature) -> Result<(), VerifyError> {
    pubkey.0.verify(bytes, &sig.0).map_err(|_| VerifyError::BadSignature)
}

/// Compute the canonical text fingerprint for `pubkey`.
///
/// Fingerprint format: `"ed25519:<hex>"` where `<hex>` is the first 12 bytes
/// of the BLAKE3 hash of the 32-byte public key representation, encoded as
/// 24 lowercase hex characters.
///
/// Routes hashing through `edda_cache::hash_bytes` per the workspace
/// single-BLAKE3-binding rule.
pub fn fingerprint(pubkey: &PublicKey) -> Fingerprint {
    let key_bytes = pubkey.0.to_bytes();
    let hash = edda_cache::hash_bytes(&key_bytes);
    // Take the first 12 bytes of the 32-byte BLAKE3 output — matches the
    // HashPrefix 12-byte truncation pattern used by edda-cache.
    let prefix_hex = bytes_to_hex(&hash.as_bytes()[..12]);
    Fingerprint(format!("ed25519:{}", prefix_hex))
}

/// Return the default directory where keypairs are stored on this machine.
///
/// Typically `~/.config/edda/keys/` on POSIX systems and
/// `%APPDATA%\edda\keys\` on Windows.  Returns `None` on unusual OS
/// configurations where the platform config directory cannot be determined.
///
/// This function does NOT create the directory; that responsibility belongs
/// to the `edda key generate` CLI command (slice F).
pub fn default_keystore_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|mut p| {
        p.push("edda");
        p.push("keys");
        p
    })
}

// ---------------------------------------------------------------------------
// Serialisation helpers
// ---------------------------------------------------------------------------

const PEM_HEADER: &str = "-----BEGIN PUBLIC KEY-----";
const PEM_FOOTER: &str = "-----END PUBLIC KEY-----";

const PRIV_PEM_HEADER: &str = "-----BEGIN PRIVATE KEY-----";
const PRIV_PEM_FOOTER: &str = "-----END PRIVATE KEY-----";

impl PublicKey {
    /// Serialise this public key to a PEM-encoded string.
    ///
    /// The PEM type header is `PUBLIC KEY`; the payload is the raw 32-byte
    /// ed25519 public key encoded as base64 (standard alphabet, line-wrapped
    /// at 64 characters).
    pub fn to_pem(&self) -> String {
        let b64 = base64_encode(self.0.as_bytes());
        let wrapped = wrap_base64_lines(&b64, 64);
        format!("{}\n{}{}\n", PEM_HEADER, wrapped, PEM_FOOTER)
    }

    /// Parse a PEM-encoded ed25519 public key.
    ///
    /// Expects the exact `PUBLIC KEY` PEM type label produced by
    /// [`PublicKey::to_pem`].  Returns a `ParseError` for any structural or
    /// cryptographic issue.
    pub fn from_pem(s: &str) -> Result<Self, ParseError> {
        let trimmed = s.trim();
        let inner = trimmed
            .strip_prefix(PEM_HEADER)
            .ok_or(ParseError::BadPemHeader)?
            .trim_start_matches('\n')
            .strip_suffix(PEM_FOOTER)
            .ok_or(ParseError::BadPemHeader)?
            .trim();
        let b64: String = inner.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        let raw = base64_decode(&b64).map_err(|_| ParseError::BadBase64)?;
        let bytes: [u8; 32] = raw.try_into().map_err(|_| ParseError::BadKeyBytes)?;
        let vk = ed25519_dalek::VerifyingKey::from_bytes(&bytes)
            .map_err(|_| ParseError::BadKeyBytes)?;
        Ok(PublicKey(vk))
    }
}

impl Signature {
    /// Return the raw 64-byte representation of this signature.
    pub fn to_bytes(&self) -> [u8; 64] {
        self.0.to_bytes()
    }

    /// Parse a signature from its 64-byte representation.
    pub fn from_bytes(b: &[u8]) -> Result<Self, ParseError> {
        let arr: [u8; 64] = b.try_into().map_err(|_| ParseError::BadSignatureLength)?;
        let sig = ed25519_dalek::Signature::from_bytes(&arr);
        Ok(Signature(sig))
    }
}

impl Keypair {
    /// Return the public half of this keypair.
    pub fn public_key(&self) -> PublicKey {
        PublicKey(self.inner.verifying_key())
    }

    /// Serialise this keypair's private signing key to a PEM-encoded string.
    ///
    /// The PEM type header is `PRIVATE KEY`; the payload is the raw 32-byte
    /// ed25519 signing-key scalar encoded as base64 (standard alphabet,
    /// line-wrapped at 64 characters). This is the form `edda key generate`
    /// writes to disk as `priv.pem`.
    pub fn to_pem(&self) -> String {
        let raw = self.inner.to_bytes();
        let b64 = base64_encode(&raw);
        let wrapped = wrap_base64_lines(&b64, 64);
        format!("{}\n{}{}\n", PRIV_PEM_HEADER, wrapped, PRIV_PEM_FOOTER)
    }

    /// Parse a PEM-encoded ed25519 signing key (the form produced by
    /// [`Keypair::to_pem`]). Reconstructs the keypair from the 32-byte
    /// signing-key scalar; the public verifying key is derived deterministically.
    pub fn from_pem(s: &str) -> Result<Self, ParseError> {
        let trimmed = s.trim();
        let inner = trimmed
            .strip_prefix(PRIV_PEM_HEADER)
            .ok_or(ParseError::BadPemHeader)?
            .trim_start_matches('\n')
            .strip_suffix(PRIV_PEM_FOOTER)
            .ok_or(ParseError::BadPemHeader)?
            .trim();
        let b64: String = inner.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        let raw = base64_decode(&b64).map_err(|_| ParseError::BadBase64)?;
        let bytes: [u8; 32] = raw.try_into().map_err(|_| ParseError::BadKeyBytes)?;
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&bytes);
        Ok(Keypair { inner: signing_key })
    }
}

// ---------------------------------------------------------------------------
// Base64 helpers (no external dep — ed25519 keys are short)
// ---------------------------------------------------------------------------

/// Encode `bytes` as standard base64 (RFC 4648, with padding).
fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let combined = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((combined >> 18) & 0x3f) as usize]);
        out.push(TABLE[((combined >> 12) & 0x3f) as usize]);
        out.push(if chunk.len() > 1 { TABLE[((combined >> 6) & 0x3f) as usize] } else { b'=' });
        out.push(if chunk.len() > 2 { TABLE[(combined & 0x3f) as usize] } else { b'=' });
    }
    // SAFETY: all bytes are ASCII from TABLE.
    unsafe { String::from_utf8_unchecked(out) }
}

/// Decode standard base64 (RFC 4648, with padding). Returns `None` on invalid input.
fn base64_decode(s: &str) -> Result<Vec<u8>, ()> {
    fn val(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            b'=' => Some(0), // padding
            _ => None,
        }
    }
    let b = s.as_bytes();
    if b.len() % 4 != 0 {
        return Err(());
    }
    let mut out = Vec::with_capacity(b.len() / 4 * 3);
    let mut i = 0;
    while i < b.len() {
        let v0 = val(b[i]).ok_or(())?;
        let v1 = val(b[i + 1]).ok_or(())?;
        let v2 = val(b[i + 2]).ok_or(())?;
        let v3 = val(b[i + 3]).ok_or(())?;
        out.push((v0 << 2) | (v1 >> 4));
        if b[i + 2] != b'=' {
            out.push((v1 << 4) | (v2 >> 2));
        }
        if b[i + 3] != b'=' {
            out.push((v2 << 6) | v3);
        }
        i += 4;
    }
    Ok(out)
}

/// Wrap a flat base64 string into lines of at most `width` characters.
fn wrap_base64_lines(s: &str, width: usize) -> String {
    let mut out = String::with_capacity(s.len() + s.len() / width + 1);
    let mut remaining = s;
    while remaining.len() > width {
        out.push_str(&remaining[..width]);
        out.push('\n');
        remaining = &remaining[width..];
    }
    if !remaining.is_empty() {
        out.push_str(remaining);
        out.push('\n');
    }
    out
}

/// Encode `bytes` as lowercase hex.
fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = vec![0u8; bytes.len() * 2];
    for (i, &b) in bytes.iter().enumerate() {
        out[i * 2] = HEX[(b >> 4) as usize];
        out[i * 2 + 1] = HEX[(b & 0x0f) as usize];
    }
    // SAFETY: all bytes are ASCII hex digits.
    unsafe { String::from_utf8_unchecked(out) }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_then_sign_then_verify_round_trip() {
        let kp = generate();
        let msg = b"hello, Mimir";
        let sig = sign(&kp, msg);
        let pk = kp.public_key();
        assert!(verify(&pk, msg, &sig).is_ok());
    }

    #[test]
    fn verify_rejects_tampered_bytes() {
        let kp = generate();
        let msg = b"original message";
        let sig = sign(&kp, msg);
        let pk = kp.public_key();
        let mut tampered = msg.to_vec();
        tampered[0] ^= 0x01;
        assert_eq!(verify(&pk, &tampered, &sig), Err(VerifyError::BadSignature));
    }

    #[test]
    fn verify_rejects_wrong_key() {
        let kp_a = generate();
        let kp_b = generate();
        let msg = b"signed by A";
        let sig = sign(&kp_a, msg);
        let pk_b = kp_b.public_key();
        assert_eq!(verify(&pk_b, msg, &sig), Err(VerifyError::BadSignature));
    }

    #[test]
    fn fingerprint_is_deterministic() {
        let kp = generate();
        let pk = kp.public_key();
        let f1 = fingerprint(&pk);
        let f2 = fingerprint(&pk);
        assert_eq!(f1.0, f2.0);
    }

    #[test]
    fn fingerprint_has_correct_format() {
        let kp = generate();
        let pk = kp.public_key();
        let f = fingerprint(&pk);
        // Must start with "ed25519:"
        assert!(f.0.starts_with("ed25519:"), "fingerprint: {}", f.0);
        let hex_part = &f.0["ed25519:".len()..];
        // 12 bytes = 24 hex chars
        assert_eq!(hex_part.len(), 24, "fingerprint hex part len: {}", hex_part.len());
        assert!(hex_part.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "fingerprint hex part not lowercase: {}", hex_part);
    }

    #[test]
    fn pem_round_trip() {
        let kp = generate();
        let pk = kp.public_key();
        let pem = pk.to_pem();
        let pk2 = PublicKey::from_pem(&pem).expect("from_pem failed");
        assert_eq!(pk.0.as_bytes(), pk2.0.as_bytes());
    }

    #[test]
    fn signature_bytes_round_trip() {
        let kp = generate();
        let msg = b"round-trip test";
        let sig = sign(&kp, msg);
        let bytes = sig.to_bytes();
        let sig2 = Signature::from_bytes(&bytes).expect("from_bytes failed");
        let pk = kp.public_key();
        assert!(verify(&pk, msg, &sig2).is_ok());
    }

    #[test]
    fn different_keys_produce_different_fingerprints() {
        let kp_a = generate();
        let kp_b = generate();
        let fa = fingerprint(&kp_a.public_key());
        let fb = fingerprint(&kp_b.public_key());
        // Overwhelmingly likely with random keys; if this fails, something is wrong.
        assert_ne!(fa.0, fb.0);
    }

    #[test]
    fn keypair_pem_round_trip() {
        let kp = generate();
        let pem = kp.to_pem();
        let kp2 = Keypair::from_pem(&pem).expect("Keypair::from_pem failed");
        // Sign the same payload with both copies — recovered key must produce
        // an identical signature (signing is deterministic per RFC 8032).
        let msg = b"keypair pem round-trip";
        let sig_a = sign(&kp, msg);
        let sig_b = sign(&kp2, msg);
        assert_eq!(sig_a.to_bytes(), sig_b.to_bytes());
        // Public-key derivation is deterministic from the private scalar.
        assert_eq!(kp.public_key().0.as_bytes(), kp2.public_key().0.as_bytes());
    }
}
