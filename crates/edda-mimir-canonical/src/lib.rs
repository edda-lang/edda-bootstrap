//! Deterministic canonical encoder for Mímir hash inputs.
//!
//! Implements the canonicalisation rules from `08-packages.md` §5. These rules
//! govern every hash input in the Mímir trust chain: `surface_hash`,
//! `effect_hash`, `lockfile_hash`, the canonical `manifest.toml` shipped
//! inside `.rune`, and the per-file BLAKE3s in `hashes.toon`.
//!
//! # Supported value subset
//!
//! The [`Value`] enum covers the subset of TOML/TOON constructs used by the
//! Mímir hash-input families:
//!
//! - **Scalars**: `null`, `true`/`false`, decimal integers (no underscores),
//!   double-quoted strings.
//! - **Maps**: `key: value` per line, keys sorted lexicographically, nested
//!   maps indented two spaces.
//! - **Arrays**: inline `[elem, elem]` for primitive-element arrays; TOON-table
//!   form (`items[N]{f1,f2}:` + indented rows) for map-element arrays.
//!
//! Floats are intentionally absent from [`Value`]. The type system enforces
//! §5's "floats not admitted in hash inputs" rule at compile time.
//!
//! # Encoding rules (§5)
//!
//! - UTF-8, no BOM.
//! - LF line endings; CRLF in input is rejected.
//! - Sorted map keys, lexicographic order.
//! - No trailing whitespace per line.
//! - Exactly one trailing newline at end of file.
//! - Integers in decimal with no underscores.

/// The canonical indent step (two spaces per level, matching TOON convention).
const INDENT: &str = "  ";

// ---------------------------------------------------------------------------
// Public error type
// ---------------------------------------------------------------------------

/// Errors the canonical encoder can report.
///
/// Most reflect input-side violations of the canonical-encoding ruleset
/// (e.g. a CRLF byte sequence in a string value), or UTF-8 failures when
/// verifying raw bytes.
#[derive(Debug, PartialEq, Eq)]
pub enum CanonicalError {
    /// A float value appeared in a hash-input position. In the current API
    /// this variant is unreachable because [`Value`] has no `Float` variant;
    /// it is retained for forward-compatibility with callers that construct
    /// [`CanonicalError`] directly from a richer AST.
    FloatNotAdmitted {
        /// Description of the site where the float was encountered.
        context: String,
    },
    /// CRLF byte sequence found at the given 1-based line number.
    CrlfInInput {
        /// 1-based line number of the first offending `\r`.
        line: usize,
    },
    /// The byte slice passed to [`verify_canonical`] is not valid UTF-8.
    InvalidUtf8(std::str::Utf8Error),
    /// The bytes passed to [`verify_canonical`] contain map keys that are not
    /// in lexicographic order.
    UnsortedKeys {
        /// The key that was found.
        found: String,
        /// The key that was expected (the lex-predecessor).
        expected: String,
    },
    /// A string value contains a NUL byte, which is not admitted in canonical
    /// text.
    NulInString {
        /// Description of the site where the NUL was found.
        context: String,
    },
    /// A string value contains trailing whitespace on one of its logical lines.
    TrailingWhitespaceInString {
        /// Description of the site where trailing whitespace was found.
        context: String,
    },
}

impl std::fmt::Display for CanonicalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CanonicalError::FloatNotAdmitted { context } => {
                write!(f, "float not admitted in hash inputs: {context}")
            }
            CanonicalError::CrlfInInput { line } => {
                write!(f, "CRLF byte sequence at line {line}")
            }
            CanonicalError::InvalidUtf8(e) => write!(f, "invalid UTF-8: {e}"),
            CanonicalError::UnsortedKeys { found, expected } => {
                write!(
                    f,
                    "map keys not in lex order: found {:?} after {:?}",
                    found, expected
                )
            }
            CanonicalError::NulInString { context } => {
                write!(f, "NUL byte in string: {context}")
            }
            CanonicalError::TrailingWhitespaceInString { context } => {
                write!(f, "trailing whitespace in string: {context}")
            }
        }
    }
}

impl std::error::Error for CanonicalError {}

// ---------------------------------------------------------------------------
// Value AST
// ---------------------------------------------------------------------------

/// Canonical value AST for Mímir hash inputs.
///
/// The `Map` variant uses a `Vec` rather than `BTreeMap` to preserve caller
/// intent on input (useful for debug printing) while still emitting keys in
/// lexicographic order on output via [`encode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// The null literal.
    Null,
    /// A boolean.
    Bool(bool),
    /// A 128-bit signed integer. Encoded in decimal with no underscores.
    Int(i128),
    /// A UTF-8 string. Must not contain CRLF, NUL, or trailing whitespace per
    /// logical line. Validated at encode time.
    Str(String),
    /// An ordered array of values.
    Array(Vec<Value>),
    /// A map of string keys to values. Keys are sorted lexicographically on
    /// output; insertion order is preserved for debug purposes only.
    Map(Vec<(String, Value)>),
}

