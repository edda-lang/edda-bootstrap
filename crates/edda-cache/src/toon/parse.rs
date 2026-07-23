//! TOON parser: recursive-descent over the lexer's indent-tagged lines.

use smol_str::SmolStr;

use super::INDENT_STEP;
use super::lex::{LexLine, lex};
use super::value::{ParseError, Value};

/// Parse a TOON document. The top-level value is always a map (or an empty
/// map for whitespace-only input).
pub fn parse(input: &str) -> Result<Value, ParseError> {
    let lines = lex(input, /* strip_comment_prefix */ false)?;
    let mut cursor = Cursor { lines: &lines, idx: 0 };
    parse_block(&mut cursor, 0)
}

/// Parse a TOON document encoded as a comment block, where every data line
/// is prefixed with `\ ` (or `\` alone for blank-content lines). Used by
/// [`crate::header`] to read the `\ @generated` header block at the top of
/// every artifact.
pub fn parse_commented(input: &str) -> Result<Value, ParseError> {
    let lines = lex(input, /* strip_comment_prefix */ true)?;
    let mut cursor = Cursor { lines: &lines, idx: 0 };
    parse_block(&mut cursor, 0)
}

struct Cursor<'a> {
    lines: &'a [LexLine],
    idx: usize,
}

impl<'a> Cursor<'a> {
    fn peek(&self) -> Option<&'a LexLine> {
        self.lines.get(self.idx)
    }

    fn advance(&mut self) {
        self.idx += 1;
    }
}

/// Parse a block of TOON entries at exactly `indent` spaces of indentation.
/// Returns either a `Map` (key:value entries) or a `List` (YAML-style `- `
/// items). Mixed blocks are rejected.
fn parse_block(cursor: &mut Cursor<'_>, indent: usize) -> Result<Value, ParseError> {
    let first = match cursor.peek() {
        Some(line) if line.indent == indent => line,
        Some(line) if line.indent < indent => {
            // Empty block (no entries at this indent level).
            return Ok(Value::Map(Vec::new()));
        }
        Some(line) => {
            return Err(ParseError {
                line: line.line_no,
                message: format!(
                    "unexpected indent {} (expected {})",
                    line.indent, indent
                ),
            });
        }
        None => return Ok(Value::Map(Vec::new())),
    };

    if first.content.starts_with("- ") || first.content == "-" {
        parse_list_block(cursor, indent)
    } else {
        parse_map_block(cursor, indent)
    }
}

/// Parse a block of `key: value` and `key:` lines at the given indent.
fn parse_map_block(cursor: &mut Cursor<'_>, indent: usize) -> Result<Value, ParseError> {
    let mut entries: Vec<(SmolStr, Value)> = Vec::new();
    while let Some(line) = cursor.peek() {
        if line.indent < indent {
            break;
        }
        if line.indent > indent {
            return Err(ParseError {
                line: line.line_no,
                message: format!(
                    "unexpected indent {} (expected {})",
                    line.indent, indent
                ),
            });
        }
        if line.content.starts_with('-') {
            return Err(ParseError {
                line: line.line_no,
                message: "list item appeared inside a map block".into(),
            });
        }
        let (key, after_key) = split_key(&line.content, line.line_no)?;
        let line_no = line.line_no;
        cursor.advance();
        let value = parse_value(cursor, indent + INDENT_STEP, after_key, line_no)?;
        entries.push((key, value));
    }
    Ok(Value::Map(entries))
}

/// Parse a block of `- ...` list items at the given indent.
fn parse_list_block(cursor: &mut Cursor<'_>, indent: usize) -> Result<Value, ParseError> {
    let mut items: Vec<Value> = Vec::new();
    while let Some(line) = cursor.peek() {
        if line.indent < indent {
            break;
        }
        if line.indent > indent {
            return Err(ParseError {
                line: line.line_no,
                message: format!(
                    "unexpected indent {} inside list (expected {})",
                    line.indent, indent
                ),
            });
        }
        if !line.content.starts_with('-') {
            return Err(ParseError {
                line: line.line_no,
                message: "expected `- ` list item".into(),
            });
        }
        let after_dash = if line.content == "-" {
            String::new()
        } else if let Some(rest) = line.content.strip_prefix("- ") {
            rest.to_string()
        } else {
            return Err(ParseError {
                line: line.line_no,
                message: "list marker `-` must be followed by a space or end-of-line".into(),
            });
        };
        let line_no = line.line_no;
        cursor.advance();
        let item = parse_list_item(cursor, indent + INDENT_STEP, &after_dash, line_no)?;
        items.push(item);
    }
    Ok(Value::List(items))
}

