//! BLAKE3 content-addressed key types.
//!
//! Per `storage.md` §2: BLAKE3, 256-bit output, lowercase hex for paths.
//! Hashing of canonical spec bodies (the input that produces these values)
//! lives in `edda-codegen`; this module owns the value type, the display /
//! parse round-trip, the short-prefix projection, and a thin
//! [`hash_bytes`] helper for cases where a caller wants to hash an opaque
//! blob through the same primitive.

use std::fmt;

/// 256-bit BLAKE3 hash. The universal content-addressed key throughout the
/// cache.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ArtifactHash([u8; 32]);

impl ArtifactHash {
    /// All-zero sentinel hash. Useful as a placeholder before a real hash is
    /// computed; never compares equal to a hash produced by [`hash_bytes`]
    /// for any non-pathological input.
    pub const ZERO: ArtifactHash = ArtifactHash([0; 32]);

    /// Construct from raw bytes.
    #[inline]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        ArtifactHash(bytes)
    }

    /// Return the raw 32-byte hash value.
    #[inline]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Project to the 12-hex-character prefix used in artifact names and in
    /// cache-tier path shards.
    pub fn prefix(&self) -> HashPrefix {
        HashPrefix::from_bytes(&self.0[..6])
    }

    /// Return the cache-tier shard string: the first 4 hex characters
    /// (= first 2 bytes) of the hash, lowercase. Per `storage.md` §7.
    pub fn shard(&self) -> [u8; 4] {
        let mut out = [0u8; 4];
        write_hex(&self.0[..2], &mut out);
        out
    }

    /// Parse a lowercase-hex hash. Accepts uppercase too, but [`Display`]
    /// only produces lowercase.
    pub fn parse_hex(s: &str) -> Option<Self> {
        if s.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            let hi = hex_digit(s.as_bytes()[i * 2])?;
            let lo = hex_digit(s.as_bytes()[i * 2 + 1])?;
            *byte = (hi << 4) | lo;
        }
        Some(ArtifactHash(out))
    }
}

impl fmt::Display for ArtifactHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut buf = [0u8; 64];
        write_hex(&self.0, &mut buf);
        // SAFETY: write_hex writes only ASCII hex digits, which are valid UTF-8.
        let s = unsafe { std::str::from_utf8_unchecked(&buf) };
        f.write_str(s)
    }
}

impl fmt::Debug for ArtifactHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ArtifactHash({})", self)
    }
}

/// 12-hex-character (48-bit) prefix of an [`ArtifactHash`]. Appears as the
/// `__<prefix>` suffix in artifact filenames per `storage.md` §2.
///
/// 48 bits is sufficient for project-scale artifact counts (collision
/// probability under birthday bound at 100M artifacts ~ 2⁻²⁸).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct HashPrefix([u8; 12]);

impl HashPrefix {
    /// Construct from the first 6 bytes of an [`ArtifactHash`].
    fn from_bytes(bytes: &[u8]) -> Self {
        debug_assert_eq!(bytes.len(), 6, "HashPrefix::from_bytes: expected 6 bytes");
        let mut out = [0u8; 12];
        write_hex(bytes, &mut out);
        HashPrefix(out)
    }

    /// Return the prefix as a `&str` (lowercase hex, no allocation).
    pub fn as_str(&self) -> &str {
        // SAFETY: write_hex only emits ASCII hex digits.
        unsafe { std::str::from_utf8_unchecked(&self.0) }
    }

    /// Parse a 12-hex-character prefix.
    pub fn parse_hex(s: &str) -> Option<Self> {
        if s.len() != 12 {
            return None;
        }
        let mut out = [0u8; 12];
        for (i, b) in s.as_bytes().iter().enumerate() {
            if hex_digit(*b).is_none() {
                return None;
            }
            // Lowercase any ASCII A-F so the stored bytes are canonical.
            out[i] = b.to_ascii_lowercase();
        }
        Some(HashPrefix(out))
    }
}

impl fmt::Display for HashPrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for HashPrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HashPrefix({})", self.as_str())
    }
}

/// Body-version byte from `storage.md` §2. Increments only when the
/// canonical-form encoding changes; that bump triggers full-codegen-tier
/// rebuild.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
pub struct BodyVersion(pub u8);

