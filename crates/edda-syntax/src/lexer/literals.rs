//! Literal-form lexing: numbers, plain strings, escape sequences,
//! f-strings. These methods form a single `impl Lexer` block split out
//! from `mod.rs` for file-size reasons; they share the [`Lexer`]'s
//! private cursor and diagnostic state through the descendant-module
//! visibility rule.

use edda_span::Span;

use super::Lexer;
use crate::token::{IntBase, Token};

impl<'a> Lexer<'a> {
    pub(super) fn lex_number(&mut self) -> Token {
        let start = self.pos;
        let first = self.bump().unwrap();
        if first == b'0' {
            match self.peek() {
                Some(b'x' | b'X') => {
                    self.pos += 1;
                    return self.finish_based_int(start, IntBase::Hex);
                }
                Some(b'b' | b'B') => {
                    self.pos += 1;
                    return self.finish_based_int(start, IntBase::Bin);
                }
                Some(b'o' | b'O') => {
                    self.pos += 1;
                    return self.finish_based_int(start, IntBase::Oct);
                }
                _ => {}
            }
        }
        // Decimal integer or float; consume the rest of the integral part.
        self.consume_digit_run(10);
        let mut is_float = false;
        // Float fractional part: `.<digit>` (a lone `.` is the field-access op,
        // so we require a digit after the dot to commit to a float).
        if self.peek() == Some(b'.') && matches!(self.peek_at(1), Some(b'0'..=b'9')) {
            is_float = true;
            self.pos += 1;
            self.consume_digit_run(10);
        }
        // Exponent part: `[eE][+-]?<digit+>`.
        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            let exp_start = self.pos;
            self.consume_digit_run(10);
            if self.pos == exp_start {
                let span = self.make_span(start, self.pos);
                self.emit_error(span, "float literal missing exponent digits");
            }
        }
        let span = self.make_span(start, self.pos);
        let text = &self.src[start..self.pos];
        if is_float {
            return Token::Float(self.interner.intern(text));
        }
        let value = self.parse_int_text(text, IntBase::Dec, span).unwrap_or(0);
        Token::Int {
            value,
            base: IntBase::Dec,
        }
    }

    fn finish_based_int(&mut self, start: usize, base: IntBase) -> Token {
        let radix = base_radix(base);
        let digits_start = self.pos;
        self.consume_digit_run(radix);
        if self.pos == digits_start {
            let span = self.make_span(start, self.pos);
            self.emit_error(span, "numeric literal has base prefix but no digits");
            return Token::Int { value: 0, base };
        }
        let span = self.make_span(start, self.pos);
        let text = &self.src[digits_start..self.pos];
        let value = self.parse_int_text(text, base, span).unwrap_or(0);
        Token::Int { value, base }
    }

    /// Consume a run of digits valid in `radix`, plus underscore separators.
    /// Leading / trailing / consecutive underscores are accepted by the
    /// lexer and validated during numeric-value computation.
    fn consume_digit_run(&mut self, radix: u32) {
        while let Some(b) = self.peek() {
            if b == b'_' || (b as char).is_digit(radix) {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn parse_int_text(&mut self, text: &str, base: IntBase, span: Span) -> Option<u128> {
        let radix = base_radix(base);
        let mut value: u128 = 0;
        for b in text.bytes() {
            if b == b'_' {
                continue;
            }
            let Some(d) = (b as char).to_digit(radix) else {
                self.emit_error(
                    span,
                    format!("invalid digit '{}' for base {}", b as char, radix),
                );
                return None;
            };
            match value
                .checked_mul(radix as u128)
                .and_then(|v| v.checked_add(d as u128))
            {
                Some(v) => value = v,
                None => {
                    self.emit_error(span, "integer literal overflows u128");
                    return None;
                }
            }
        }
        Some(value)
    }

    pub(super) fn lex_string(&mut self) -> Token {
        if self.peek_at(1) == Some(b'"') && self.peek_at(2) == Some(b'"') {
            return self.lex_triple_string();
        }
        let start = self.pos;
        self.pos += 1; // consume opening `"`
        let mut content = String::new();
        loop {
            match self.peek() {
                Some(b'"') => {
                    self.pos += 1;
                    return Token::Str(self.interner.intern(&content));
                }
                Some(b'\\') => {
                    self.pos += 1;
                    self.lex_escape_into(&mut content);
                }
                Some(b'\n') | None => {
                    let span = self.make_span(start, self.pos);
                    self.emit_error(span, "unterminated string literal");
                    return Token::Str(self.interner.intern(&content));
                }
                Some(_) => {
                    let ch_start = self.pos;
                    self.advance_utf8_char();
                    content.push_str(&self.src[ch_start..self.pos]);
                }
            }
        }
    }

    /// Lex a `"""..."""` triple-quoted multi-line string.
    fn lex_triple_string(&mut self) -> Token {
        let start = self.pos;
        self.pos += 3; // consume opening `"""`
        let content_start = self.pos;
        let end = loop {
            match self.peek() {
                Some(b'"') if self.peek_at(1) == Some(b'"') && self.peek_at(2) == Some(b'"') => {
                    break self.pos;
                }
                Some(b'\\') => {
                    self.pos += 1;
                    if self.peek().is_some() {
                        self.advance_utf8_char();
                    }
                }
                None => {
                    let span = self.make_span(start, self.pos);
                    self.emit_error(span, "unterminated triple-quoted string literal");
                    break self.pos;
                }
                Some(_) => self.advance_utf8_char(),
            }
        };
        let content = self.cook_triple_content(content_start, end);
        // Step past the closing `"""` when one was found (not at EOF).
        self.pos = (end + 3).min(self.bytes.len());
        Token::Str(self.interner.intern(&content))
    }

    /// Replay `content_start..end` building the cooked payload: strip the
    /// leading newline after the opener, strip the closing delimiter's
    /// indentation at each line start, and process escape sequences.
    fn cook_triple_content(&mut self, content_start: usize, end: usize) -> String {
        let indent_start = match self.src[content_start..end].rfind('\n') {
            Some(rel) if self.bytes[content_start + rel + 1..end]
                .iter()
                .all(|&b| b == b' ' || b == b'\t') =>
            {
                content_start + rel + 1
            }
            _ => end,
        };
        let indent_len = end - indent_start;
        let mut content = String::new();
        self.pos = content_start;
        if self.peek() == Some(b'\r') && self.peek_at(1) == Some(b'\n') {
            self.pos += 2;
        } else if self.peek() == Some(b'\n') {
            self.pos += 1;
        }
        let mut at_line_start = true;
        while self.pos < end {
            if at_line_start
                && indent_len > 0
                && end - self.pos >= indent_len
                && self.bytes[self.pos..self.pos + indent_len]
                    == self.bytes[indent_start..end]
            {
                self.pos += indent_len;
            }
            at_line_start = false;
            if self.pos >= end {
                break;
            }
            match self.bytes[self.pos] {
                b'\\' => {
                    self.pos += 1;
                    self.lex_escape_into(&mut content);
                }
                b'\n' => {
                    self.pos += 1;
                    content.push('\n');
                    at_line_start = true;
                }
                _ => {
                    let ch_start = self.pos;
                    self.advance_utf8_char();
                    content.push_str(&self.src[ch_start..self.pos]);
                }
            }
        }
        content
    }

    fn lex_escape_into(&mut self, out: &mut String) {
        let esc_start = self.pos - 1; // position of the leading `\`
        let Some(b) = self.peek() else {
            let span = self.make_span(esc_start, self.pos);
            self.emit_error(span, "unterminated escape sequence");
            return;
        };
        self.pos += 1;
        match b {
            b'\\' => out.push('\\'),
            b'"' => out.push('"'),
            b'n' => out.push('\n'),
            b'r' => out.push('\r'),
            b't' => out.push('\t'),
            b'0' => out.push('\0'),
            b'x' => self.lex_hex_escape(out, esc_start),
            b'u' => self.lex_unicode_escape(out, esc_start),
            other => {
                let span = self.make_span(esc_start, self.pos);
                self.emit_error(
                    span,
                    format!("unknown escape sequence `\\{}`", other as char),
                );
            }
        }
    }

    fn lex_hex_escape(&mut self, out: &mut String, esc_start: usize) {
        let mut value: u32 = 0;
        for _ in 0..2 {
            let Some(b) = self.peek() else { break };
            let Some(d) = (b as char).to_digit(16) else { break };
            self.pos += 1;
            value = value * 16 + d;
        }
        let span = self.make_span(esc_start, self.pos);
        if self.pos - esc_start != 4 {
            self.emit_error(span, "`\\x` escape requires exactly two hex digits");
            return;
        }
        if value > 0x7F {
            self.emit_error(span, "`\\x` escape must be in range 0x00..=0x7F");
            return;
        }
        out.push(value as u8 as char);
    }

    fn lex_unicode_escape(&mut self, out: &mut String, esc_start: usize) {
        if self.peek() != Some(b'{') {
            let span = self.make_span(esc_start, self.pos);
            self.emit_error(span, "`\\u` escape must be followed by `{`");
            return;
        }
        self.pos += 1;
        let mut value: u32 = 0;
        let mut digits = 0;
        while let Some(b) = self.peek() {
            let Some(d) = (b as char).to_digit(16) else { break };
            self.pos += 1;
            digits += 1;
            value = value.saturating_mul(16).saturating_add(d);
            if digits > 6 {
                break;
            }
        }
        if self.peek() != Some(b'}') {
            let span = self.make_span(esc_start, self.pos);
            self.emit_error(span, "`\\u{...}` escape missing closing `}`");
            return;
        }
        self.pos += 1;
        let span = self.make_span(esc_start, self.pos);
        if digits == 0 || digits > 6 {
            self.emit_error(span, "`\\u{...}` escape requires 1 to 6 hex digits");
            return;
        }
        let Some(ch) = char::from_u32(value) else {
            self.emit_error(span, "`\\u{...}` escape is not a valid Unicode scalar");
            return;
        };
        out.push(ch);
    }

    /// Lex an `f"..."` interpolated string as a single token whose payload
    /// is the raw bytes between the quotes (escapes preserved). Structured
    /// slot lexing happens in a later wave at parse time.
    pub(super) fn lex_fstring(&mut self) -> Token {
        let start = self.pos;
        self.pos += 2; // consume `f"`
        let content_start = self.pos;
        let mut slot_depth: u32 = 0;
        loop {
            match self.peek() {
                Some(b'\\') => {
                    // Preserve the backslash and the next byte verbatim.
                    self.pos += 1;
                    if self.peek().is_some() {
                        self.advance_utf8_char();
                    }
                }
                Some(b'{') => {
                    slot_depth += 1;
                    self.pos += 1;
                }
                Some(b'}') => {
                    slot_depth = slot_depth.saturating_sub(1);
                    self.pos += 1;
                }
                Some(b'"') if slot_depth == 0 => {
                    let text = &self.src[content_start..self.pos];
                    let sym = self.interner.intern(text);
                    self.pos += 1;
                    return Token::FString(sym);
                }
                Some(b'\n') | None => {
                    let span = self.make_span(start, self.pos);
                    self.emit_error(span, "unterminated f-string");
                    let text = &self.src[content_start..self.pos];
                    return Token::FString(self.interner.intern(text));
                }
                Some(_) => {
                    self.advance_utf8_char();
                }
            }
        }
    }
}

#[inline]
fn base_radix(base: IntBase) -> u32 {
    match base {
        IntBase::Dec => 10,
        IntBase::Hex => 16,
        IntBase::Bin => 2,
        IntBase::Oct => 8,
    }
}
