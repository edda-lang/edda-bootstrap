//! TOON lexer: tokenise a document into significant, indent-tagged lines.

use super::INDENT_STEP;
use super::value::ParseError;

#[derive(Debug)]
pub(super) struct LexLine {
    pub(super) indent: usize,
    pub(super) line_no: u32,
    pub(super) content: String,
}

/// Tokenise the document into significant lines. When
/// `strip_comment_prefix` is true, every data line must start with `\` (or
/// `\ `); the prefix is stripped before further processing.
pub(super) fn lex(input: &str, strip_comment_prefix: bool) -> Result<Vec<LexLine>, ParseError> {
    let mut out = Vec::new();
    for (i, raw_line) in input.lines().enumerate() {
        let line_no = u32::try_from(i + 1).unwrap_or(u32::MAX);
        let line = raw_line.trim_end_matches('\r');
        let processed = if strip_comment_prefix {
            match strip_header_prefix(line) {
                Some(rest) => rest.to_string(),
                None => continue,
            }
        } else {
            line.to_string()
        };
        if processed.bytes().any(|b| b == b'\t') {
            return Err(ParseError {
                line: line_no,
                message: "tab character in line (use spaces only)".into(),
            });
        }
        let indent = leading_spaces(&processed);
        let body = &processed[indent..];
        if body.is_empty() {
            continue;
        }
        if body.starts_with('\\') && !strip_comment_prefix {
            // Comment line in non-header mode.
            continue;
        }
        if indent % INDENT_STEP != 0 {
            return Err(ParseError {
                line: line_no,
                message: format!(
                    "indent {} is not a multiple of {}",
                    indent, INDENT_STEP
                ),
            });
        }
        out.push(LexLine {
            indent,
            line_no,
            content: body.to_string(),
        });
    }
    Ok(out)
}

/// Strip a header-style comment prefix from a line. The header uses
/// Edda's native line-comment syntax (`//`) so the bytes round-trip
/// through `edda_syntax::parse_file` cleanly — the cascade-restart pass
/// reads generated artifacts as ordinary source files. Accepts:
///   - `//` (bare): returns `Some("")` (blank content line in header)
///   - `//<space><rest>`: returns `Some(rest)` preserving any indentation
///     inside the comment
///
/// Returns `None` for any other line shape (non-comment lines are skipped
/// by the caller).
fn strip_header_prefix(line: &str) -> Option<&str> {
    let trimmed = line.trim_start_matches(' ');
    if trimmed == "//" {
        return Some("");
    }
    trimmed.strip_prefix("// ")
}

/// Count leading spaces in `s`.
fn leading_spaces(s: &str) -> usize {
    s.bytes().take_while(|&b| b == b' ').count()
}