/// Parse a single list item. When `after_dash` is non-empty it carries the
/// first `key: value` pair on the same line as the `- ` marker; that pair
/// joins the rest of the item's map at `indent`. A bare scalar after the
/// `-` (no colon) is admitted as a scalar list item — used by every list
/// of strings in `manifest.toon` (sources, nested_deps, etc.).
fn parse_list_item(
    cursor: &mut Cursor<'_>,
    indent: usize,
    after_dash: &str,
    line_no: u32,
) -> Result<Value, ParseError> {
    if after_dash.is_empty() {
        // Fully-indented body following the `-` marker.
        return parse_block(cursor, indent);
    }
    let trimmed = after_dash.trim();
    let is_bare_quoted =
        trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2;
    if is_bare_quoted || !trimmed.contains(':') {
        return Ok(Value::Scalar(SmolStr::new(parse_scalar(trimmed))));
    }
    let (key, after_key) = split_key(after_dash, line_no)?;
    let first_value = parse_value(cursor, indent + INDENT_STEP, after_key, line_no)?;
    let mut entries: Vec<(SmolStr, Value)> = vec![(key, first_value)];
    // Subsequent `key: value` lines at exactly `indent` belong to the same
    // item until indent drops or the next `-` arrives at the parent level.
    while let Some(next) = cursor.peek() {
        if next.indent < indent {
            break;
        }
        if next.indent > indent {
            return Err(ParseError {
                line: next.line_no,
                message: format!(
                    "unexpected indent {} inside list item (expected {})",
                    next.indent, indent
                ),
            });
        }
        if next.content.starts_with('-') {
            break;
        }
        let (key2, after_key2) = split_key(&next.content, next.line_no)?;
        let line_no2 = next.line_no;
        cursor.advance();
        let value2 = parse_value(cursor, indent + INDENT_STEP, after_key2, line_no2)?;
        entries.push((key2, value2));
    }
    Ok(Value::Map(entries))
}

/// Split a `key: rest` line into the key (with any `[N]{...}` schema
/// annotation stripped) and the trimmed remainder.
fn split_key(line: &str, line_no: u32) -> Result<(SmolStr, String), ParseError> {
    let colon = line.find(':').ok_or_else(|| ParseError {
        line: line_no,
        message: "expected `key: value` or `key:`".into(),
    })?;
    let raw_key = &line[..colon];
    let key = strip_schema_annotation(raw_key);
    if key.is_empty() {
        return Err(ParseError {
            line: line_no,
            message: "empty key".into(),
        });
    }
    let rest = line[colon + 1..].trim_start_matches(' ').to_string();
    Ok((SmolStr::new(key), rest))
}

/// `artifacts[N]{path,hash}` → `artifacts`. The schema annotation is
/// informational only; this lexer drops it on read.
fn strip_schema_annotation(raw_key: &str) -> &str {
    let mut end = raw_key.len();
    if let Some(idx) = raw_key.find('[') {
        end = end.min(idx);
    }
    if let Some(idx) = raw_key.find('{') {
        end = end.min(idx);
    }
    raw_key[..end].trim()
}

/// Parse the value portion of a `key: rest` line, possibly recursing into
/// an indented block when `rest` is empty.
fn parse_value(
    cursor: &mut Cursor<'_>,
    child_indent: usize,
    rest: String,
    _line_no: u32,
) -> Result<Value, ParseError> {
    let trimmed = rest.trim_end();
    if trimmed.is_empty() {
        // Indented block follows.
        return parse_block(cursor, child_indent);
    }
    if trimmed == "[]" {
        return Ok(Value::List(Vec::new()));
    }
    Ok(Value::Scalar(SmolStr::new(parse_scalar(trimmed))))
}

/// Strip optional surrounding double quotes and undo
/// [`super::write::Writer`]'s backslash escaping. Without this, a value
/// that round-trips through `write` then `parse` picks up one extra
/// backslash per occurrence; on a project whose paths already contain
/// backslashes (Windows), repeated build/verify cycles double the count
/// every pass.
fn parse_scalar(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        unescape_quoted(&s[1..s.len() - 1])
    } else {
        s.to_string()
    }
}

/// Undo the two-character escaping [`super::write::Writer::write_scalar_value`]
/// applies (`\"` → `"`, `\\` → `\`). A trailing lone backslash (malformed
/// input) is passed through literally rather than dropped.
fn unescape_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(escaped) => out.push(escaped),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}
