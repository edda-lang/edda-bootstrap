//! surface_hash walker — §4.2.
//!
//! BLAKE3 over the concatenation of every `surface/*.toon` file in lex order,
//! considering STABLE ITEMS ONLY.
//!
//! Per §4.2: unstable items appear in surface/ files (so consumers can opt
//! into them with `accept_unstable: true`) but do NOT participate in the
//! `surface_hash` input. This means the walker must filter each
//! `surface/*.toon` file's contents to drop unstable items before hashing.
//!
//! # Stable-item filtering
//!
//! The walker supports two TOON surface-file shapes:
//!
//! **Top-level table split** (primary shape per the spec wording):
//! ```toon
//! stable_items[N]{name,signature,...}:
//!   push,(s: mutable Vec_T) -> () with {panic},,,Push.,
//!
//! unstable_items[M]{name,signature,...}:
//!   experimental_op,...
//! ```
//! The `unstable_items[...]` table is dropped entirely.
//!
//! **Single items table with a stability column** (fallback shape):
//! ```toon
//! items[N]{name,stability,signature,...}:
//!   push,stable,(s: mutable Vec_T) -> () with {panic},,,
//!   experimental_op,unstable,...
//! ```
//! Rows where the `stability` column is not `"stable"` are dropped.
//!
//! The parser inspects real surface files from the RuneLayout to determine
//! which shape applies. If neither shape is detected, the raw file bytes are
//! passed through unchanged (defensive: new schema versions should not
//! silently break the hash).
//!
//! # Canonical encoding
//!
//! After filtering, the stable-only subset is re-emitted by:
//! 1. Taking the filtered text lines.
//! 2. Running through [`edda_mimir_canonical::normalise_text`] to strip
//!    trailing whitespace and ensure canonical LF endings.
//!
//! The canonical bytes for each file are then concatenated in lex path order
//! and BLAKE3-hashed.

use crate::HashError;

/// Compute `surface_hash` over the given surface files.
///
/// Per §4.2: BLAKE3 over the concatenation of every `surface/*.toon` file in
/// lex order, considering stable items only. The walker:
/// 1. Sorts the input by path lex.
/// 2. For each file: parses the TOON, drops unstable items, re-emits the
///    stable-only subset in canonical form.
/// 3. Concatenates the canonical bytes in path order.
/// 4. BLAKE3 the concat → `"blake3:<hex>"`.
///
/// Returns `Err` if any file fails to parse as UTF-8 or canonicalise.
pub fn compute_surface_hash(
    surface_files: &[(String, Vec<u8>)],
) -> Result<String, HashError> {
    if surface_files.is_empty() {
        let hash = edda_cache::hash_bytes(b"");
        return Ok(format!("blake3:{hash}"));
    }

    // Sort by path lex — walker is invariant to caller order.
    let mut sorted: Vec<(&String, &Vec<u8>)> = surface_files.iter().map(|(p, b)| (p, b)).collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));

    let mut concat: Vec<u8> = Vec::new();

    for (path, bytes) in &sorted {
        let canonical = filter_and_canonicalise(path, bytes)?;
        concat.extend_from_slice(&canonical);
    }

    let hash = edda_cache::hash_bytes(&concat);
    Ok(format!("blake3:{hash}"))
}

/// Parse a surface/*.toon file, filter out unstable items, and return the
/// canonical bytes of the stable-only subset.
fn filter_and_canonicalise(path: &str, bytes: &[u8]) -> Result<Vec<u8>, HashError> {
    let text = std::str::from_utf8(bytes).map_err(|e| HashError::SurfaceParse {
        file: path.to_string(),
        msg: format!("not valid UTF-8: {e}"),
    })?;

    let filtered = filter_stable_only(path, text)?;

    let canonical = edda_mimir_canonical::normalise_text(&filtered).map_err(|e| {
        HashError::CanonicalEncode(e.to_string())
    })?;

    Ok(canonical.into_bytes())
}

