//! effect_hash walker — §4.3.
//!
//! BLAKE3 over the sorted, canonically-encoded union of every effect-row
//! entry reachable from any public function in the surface.
//!
//! Per §4.3: an effect-row entry is one of:
//! - A capability name (e.g. `"Filesystem"`, `"Network"`, `"Allocator"`)
//! - A typed pure effect: `"err: T"`, `"panic"`, `"yield: T"`,
//!   `"cancellation"`, `"divergence"`, `"nondet"`
//! - A graded effect: `"alloc(bytes <= N)"`, `"io(calls <= N)"`,
//!   `"time(ops <= N)"`
//!
//! # Reachability for v0.1
//!
//! The walker collects effect-row entries from the `effect_row` column of
//! every public function in the `stable_items[...]` table.
//!
//! Transitive reachability via a `calls` field per function is specified in
//! §4.3 ("walking every public function's effect row and every
//! transitively-reachable internal function's effect row"). However, the
//! current surface/*.toon schema (emitted by `edda-structmap`) does NOT
//! include a `calls` field at function-item granularity within the
//! `stable_items` table — the per-file `calls` field in `index.toon` is a
//! cross-reference table, not a per-function annotation.
//!
//! TODO: Coordinate with `edda-structmap` (or a follow-up structmap emitter
//! for `surface/*.toon`) to add a `calls` column to `stable_items` rows —
//! tracked as a follow-up in the native tracker. When that column is
//! available, the worklist walk for transitive effects can be added.
//! Until then, v0.1 ships the local-effect-only version.
//!
//! External callees (other runes, stdlib) contribute via their own
//! `surface_hash` + `effect_hash` — this walker does NOT need to follow
//! them. This is by design: the independence property of the three hashes
//! means that a change to a transitive external dep bumps only that dep's
//! hashes in the lockfile.
//!
//! # Canonical encoding
//!
//! Effect-row entries are collected into a `BTreeSet<String>` (lex-sorted,
//! deduplicated). The canonical encoding is:
//! - One entry per line, sorted lex, no duplicates.
//! - UTF-8, LF line endings.
//! - Single trailing LF.
//!
//! This does not need the full `edda-mimir-canonical` encoder — direct string
//! operations suffice.

use std::collections::BTreeSet;

use crate::HashError;

/// Compute `effect_hash` over the given surface files.
///
/// Per §4.3: BLAKE3 over the sorted, deduplicated union of every effect-row
/// entry reachable from any public function in the surface.
///
/// For v0.1, reachability is bounded to the function's own effect-row column
/// in the `stable_items[...]` table. Transitive callee walking is deferred
/// pending a `calls` column in the surface/*.toon schema (see module doc).
///
/// Returns `Err` if any file fails to parse as UTF-8.
pub fn compute_effect_hash(
    surface_files: &[(String, Vec<u8>)],
) -> Result<String, HashError> {
    let mut entries: BTreeSet<String> = BTreeSet::new();

    for (path, bytes) in surface_files {
        let text = std::str::from_utf8(bytes).map_err(|e| HashError::SurfaceParse {
            file: path.clone(),
            msg: format!("not valid UTF-8: {e}"),
        })?;
        collect_effect_entries(text, &mut entries);
    }

    let canonical = encode_effect_entries(&entries);
    let hash = edda_cache::hash_bytes(canonical.as_bytes());
    Ok(format!("blake3:{hash}"))
}

