//! `.rune` archive unpacker with per-file BLAKE3 verification.
//!
//! Strategy: decompress the zstd stream fully into a buffer first, then read
//! the tar archive twice — once to extract `hashes.toon`, and once to verify
//! and collect all entries. Two-pass over an in-memory buffer avoids the
//! "read hashes.toon by name without streaming to completion" problem that
//! would require a seekable tar reader.

use std::collections::HashMap;
use std::io::Read;

use crate::error::UnpackError;
use crate::hash_toon::{parse_hashes_toon, verify_hash};
use crate::RuneLayout;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Unpack a `.rune` byte stream with full per-file BLAKE3 verification.
pub fn unpack(bytes: &[u8]) -> Result<RuneLayout, UnpackError> {
    // Decompress the full zstd stream into memory.
    let tar_bytes = decompress_zstd(bytes)?;

    // Pass 1: extract hashes.toon bytes.
    let hashes_toon_bytes = extract_hashes_toon(&tar_bytes)?;

    // Parse the per-file hash table.
    let hash_map = parse_hashes_toon(&hashes_toon_bytes)
        .map_err(UnpackError::MalformedHashesToon)?;

    // Pass 2: verify and collect all entries.
    collect_verified(&tar_bytes, &hash_map, hashes_toon_bytes)
}

// ---------------------------------------------------------------------------
// Zstd decompression
// ---------------------------------------------------------------------------