/// Filter a surface TOON text to retain only stable items.
///
/// Supports two shapes:
/// - Top-level `stable_items[N]{...}:` / `unstable_items[M]{...}:` table split.
/// - Single `items[N]{name,stability,...}:` table with per-row stability column.
fn filter_stable_only(path: &str, text: &str) -> Result<String, HashError> {
    let _ = path; // Reserved for future diagnostics.

    // Detect whether the file uses the split-table schema.
    let has_unstable_table = text.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with("unstable_items[") && t.contains(']') && t.contains(':')
    });
    let has_stable_table = text.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with("stable_items[") && t.contains(']') && t.contains(':')
    });

    if has_stable_table || has_unstable_table {
        return Ok(filter_split_schema(text));
    }

    // Check for single-table schema with stability column.
    let has_items_table_with_stability = text.lines().any(|l| {
        let t = l.trim_start();
        t.starts_with("items[") && t.contains("stability") && t.contains(']') && t.contains(':')
    });

    if has_items_table_with_stability {
        return Ok(filter_single_table_schema(text));
    }

    // Neither schema detected — pass through unchanged.
    // This handles unknown/future schema versions defensively.
    Ok(text.to_string())
}

/// Filter a split-schema surface file.
///
/// Drops every line inside and including `unstable_items[...]{ ... }:`
/// blocks. Retains the header lines and `stable_items[...]` blocks.
fn filter_split_schema(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_unstable_block = false;

    for line in text.lines() {
        let trimmed = line.trim_start();

        // Detect unstable_items table header.
        if trimmed.starts_with("unstable_items[") && trimmed.contains(']') && trimmed.contains(':') {
            in_unstable_block = true;
            // Do not emit the header itself.
            continue;
        }

        if in_unstable_block {
            // Rows inside the unstable block are indented (start with whitespace).
            // A non-indented, non-blank line (or a different table header) ends the block.
            if trimmed.is_empty() {
                // Blank lines inside the block — skip.
                continue;
            }
            if line.starts_with(' ') || line.starts_with('\t') {
                // Indented row inside the unstable block — skip.
                continue;
            }
            // Non-indented content ends the unstable block.
            in_unstable_block = false;
        }

        out.push_str(line);
        out.push('\n');
    }

    // Trim trailing blank lines, normalise to single trailing LF.
    while out.ends_with("\n\n") {
        out.pop();
    }
    if !out.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }

    out
}

/// Filter a single-table schema surface file.
///
/// Detects the `stability` column index from `items[N]{name,stability,...}:`
/// and drops rows where that column is not `"stable"`.
fn filter_single_table_schema(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut stability_col: Option<usize> = None;
    let mut in_items_table = false;

    for line in text.lines() {
        let trimmed = line.trim_start();

        // Detect items[N]{...stability...}: header.
        if trimmed.starts_with("items[") && trimmed.contains("stability") && trimmed.contains(']') && trimmed.contains(':') {
            in_items_table = true;
            // Find stability column index from the field list.
            stability_col = extract_stability_col(trimmed);
            out.push_str(line);
            out.push('\n');
            continue;
        }

        if in_items_table {
            if trimmed.is_empty() {
                in_items_table = false;
                out.push('\n');
                continue;
            }
            if line.starts_with(' ') || line.starts_with('\t') {
                // A row in the items table — check stability column.
                if let Some(col) = stability_col {
                    let row_content = trimmed;
                    let fields: Vec<&str> = row_content.splitn(col + 2, ',').collect();
                    if fields.len() > col && fields[col].trim() == "stable" {
                        out.push_str(line);
                        out.push('\n');
                    }
                    // else: not "stable" — skip the row.
                } else {
                    // No stability column found — pass through.
                    out.push_str(line);
                    out.push('\n');
                }
                continue;
            }
            // Non-indented content ends the table.
            in_items_table = false;
        }

        out.push_str(line);
        out.push('\n');
    }

    while out.ends_with("\n\n") {
        out.pop();
    }
    if !out.ends_with('\n') && !out.is_empty() {
        out.push('\n');
    }

    out
}

/// Extract the 0-based column index of `"stability"` from a TOON table header
/// fragment like `{name,stability,signature,...}`.
fn extract_stability_col(header: &str) -> Option<usize> {
    let start = header.find('{')?;
    let end = header.find('}')?;
    let fields_str = &header[start + 1..end];
    fields_str
        .split(',')
        .position(|f| f.trim() == "stability")
}
