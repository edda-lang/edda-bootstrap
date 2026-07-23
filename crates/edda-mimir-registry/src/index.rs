//! Sparse-index entry type returned by `fetch_index`.

use serde::{Deserialize, Serialize};

/// One entry per available version, as returned by the Mímir sparse index.
///
/// The index JSON uses field names that match these struct fields exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    /// Package name.
    pub name: String,
    /// Semver version string, e.g. `"1.2.3"`.
    pub version: String,
    /// BLAKE3 hash of the `.rune` archive bytes, `"blake3:<hex>"`.
    pub rune_hash: String,
    /// BLAKE3 hash of the surface interface, `"blake3:<hex>"`.
    pub surface_hash: String,
    /// BLAKE3 hash of the effect manifest, `"blake3:<hex>"`.
    pub effect_hash: String,
    /// Ed25519 publisher public key, `"ed25519:<pem-or-hex>"`.
    pub publisher_key: String,
    /// Ed25519 signature over the index entry, `"ed25519:<hex>"`.
    pub publisher_sig: String,
}