fn decompress_zstd(bytes: &[u8]) -> Result<Vec<u8>, UnpackError> {
    let cursor = std::io::Cursor::new(bytes);
    let mut dec = zstd::stream::read::Decoder::new(cursor)
        .map_err(|e| UnpackError::Zstd(e.to_string()))?;
    let mut out = Vec::new();
    Read::read_to_end(&mut dec, &mut out)
        .map_err(|e| UnpackError::Zstd(e.to_string()))?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// Pass 1: find and extract hashes.toon
// ---------------------------------------------------------------------------

fn extract_hashes_toon(tar_bytes: &[u8]) -> Result<Vec<u8>, UnpackError> {
    let cursor = std::io::Cursor::new(tar_bytes);
    let mut archive = tar::Archive::new(cursor);

    for entry_result in archive.entries().map_err(|e| UnpackError::Tar(e.to_string()))? {
        let mut entry = entry_result.map_err(|e| UnpackError::Tar(e.to_string()))?;
        let path = entry_path(&entry)?;
        if path == "hashes.toon" {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .map_err(|e| UnpackError::Io(e))?;
            return Ok(buf);
        }
    }

    Err(UnpackError::MissingHashesToon)
}

// ---------------------------------------------------------------------------
// Pass 2: verify and collect all entries
// ---------------------------------------------------------------------------

fn collect_verified(
    tar_bytes: &[u8],
    hash_map: &HashMap<String, String>,
    hashes_toon_bytes: Vec<u8>,
) -> Result<RuneLayout, UnpackError> {
    let mut manifest_toml: Option<Vec<u8>> = None;
    let mut surface: Vec<(String, Vec<u8>)> = Vec::new();
    let mut mir: Vec<(String, Vec<u8>)> = Vec::new();
    let mut objects: Vec<(String, String, Vec<u8>)> = Vec::new();
    let mut index_toon: Option<Vec<u8>> = None;
    let mut signature_bin: Option<Vec<u8>> = None;
    let mut publisher_key: Option<Vec<u8>> = None;

    let cursor = std::io::Cursor::new(tar_bytes);
    let mut archive = tar::Archive::new(cursor);

    for entry_result in archive.entries().map_err(|e| UnpackError::Tar(e.to_string()))? {
        let mut entry = entry_result.map_err(|e| UnpackError::Tar(e.to_string()))?;
        let path = entry_path(&entry)?;

        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .map_err(|e| UnpackError::Io(e))?;

        // Classify and dispatch.
        if path == "manifest.toml" {
            verify_entry(&path, &data, hash_map)?;
            manifest_toml = Some(data);
        } else if let Some(leaf) = strip_prefix_ext(&path, "surface/", ".toon") {
            verify_entry(&path, &data, hash_map)?;
            surface.push((leaf, data));
        } else if let Some(leaf) = strip_prefix_ext(&path, "mir/", ".mir") {
            verify_entry(&path, &data, hash_map)?;
            mir.push((leaf, data));
        } else if path.starts_with("objects/") {
            let rest = &path["objects/".len()..];
            let slash = rest
                .find('/')
                .ok_or_else(|| UnpackError::UnexpectedEntry(path.clone()))?;
            let triple = rest[..slash].to_string();
            let leaf = rest[slash + 1..].to_string();
            if triple.is_empty() || leaf.is_empty() {
                return Err(UnpackError::UnexpectedEntry(path));
            }
            verify_entry(&path, &data, hash_map)?;
            objects.push((triple, leaf, data));
        } else if path == "index.toon" {
            verify_entry(&path, &data, hash_map)?;
            index_toon = Some(data);
        } else if path == "hashes.toon" {
            // Already extracted in pass 1; skip verification (it's not in files[]).
        } else if path == "signature.bin" {
            // Not in per-file hashes; accepted verbatim.
            signature_bin = Some(data);
        } else if path == "publisher.key" {
            // Not in per-file hashes; accepted verbatim.
            publisher_key = Some(data);
        } else {
            return Err(UnpackError::UnexpectedEntry(path));
        }
    }

    // Sort surface and mir by leaf name (lex) to match the RuneLayout invariant.
    surface.sort_by(|a, b| a.0.cmp(&b.0));
    mir.sort_by(|a, b| a.0.cmp(&b.0));
    objects.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));

    // Check required entries.
    let manifest_toml = manifest_toml
        .ok_or_else(|| UnpackError::MissingEntry("manifest.toml".to_string()))?;
    let index_toon =
        index_toon.ok_or_else(|| UnpackError::MissingEntry("index.toon".to_string()))?;
    let signature_bin =
        signature_bin.ok_or_else(|| UnpackError::MissingEntry("signature.bin".to_string()))?;
    let publisher_key =
        publisher_key.ok_or_else(|| UnpackError::MissingEntry("publisher.key".to_string()))?;

    Ok(RuneLayout {
        manifest_toml,
        surface,
        mir,
        objects,
        index_toon,
        hashes_toon: hashes_toon_bytes,
        signature_bin,
        publisher_key,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn entry_path<R: Read>(entry: &tar::Entry<'_, R>) -> Result<String, UnpackError> {
    let raw = entry.path().map_err(|e| UnpackError::Tar(e.to_string()))?;
    let s = raw
        .to_str()
        .ok_or_else(|| UnpackError::Tar("non-UTF-8 tar entry path".to_string()))?;
    // Normalise: strip leading "./" if present.
    let s = s.trim_start_matches("./");
    // Normalise: forward slashes (Windows tar may use backslashes).
    Ok(s.replace('\\', "/"))
}

fn strip_prefix_ext<'a>(path: &'a str, prefix: &str, ext: &str) -> Option<String> {
    let rest = path.strip_prefix(prefix)?;
    // Reject nested slashes (no sub-directory entries in surface/ or mir/).
    if rest.contains('/') {
        return None;
    }
    let leaf = rest.strip_suffix(ext)?;
    if leaf.is_empty() {
        return None;
    }
    Some(leaf.to_string())
}

fn verify_entry(
    path: &str,
    data: &[u8],
    hash_map: &HashMap<String, String>,
) -> Result<(), UnpackError> {
    let expected = hash_map
        .get(path)
        .ok_or_else(|| UnpackError::HashMismatch {
            file: path.to_string(),
            expected: "(not in hashes.toon)".to_string(),
            actual: format!("blake3:{}", crate::hash_toon::hash_hex(data)),
        })?;

    verify_hash(data, expected).map_err(|(exp, act)| UnpackError::HashMismatch {
        file: path.to_string(),
        expected: exp,
        actual: act,
    })
}