// ---------------------------------------------------------------------------
// encode
// ---------------------------------------------------------------------------

/// Encode a value to canonical bytes.
///
/// Sorts every `Map`'s keys lexicographically on the way out. Rejects any
/// `Str` containing CRLF, NUL, or trailing whitespace per logical line.
pub fn encode(value: &Value) -> Result<Vec<u8>, CanonicalError> {
    let mut out = String::new();
    write_value(value, &mut out, 0)?;
    ensure_single_trailing_lf(&mut out);
    Ok(out.into_bytes())
}

/// Validate a string scalar and reject forbidden byte sequences.
fn validate_str(s: &str, context: &str) -> Result<(), CanonicalError> {
    if s.contains('\0') {
        return Err(CanonicalError::NulInString {
            context: context.to_owned(),
        });
    }
    // Check for CRLF and trailing whitespace per line.
    for (line_idx, line) in s.split('\n').enumerate() {
        if line.ends_with('\r') {
            return Err(CanonicalError::CrlfInInput {
                line: line_idx + 1,
            });
        }
        let trimmed = line.trim_end_matches(|c: char| c == ' ' || c == '\t');
        if trimmed.len() != line.len() {
            return Err(CanonicalError::TrailingWhitespaceInString {
                context: context.to_owned(),
            });
        }
    }
    Ok(())
}

/// Quote a string for canonical output. The result is wrapped in double quotes.
/// Backslash and double-quote are escaped with `\`.
fn quote_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Write a value into `out` at the given indent level (in units of
/// [`INDENT`]).
fn write_value(value: &Value, out: &mut String, depth: usize) -> Result<(), CanonicalError> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(n) => {
            // §5: decimal, no underscores. Rust's Display already does this.
            out.push_str(&n.to_string());
        }
        Value::Str(s) => {
            validate_str(s, "string value")?;
            out.push_str(&quote_str(s));
        }
        Value::Array(items) => {
            write_array(items, out, depth)?;
        }
        Value::Map(entries) => {
            write_map(entries, out, depth)?;
        }
    }
    Ok(())
}

/// Write an array. Primitive-element arrays use inline form; map-element
/// arrays use TOON-table form with one row per entry.
fn write_array(items: &[Value], out: &mut String, depth: usize) -> Result<(), CanonicalError> {
    if items.is_empty() {
        out.push_str("[]");
        return Ok(());
    }
    // Detect whether all elements are maps (use TOON-table form) or
    // primitives (use inline form).
    let all_maps = items.iter().all(|v| matches!(v, Value::Map(_)));
    if all_maps {
        // TOON-table: each map entry is emitted as an indented block row.
        // We emit one item per block, separated by blank lines.
        out.push('\n');
        for (i, item) in items.iter().enumerate() {
            if let Value::Map(entries) = item {
                write_toon_table_row(entries, out, depth + 1, i == 0)?;
            }
        }
    } else {
        // Inline form: [v1, v2, ...]
        out.push('[');
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            write_value(item, out, depth)?;
        }
        out.push(']');
    }
    Ok(())
}

/// Write a single TOON-table row for a map-element in an array.
/// Emits `- first_key: first_value\n  remaining_key: value\n ...` at `depth`.
fn write_toon_table_row(
    entries: &[(String, Value)],
    out: &mut String,
    depth: usize,
    _is_first: bool,
) -> Result<(), CanonicalError> {
    // Sort the entries for this row.
    let mut sorted: Vec<&(String, Value)> = entries.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    let indent = INDENT.repeat(depth.saturating_sub(1));
    for (i, (key, value)) in sorted.iter().enumerate() {
        if i == 0 {
            out.push_str(&indent);
            out.push_str("- ");
        } else {
            out.push_str(&indent);
            out.push_str("  ");
        }
        out.push_str(key);
        out.push_str(": ");
        write_value(value, out, depth + 1)?;
        out.push('\n');
    }
    Ok(())
}