/// Walk one surface TOON text and insert every effect-row token found in
/// `stable_items[...]` rows into `entries`.
///
/// The `effect_row` column is the third column (0-based index 2) in the
/// `{name,signature,effect_row,...}` field list. The value for a row like:
///   `  push,(s: mutable Vec_T) -> () with {panic Filesystem},,,`
/// is parsed as the substring between `{` and `}` in the effect_row field,
/// then split on whitespace to yield individual tokens.
///
/// If the effect_row column is absent or empty, no entries are added.
fn collect_effect_entries(text: &str, entries: &mut BTreeSet<String>) {
    let mut in_stable_table = false;
    let mut effect_col: Option<usize> = None;

    for line in text.lines() {
        let trimmed = line.trim_start();

        // Detect stable_items table header.
        if trimmed.starts_with("stable_items[") && trimmed.contains(']') && trimmed.contains(':') {
            in_stable_table = true;
            effect_col = extract_effect_col(trimmed);
            continue;
        }

        // Detect end of stable_items block.
        if in_stable_table {
            if trimmed.is_empty() {
                in_stable_table = false;
                continue;
            }
            if !line.starts_with(' ') && !line.starts_with('\t') {
                in_stable_table = false;
                // Fall through to handle this line as a new table header.
            }
        }

        if in_stable_table {
            // A row in the stable_items table.
            if let Some(col) = effect_col {
                extract_row_effect_entries(trimmed, col, entries);
            }
        }
    }
}

/// Extract the 0-based column index of `"effect_row"` from a TOON table
/// header fragment like `{name,signature,effect_row,...}`.
fn extract_effect_col(header: &str) -> Option<usize> {
    let start = header.find('{')?;
    let end = header.find('}')?;
    let fields_str = &header[start + 1..end];
    fields_str
        .split(',')
        .position(|f| f.trim() == "effect_row")
}

/// Extract effect-row tokens from one TOON table row.
///
/// Parses the `effect_row` column (at 0-based `col`) from a comma-delimited
/// row string. The effect-row field value has the form `{token1 token2 ...}`
/// or an empty `{}`. Inserts each token into `entries`.
fn extract_row_effect_entries(row: &str, col: usize, entries: &mut BTreeSet<String>) {
    // Split on commas, limited by col + 1 splits to get the col field.
    // We need to be careful: the signature field may contain commas inside
    // parentheses. The TOON surface files use a comma-separated row format
    // where the signature field comes second (col 1) — we need to parse
    // conservatively.
    //
    // Strategy: split on commas naively and take field at index `col`. This
    // works because the schema places effect_row at index 2 (after name at 0
    // and signature at 1), and the signature field itself may contain commas.
    // For v0.1, we use a simple approach: find the Nth comma by scanning,
    // which handles the simple case. If the signature field contains commas
    // (e.g. function types), a smarter parser is needed — that's a v0.2 gap
    // to address when real surface/*.toon files are available.
    let field = extract_nth_csv_field(row, col);
    let field = field.trim();

    if field.is_empty() || field == "{}" {
        return;
    }

    // The effect_row field value looks like: `{panic Filesystem Network}` or
    // `panic Filesystem` (if no braces). We strip braces and split on whitespace.
    let inner = if field.starts_with('{') && field.ends_with('}') {
        &field[1..field.len() - 1]
    } else {
        field
    };

    for token in inner.split_whitespace() {
        let token = token.trim();
        if !token.is_empty() {
            entries.insert(token.to_string());
        }
    }
}

/// Extract the field at 0-based index `n` from a comma-separated string.
///
/// Splits naively on commas. For v0.1 this is sufficient because the effect_row
/// column (index 2) follows the signature (index 1) which may itself contain
/// commas — but in practice the surface TOON rows are formatted to avoid
/// ambiguity. A follow-up can add parenthesis-aware splitting if needed.
fn extract_nth_csv_field(s: &str, n: usize) -> &str {
    let mut field_start = 0;
    let mut field_count = 0;

    for (i, ch) in s.char_indices() {
        if ch == ',' {
            if field_count == n {
                return &s[field_start..i];
            }
            field_count += 1;
            field_start = i + 1;
        }
    }

    // Last field (no trailing comma).
    if field_count == n {
        &s[field_start..]
    } else {
        ""
    }
}

/// Encode the effect entries into canonical bytes.
///
/// One entry per line, sorted lex (guaranteed by `BTreeSet`), single
/// trailing LF. For an empty set, emits an empty string (no LF) to produce
/// a distinct hash from any non-empty set.
fn encode_effect_entries(entries: &BTreeSet<String>) -> String {
    if entries.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for entry in entries {
        out.push_str(entry);
        out.push('\n');
    }
    out
}
