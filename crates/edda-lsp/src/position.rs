//! LSP `Position` / `Range` conversion against a source buffer.
//!
//! LSP positions are line + character offsets. The `character` unit depends
//! on the negotiated `positionEncoding`:
//!
//! - `utf-16` (default per the spec): UTF-16 code units from the start of
//!   the line. ASCII is one unit per byte; surrogate-pair codepoints
//!   (U+10000..) count as two units; everything else is one unit.
//! - `utf-8`: UTF-8 bytes from the start of the line. Matches
//!   [`edda_span::BytePos`] semantics directly.
//! - `utf-32`: Unicode codepoints. Supported here for completeness; rarely
//!   negotiated in practice.
//!
//! This module converts in both directions against the buffer text the
//! daemon already holds. Conversion is always relative to a specific
//! buffer — there is no encoding-free middle representation.
//!
//! All loops here are bounded by the input length; there is no recursion.

use lsp_types::{Position, PositionEncodingKind, Range};

use edda_span::{BytePos, FileId, Span, SourceMap};

/// The position encoding currently in force on a session.
///
/// Negotiated during `initialize`. Defaults to [`Utf16`](Self::Utf16) per
/// LSP, but the server advertises [`Utf8`](Self::Utf8) as its preferred
/// encoding because it matches [`edda_span::BytePos`] exactly and avoids
/// per-position UTF-16 re-encoding work.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PositionEncoding {
    /// UTF-8 bytes — matches [`edda_span::BytePos`].
    Utf8,
    /// UTF-16 code units — the LSP default.
    Utf16,
    /// Unicode codepoints (chars).
    Utf32,
}

impl PositionEncoding {
    /// Project to the LSP wire kind for capability advertisement.
    pub fn to_lsp(self) -> PositionEncodingKind {
        match self {
            Self::Utf8 => PositionEncodingKind::UTF8,
            Self::Utf16 => PositionEncodingKind::UTF16,
            Self::Utf32 => PositionEncodingKind::UTF32,
        }
    }

    /// Parse an LSP wire kind. Unknown kinds fall back to UTF-16.
    pub fn from_lsp(kind: &PositionEncodingKind) -> Self {
        if *kind == PositionEncodingKind::UTF8 {
            Self::Utf8
        } else if *kind == PositionEncodingKind::UTF32 {
            Self::Utf32
        } else {
            Self::Utf16
        }
    }
}

/// Convert a UTF-8 byte offset inside `text` to an LSP [`Position`].
///
/// The line is 0-based; the column is 0-based and expressed in the
/// negotiated `encoding`. `\r\n` is treated as a line break on the `\n`.
pub fn byte_to_position(text: &str, byte_offset: usize, encoding: PositionEncoding) -> Position {
    let clamped = byte_offset.min(text.len());
    let mut line: u32 = 0;
    let mut line_start: usize = 0;
    // Bounded loop: each iteration advances by one byte; loop terminates at
    // `clamped` which is bounded by `text.len()`.
    for (i, b) in text.as_bytes()[..clamped].iter().enumerate() {
        if *b == b'\n' {
            line = line.saturating_add(1);
            line_start = i + 1;
        }
    }
    let column_text = &text[line_start..clamped];
    let character = match encoding {
        PositionEncoding::Utf8 => column_text.len() as u32,
        PositionEncoding::Utf16 => count_utf16_units(column_text),
        PositionEncoding::Utf32 => column_text.chars().count() as u32,
    };
    Position { line, character }
}

/// Convert an LSP [`Position`] inside `text` to a UTF-8 byte offset.
pub fn position_to_byte(text: &str, pos: Position, encoding: PositionEncoding) -> usize {
    let line_start = line_start_offset(text, pos.line);
    let line_end = next_line_offset(text, line_start);
    let line_text = &text[line_start..line_end];
    let target = pos.character as usize;
    let column_bytes = match encoding {
        PositionEncoding::Utf8 => target.min(line_text.len()),
        PositionEncoding::Utf16 => bytes_for_utf16_units(line_text, target),
        PositionEncoding::Utf32 => bytes_for_chars(line_text, target),
    };
    line_start + column_bytes
}

/// Byte offset where line `line` starts (0-based line index).
fn line_start_offset(text: &str, line: u32) -> usize {
    if line == 0 {
        return 0;
    }
    let mut seen: u32 = 0;
    // Bounded loop: at most `text.len()` iterations.
    for (i, b) in text.as_bytes().iter().enumerate() {
        if *b == b'\n' {
            seen = seen.saturating_add(1);
            if seen == line {
                return i + 1;
            }
        }
    }
    text.len()
}