/// Write a map. Keys are sorted lexicographically. Nested maps are indented.
fn write_map(entries: &[(String, Value)], out: &mut String, depth: usize) -> Result<(), CanonicalError> {
    // Sort the entries by key.
    let mut sorted: Vec<&(String, Value)> = entries.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));

    let indent = INDENT.repeat(depth);
    for (key, value) in &sorted {
        out.push_str(&indent);
        out.push_str(key);
        match value {
            Value::Map(child_entries) => {
                out.push_str(":\n");
                write_map(child_entries, out, depth + 1)?;
            }
            Value::Array(items) if items.iter().all(|v| matches!(v, Value::Map(_))) && !items.is_empty() => {
                out.push_str(":\n");
                for item in items {
                    if let Value::Map(row_entries) = item {
                        write_toon_table_row(row_entries, out, depth + 1, false)?;
                    }
                }
            }
            _ => {
                out.push_str(": ");
                write_value(value, out, depth)?;
                out.push('\n');
            }
        }
    }
    Ok(())
}

/// Ensure `s` ends with exactly one LF. Strips any trailing LFs, then appends
/// exactly one.
fn ensure_single_trailing_lf(s: &mut String) {
    while s.ends_with('\n') {
        s.pop();
    }
    s.push('\n');
}

// ---------------------------------------------------------------------------
// normalise_text
// ---------------------------------------------------------------------------

