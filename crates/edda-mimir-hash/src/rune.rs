//! rune_hash walker — §4.1.
//!
//! The simplest of the three Mímir hashes: BLAKE3 of the raw `.tar.zst`
//! archive bytes. Routes through [`edda_cache::hash_bytes`] per the
//! workspace single-BLAKE3-binding rule.

/// Compute `rune_hash` for the given `.tar.zst` archive bytes.
///
/// Per §4.1: BLAKE3 of the `.tar.zst` archive bytes. The simplest of the
/// three Mímir hashes — just wraps [`edda_cache::hash_bytes`] behind a
/// clearer name and formats the result as `"blake3:<hex>"`.
pub fn compute_rune_hash(archive_bytes: &[u8]) -> String {
    let hash = edda_cache::hash_bytes(archive_bytes);
    format!("blake3:{hash}")
}