/// Byte offset of the next `\n` after `start`, or `text.len()` if none.
fn next_line_offset(text: &str, start: usize) -> usize {
    let tail = &text.as_bytes()[start..];
    // Bounded loop: at most `tail.len()` iterations.
    for (off, b) in tail.iter().enumerate() {
        if *b == b'\n' {
            return start + off;
        }
    }
    text.len()
}

/// Count the UTF-16 code units required to encode `text`.
fn count_utf16_units(text: &str) -> u32 {
    let mut units: u32 = 0;
    // Bounded loop: one iteration per char.
    for ch in text.chars() {
        units = units.saturating_add(ch.len_utf16() as u32);
    }
    units
}

/// Find the byte offset inside `text` that lies after `target_units`
/// UTF-16 code units. If `target_units` is past the end, returns the
/// length of `text`.
fn bytes_for_utf16_units(text: &str, target_units: usize) -> usize {
    let mut units: usize = 0;
    let bytes = text.as_bytes();
    // Bounded loop: one iteration per char, at most `text.len()` chars.
    for (byte_idx, ch) in text.char_indices() {
        if units >= target_units {
            return byte_idx;
        }
        units = units.saturating_add(ch.len_utf16());
    }
    bytes.len()
}

/// Find the byte offset inside `text` that lies after `target_chars`
/// codepoints.
fn bytes_for_chars(text: &str, target_chars: usize) -> usize {
    let mut chars: usize = 0;
    let bytes = text.as_bytes();
    // Bounded loop: one iteration per char.
    for (byte_idx, _) in text.char_indices() {
        if chars >= target_chars {
            return byte_idx;
        }
        chars = chars.saturating_add(1);
    }
    bytes.len()
}

/// Convert an [`edda_span::Span`] to an LSP [`Range`] under `encoding`.
///
/// The `source_map` must hold the file the span refers to. `Span::DUMMY`
/// produces a zero-length range at `(0, 0)` — clients tolerate that and
/// it keeps the conversion total.
pub fn span_to_range(
    source_map: &SourceMap,
    span: Span,
    encoding: PositionEncoding,
) -> Range {
    if span.is_dummy() {
        return Range {
            start: Position { line: 0, character: 0 },
            end: Position { line: 0, character: 0 },
        };
    }
    let text = source_map.file_content(span.file);
    let start = byte_to_position(text, span.lo.to_usize(), encoding);
    let end = byte_to_position(text, span.hi.to_usize(), encoding);
    Range { start, end }
}

/// Convert an LSP [`Range`] to an [`edda_span::Span`] under `encoding`.
///
/// Reserved for the code-action / workspace-edit handlers that arrive
/// in a follow-up; not yet called by any v0.1 handler.
#[allow(dead_code)]
pub(crate) fn range_to_span(
    text: &str,
    file_id: FileId,
    range: Range,
    encoding: PositionEncoding,
) -> Span {
    let lo = position_to_byte(text, range.start, encoding);
    let hi = position_to_byte(text, range.end, encoding);
    Span::new(file_id, BytePos(lo as u32), BytePos(hi as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_round_trip_utf8() {
        let text = "abc\ndef\n";
        let pos = byte_to_position(text, 5, PositionEncoding::Utf8);
        assert_eq!(pos.line, 1);
        assert_eq!(pos.character, 1);
        let byte = position_to_byte(text, pos, PositionEncoding::Utf8);
        assert_eq!(byte, 5);
    }

    #[test]
    fn utf16_counts_surrogates_as_two_units() {
        // U+1F600 GRINNING FACE encodes to 4 UTF-8 bytes and 2 UTF-16 units.
        let text = "\u{1F600}xy";
        let pos = byte_to_position(text, 4, PositionEncoding::Utf16);
        assert_eq!(pos.character, 2); // emoji = 2 UTF-16 units
        let pos2 = byte_to_position(text, 4, PositionEncoding::Utf8);
        assert_eq!(pos2.character, 4); // 4 UTF-8 bytes
    }

    #[test]
    fn position_past_end_clamps_to_length() {
        let text = "ab";
        let pos = Position { line: 5, character: 10 };
        assert_eq!(position_to_byte(text, pos, PositionEncoding::Utf8), 2);
    }
}