impl BodyVersion {
    /// The body-version byte shipping in this bootstrap.
    ///
    /// 0x07 — appends a dll-presence byte (0/1) plus the optional
    /// length-prefixed importing-DLL string after the extern-symbol
    /// bytes in every `FnBody::Extern` encoding, admitting the
    /// `extern "sym" from "dll"` clause.
    /// Additive: `None` encodes as a single 0x00 byte. Triggers a full
    /// codegen-tier rebuild per `storage.md` §2.
    ///
    /// 0x06 — appends the `expr_kind::COMP_FIELD` (0x20) tag for
    /// `ExprKind::CompField` (`<receiver>.(<index>)`, comptime-indexed
    /// field access) per D-22 (`04-specs-comptime.md` §4.5). No existing
    /// tag reordered; the new node only ever appears inside spec bodies,
    /// so the encoding extension is additive. Triggers a full
    /// codegen-tier rebuild per `storage.md` §2.
    ///
    /// 0x05 — appends a `CastMode` tag byte after the target type on
    /// every `ExprKind::Cast` encoding, admitting the trailing
    /// `wrapping` / `saturating` / `checked` keywords per CLAUDE.md
    /// §"Numeric operators".
    ///
    /// 0x04 — adds `decreases` measure on `ExprKind::Loop`, the
    /// `RefinementKind::Decreases` tag, and the `ItemKind::Derive`
    /// top-level form.
    ///
    /// 0x08 — interpolated strings move from a `Literal::FString` literal
    /// sub-tag to the `ExprKind::FString` expr tag carrying parsed slot
    /// sub-expressions.
    ///
    /// 0x09 — appends the `expr_kind::ARRAY` (0x22) tag for
    /// `ExprKind::Array` (`[e1, ..., en]` array / slice literal, including
    /// the empty form `[]`). No existing
    /// tag reordered; the encoding extension is additive (an element
    /// sequence after the tag). Triggers a full codegen-tier rebuild per
    /// `storage.md` §2.
    ///
    /// 0x0a — appends the `pat_kind::RANGE` (0x08), `pat_kind::AT_BINDING`
    /// (0x09), and `pat_kind::SLICE` (0x0a) tags for the locked §8 range
    /// (`lo..<hi` / `lo..=hi`), `@`-binding (`name @ subpattern`), and
    /// slice (`[head, ..tail]` / `[..init, last]` / `[]`) patterns.
    /// No existing tag reordered; the
    /// extensions are additive. Triggers a full codegen-tier rebuild per
    /// `storage.md` §2.
    pub const CURRENT: BodyVersion = BodyVersion(0x0a);
}

impl fmt::Display for BodyVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:02x}", self.0)
    }
}

/// Hash an opaque byte sequence with BLAKE3.
///
/// Provided as a workspace-wide entry point so the codegen and typecheck
/// caches can hash blobs without each crate carrying its own dependency on
/// `blake3`. The canonical-form serialisation of spec bodies (the actual
/// `storage.md` §2 hash input) is `edda-codegen`'s concern; this helper is
/// for unrelated blob hashing.
pub fn hash_bytes(bytes: &[u8]) -> ArtifactHash {
    let h = blake3::hash(bytes);
    ArtifactHash::from_bytes(*h.as_bytes())
}

/// Write the lowercase-hex encoding of `bytes` into `out`. Caller must size
/// `out` to `bytes.len() * 2` exactly; debug-asserted.
fn write_hex(bytes: &[u8], out: &mut [u8]) {
    debug_assert_eq!(out.len(), bytes.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (i, &b) in bytes.iter().enumerate() {
        out[i * 2] = HEX[(b >> 4) as usize];
        out[i * 2 + 1] = HEX[(b & 0x0f) as usize];
    }
}

/// Parse a single ASCII hex digit to its 0-15 value.
fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_via_hex() {
        let h = hash_bytes(b"hello world");
        let s = h.to_string();
        assert_eq!(s.len(), 64);
        assert!(s.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
        let parsed = ArtifactHash::parse_hex(&s).expect("round-trip parse");
        assert_eq!(h, parsed);
    }

    #[test]
    fn parse_hex_rejects_wrong_length() {
        assert!(ArtifactHash::parse_hex("").is_none());
        assert!(ArtifactHash::parse_hex("abcd").is_none());
        assert!(ArtifactHash::parse_hex(&"a".repeat(63)).is_none());
        assert!(ArtifactHash::parse_hex(&"a".repeat(65)).is_none());
    }

    #[test]
    fn parse_hex_rejects_non_hex() {
        let mut bad = String::from("z");
        bad.push_str(&"0".repeat(63));
        assert!(ArtifactHash::parse_hex(&bad).is_none());
    }

    #[test]
    fn parse_hex_accepts_uppercase() {
        let h = hash_bytes(b"x");
        let lower = h.to_string();
        let upper = lower.to_uppercase();
        let from_upper = ArtifactHash::parse_hex(&upper).unwrap();
        assert_eq!(from_upper, h);
    }

    #[test]
    fn prefix_is_first_12_hex_chars() {
        let h = hash_bytes(b"abc");
        let full = h.to_string();
        let prefix = h.prefix();
        assert_eq!(prefix.as_str(), &full[..12]);
    }

    #[test]
    fn shard_is_first_4_hex_chars() {
        let h = hash_bytes(b"abc");
        let full = h.to_string();
        let shard = h.shard();
        assert_eq!(std::str::from_utf8(&shard).unwrap(), &full[..4]);
    }

    #[test]
    fn hash_bytes_matches_blake3() {
        let h = hash_bytes(b"the quick brown fox");
        let direct = blake3::hash(b"the quick brown fox");
        assert_eq!(h.as_bytes(), direct.as_bytes());
    }

    #[test]
    fn zero_hash_is_distinct_from_real_hash() {
        let h = hash_bytes(b"anything");
        assert_ne!(h, ArtifactHash::ZERO);
    }

    #[test]
    fn body_version_displays_as_hex() {
        assert_eq!(BodyVersion(0x01).to_string(), "0x01");
        assert_eq!(BodyVersion(0xff).to_string(), "0xff");
    }

    #[test]
    fn hash_prefix_round_trip() {
        let h = hash_bytes(b"hello");
        let prefix = h.prefix();
        let parsed = HashPrefix::parse_hex(prefix.as_str()).unwrap();
        assert_eq!(prefix, parsed);
    }

    #[test]
    fn hash_prefix_rejects_wrong_length() {
        assert!(HashPrefix::parse_hex("").is_none());
        assert!(HashPrefix::parse_hex("abc").is_none());
        assert!(HashPrefix::parse_hex(&"a".repeat(13)).is_none());
    }
}