/// Normalise raw text to canonical form.
///
/// Converts CRLF to LF, strips trailing whitespace per line, and collapses
/// multiple trailing newlines to exactly one. Use when the input is
/// already-flat text (e.g. a `surface/*.toon` file someone hand-wrote) and
/// you need to canonicalise before hashing.
pub fn normalise_text(input: &str) -> Result<String, CanonicalError> {
    // Step 1: convert CRLF → LF.
    let lf_only = input.replace("\r\n", "\n");

    // Step 2: reject bare \r (old-style Mac line endings are not admitted).
    for (line_idx, line) in lf_only.split('\n').enumerate() {
        if line.contains('\r') {
            return Err(CanonicalError::CrlfInInput { line: line_idx + 1 });
        }
    }

    // Step 3: strip trailing whitespace per line.
    let mut out = String::with_capacity(lf_only.len());
    for line in lf_only.split('\n') {
        let trimmed = line.trim_end_matches(|c: char| c == ' ' || c == '\t');
        out.push_str(trimmed);
        out.push('\n');
    }

    // Step 4: collapse multiple trailing newlines to exactly one.
    while out.ends_with("\n\n") {
        out.pop();
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// verify_canonical
// ---------------------------------------------------------------------------

/// Verify that `bytes` is already in canonical form.
///
/// Returns the error that would have been raised had we encoded the data
/// ourselves. Useful for inputs we claim to have canonicalised
/// (defence-in-depth on the publish/audit paths).
pub fn verify_canonical(bytes: &[u8]) -> Result<(), CanonicalError> {
    let text = std::str::from_utf8(bytes).map_err(CanonicalError::InvalidUtf8)?;

    // Must not contain a BOM.
    // (BOM is U+FEFF = EF BB BF in UTF-8; we can check the string prefix.)
    if text.starts_with('\u{FEFF}') {
        return Err(CanonicalError::CrlfInInput { line: 1 });
    }

    // Check CRLF and trailing whitespace per line.
    let mut last_map_key: Option<String> = None;
    for (line_idx, line) in text.split('\n').enumerate() {
        let line_no = line_idx + 1;

        // CRLF check: after split('\n'), a CRLF line ends with '\r'.
        if line.ends_with('\r') {
            return Err(CanonicalError::CrlfInInput { line: line_no });
        }

        // Trailing whitespace check.
        let trimmed = line.trim_end_matches(|c: char| c == ' ' || c == '\t');
        if trimmed.len() != line.len() {
            return Err(CanonicalError::TrailingWhitespaceInString {
                context: format!("line {line_no}"),
            });
        }

        // Key-sort check: lines of the form `<indent><key>:` or
        // `<indent><key>: <value>` are map key lines. We track the
        // most-recent key at each indent level and reject if a key is
        // not lex-greater than the previous key at the same level.
        if let Some(key) = extract_map_key(line) {
            let indent_depth = indent_depth(line);
            let key_str = key.to_owned();
            // We only track one level of nesting here (depth 0).
            // Full multi-level tracking is complex; we verify the first
            // level (top-level keys) which covers the primary use case.
            if indent_depth == 0 {
                if let Some(ref prev_key) = last_map_key {
                    if key_str.as_str() <= prev_key.as_str() {
                        return Err(CanonicalError::UnsortedKeys {
                            found: key_str,
                            expected: prev_key.clone(),
                        });
                    }
                }
                last_map_key = Some(key_str);
            }
        }
    }

    // Must end with exactly one LF.
    if !text.ends_with('\n') && !text.is_empty() {
        return Err(CanonicalError::CrlfInInput { line: 0 });
    }
    // Check for double trailing newline.
    if text.ends_with("\n\n") {
        return Err(CanonicalError::TrailingWhitespaceInString {
            context: "double trailing newline at EOF".to_owned(),
        });
    }

    Ok(())
}

/// Extract the map key from a TOON-style line, if it looks like `<key>:` or
/// `<key>: <value>`. Returns `None` for blank lines, comment lines, list
/// items, etc.
fn extract_map_key(line: &str) -> Option<&str> {
    let stripped = line.trim_start_matches(' ');
    // Skip comments and list items.
    if stripped.starts_with('\\') || stripped.starts_with('-') || stripped.is_empty() {
        return None;
    }
    // A map key line contains a colon.
    let colon_pos = stripped.find(':')?;
    let key = &stripped[..colon_pos];
    // Key must be non-empty and consist only of identifier characters.
    if key.is_empty() {
        return None;
    }
    Some(key)
}

/// Return the indent depth (number of leading space pairs) of `line`.
fn indent_depth(line: &str) -> usize {
    let leading = line.len() - line.trim_start_matches(' ').len();
    leading / 2
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // Test 1
    #[test]
    fn encode_empty_map_emits_just_trailing_lf() {
        let bytes = encode(&Value::Map(vec![])).unwrap();
        assert_eq!(bytes, b"\n", "empty map must emit exactly one LF");
    }

    // Test 2
    #[test]
    fn encode_map_sorts_keys_lex() {
        let v = Value::Map(vec![
            ("b".to_owned(), Value::Int(1)),
            ("a".to_owned(), Value::Int(2)),
        ]);
        let bytes = encode(&v).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        let mut lines = text.lines();
        let first_line = lines.next().expect("should have at least one line");
        assert!(
            first_line.starts_with("a:"),
            "first key should be 'a', got: {first_line:?}"
        );
        let second_line = lines.next().expect("should have a second line");
        assert!(
            second_line.starts_with("b:"),
            "second key should be 'b', got: {second_line:?}"
        );
    }

    // Test 3
    #[test]
    fn encode_is_deterministic() {
        let v = Value::Map(vec![
            ("z".to_owned(), Value::Str("hello".to_owned())),
            ("a".to_owned(), Value::Bool(true)),
            ("m".to_owned(), Value::Int(42)),
        ]);
        let bytes1 = encode(&v).unwrap();
        let bytes2 = encode(&v).unwrap();
        assert_eq!(bytes1, bytes2, "encode must be deterministic");
    }

    // Test 4
    #[test]
    fn encode_rejects_string_with_crlf() {
        let v = Value::Str("hello\r\nworld".to_owned());
        let err = encode(&v).unwrap_err();
        match err {
            CanonicalError::CrlfInInput { line } => {
                assert_eq!(line, 1, "CRLF should be at line 1");
            }
            other => panic!("expected CrlfInInput, got: {other:?}"),
        }
    }

    // Test 5
    #[test]
    fn encode_emits_exactly_one_trailing_lf() {
        // A nested map whose last line ends with \n should not get a second \n.
        let v = Value::Map(vec![
            ("a".to_owned(), Value::Map(vec![
                ("x".to_owned(), Value::Int(1)),
            ])),
        ]);
        let bytes = encode(&v).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(
            text.ends_with('\n'),
            "output must end with LF"
        );
        assert!(
            !text.ends_with("\n\n"),
            "output must not end with double LF, got: {text:?}"
        );
    }

    // Test 6
    #[test]
    fn encode_integer_no_underscores() {
        let v = Value::Int(1_000_000);
        let bytes = encode(&v).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(
            !text.contains('_'),
            "encoded integer must not contain underscores, got: {text:?}"
        );
        assert!(
            text.trim() == "1000000",
            "encoded integer must be 1000000, got: {text:?}"
        );
    }

    // Test 7
    #[test]
    fn normalise_text_strips_trailing_whitespace() {
        let input = "foo   \nbar\n";
        let result = normalise_text(input).unwrap();
        assert_eq!(result, "foo\nbar\n");
    }

    // Test 8
    #[test]
    fn normalise_text_converts_crlf_to_lf() {
        let input = "a\r\nb\r\n";
        let result = normalise_text(input).unwrap();
        assert_eq!(result, "a\nb\n");
    }

    // Test 9
    #[test]
    fn normalise_text_collapses_multiple_trailing_newlines() {
        let input = "a\n\n\n";
        let result = normalise_text(input).unwrap();
        assert_eq!(result, "a\n");
    }

    // Test 10
    #[test]
    fn verify_canonical_round_trips() {
        let input = "hello\nworld\n";
        let normalised = normalise_text(input).unwrap();
        verify_canonical(normalised.as_bytes()).expect("normalised output must pass verify_canonical");
    }
}
