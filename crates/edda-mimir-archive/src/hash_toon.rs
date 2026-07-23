//! Minimal `hashes.toon` reader and writer for the `.rune` archive format.
//!
//! The schema is fixed per the slice-D spec:
//!
//! ```toon
//! # hashes.toon — per-file BLAKE3 + the three rollup hashes
//! schema_version: 1
//!
//! rune_hash: blake3:<hex>
//! surface_hash: blake3:<hex>
//! effect_hash: blake3:<hex>
//!
//! files[N]{path,hash}:
//!   manifest.toml,blake3:<hex>
//!   surface/<module>.toon,blake3:<hex>
//!   ...
//! ```
//!
//! Rollup hashes (`rune_hash`, `surface_hash`, `effect_hash`) are
//! placeholders during pack — they are zeros in hex. Slice F will compute
//! them; this crate only records and verifies per-file BLAKE3s.

use std::collections::HashMap;

use edda_cache::hash_bytes;

// ---------------------------------------------------------------------------
// Writer
// ---------------------------------------------------------------------------

/// Build the canonical `hashes.toon` bytes for a layout being packed.
///
/// Per-file BLAKE3s are computed via [`edda_cache::hash_bytes`] (workspace
/// single-BLAKE3-binding rule). Rollup hashes (`rune_hash`, `surface_hash`,
/// `effect_hash`) are emitted as 64-zero-hex placeholders; slice F fills
/// the real values when it walks the archive.
pub fn build_hashes_toon(
    manifest_toml: &[u8],
    surface: &[(String, Vec<u8>)],
    mir: &[(String, Vec<u8>)],
    objects: &[(String, String, Vec<u8>)],
    index_toon: &[u8],
) -> Vec<u8> {
    let mut entries: Vec<(String, String)> = Vec::new();

    entries.push((
        "manifest.toml".to_string(),
        format!("blake3:{}", hash_hex(manifest_toml)),
    ));

    for (leaf, bytes) in surface {
        entries.push((
            format!("surface/{leaf}.toon"),
            format!("blake3:{}", hash_hex(bytes)),
        ));
    }

    for (leaf, bytes) in mir {
        entries.push((
            format!("mir/{leaf}.mir"),
            format!("blake3:{}", hash_hex(bytes)),
        ));
    }

    for (triple, leaf, bytes) in objects {
        entries.push((
            format!("objects/{triple}/{leaf}"),
            format!("blake3:{}", hash_hex(bytes)),
        ));
    }

    entries.push((
        "index.toon".to_string(),
        format!("blake3:{}", hash_hex(index_toon)),
    ));

    let zero_hash = "blake3:".to_string() + &"0".repeat(64);
    let count = entries.len();
    let mut out = String::new();
    out.push_str("# hashes.toon — per-file BLAKE3 + the three rollup hashes\n");
    out.push_str("schema_version: 1\n");
    out.push('\n');
    out.push_str(&format!("rune_hash: {zero_hash}\n"));
    out.push_str(&format!("surface_hash: {zero_hash}\n"));
    out.push_str(&format!("effect_hash: {zero_hash}\n"));
    out.push('\n');
    out.push_str(&format!("files[{count}]{{path,hash}}:\n"));
    for (path, hash) in &entries {
        out.push_str(&format!("  {path},{hash}\n"));
    }

    out.into_bytes()
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Parse `hashes.toon` bytes into a `path → hash-string` map.
///
/// Only the `files[...]` table is read; rollup hash scalars and comments are
/// ignored. Returns `Err(message)` on any structural problem.
pub fn parse_hashes_toon(bytes: &[u8]) -> Result<HashMap<String, String>, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!("not valid UTF-8: {e}"))?;
    let mut map: HashMap<String, String> = HashMap::new();
    let mut in_files_table = false;

    for line in text.lines() {
        let trimmed = line.trim_start();

        // Comment or blank — skip.
        if trimmed.starts_with('#') || trimmed.is_empty() {
            in_files_table = false;
            // Check whether we're falling out of the files table.
            // Blank lines between table entries would end the block, but
            // our writer never emits them there.
            continue;
        }

        // Detect the files[N]{path,hash}: header.
        if trimmed.starts_with("files[") && trimmed.contains("]{path,hash}:") {
            in_files_table = true;
            continue;
        }

        // Other scalar key: value lines — reset table mode.
        if !line.starts_with(' ') && !in_files_table {
            continue;
        }
        if !line.starts_with(' ') && in_files_table {
            // Non-indented line after the table header ends the table.
            in_files_table = false;
            continue;
        }

        if in_files_table {
            // Each files[] row: `  path,blake3:<hex>`
            let content = trimmed;
            // Split on first comma only.
            let comma = content
                .find(',')
                .ok_or_else(|| format!("files[] row missing comma: {content:?}"))?;
            let path = content[..comma].to_string();
            let hash_val = content[comma + 1..].to_string();
            if path.is_empty() || hash_val.is_empty() {
                return Err(format!("files[] row has empty path or hash: {content:?}"));
            }
            map.insert(path, hash_val);
        }
    }

    Ok(map)
}

/// BLAKE3-hash `bytes` and return the 64-char lowercase hex string.
pub(crate) fn hash_hex(bytes: &[u8]) -> String {
    hash_bytes(bytes).to_string()
}

// ---------------------------------------------------------------------------
// Verification helper
// ---------------------------------------------------------------------------

/// Verify that `actual_bytes` match the `expected` `blake3:<hex>` value from
/// `hashes.toon`. Returns `Ok(())` on match, `Err((expected, actual))` on
/// mismatch.
pub(crate) fn verify_hash(
    actual_bytes: &[u8],
    expected: &str,
) -> Result<(), (String, String)> {
    let actual = format!("blake3:{}", hash_hex(actual_bytes));
    if actual != expected {
        return Err((expected.to_string(), actual));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_parse_round_trip() {
        let manifest = b"[package]\nname = \"foo\"\n";
        let surface = vec![("mod_a".to_string(), b"toon bytes".to_vec())];
        let mir = vec![("mod_a".to_string(), b"mir bytes".to_vec())];
        let objects = vec![(
            "x86_64-linux-gnu".to_string(),
            "mod_a.o".to_string(),
            b"obj bytes".to_vec(),
        )];
        let index = b"index content";

        let toon = build_hashes_toon(manifest, &surface, &mir, &objects, index);
        let map = parse_hashes_toon(&toon).expect("parse failed");

        assert!(map.contains_key("manifest.toml"));
        assert!(map.contains_key("surface/mod_a.toon"));
        assert!(map.contains_key("mir/mod_a.mir"));
        assert!(map.contains_key("objects/x86_64-linux-gnu/mod_a.o"));
        assert!(map.contains_key("index.toon"));
        assert_eq!(map.len(), 5);
    }

    #[test]
    fn verify_hash_ok_and_mismatch() {
        let bytes = b"hello";
        let expected = format!("blake3:{}", hash_hex(bytes));
        assert!(verify_hash(bytes, &expected).is_ok());
        let bad = format!("blake3:{}", "0".repeat(64));
        assert!(verify_hash(bytes, &bad).is_err());
    }
}
