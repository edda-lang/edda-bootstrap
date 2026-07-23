//! Punctuation and operator lexing: the three-, two-, and one-byte
//! operator tables and the unexpected-character recovery path. These
//! methods form a single `impl Lexer` block split out from `mod.rs` for
//! file-size reasons; they share the [`Lexer`]'s private cursor state
//! through the descendant-module visibility rule.

use super::Lexer;
use crate::token::Token;

impl<'a> Lexer<'a> {
    pub(super) fn lex_punct_or_op(&mut self) -> Option<Token> {
        let b = self.peek()?;
        if let Some(tok) = self.try_match3() {
            return Some(tok);
        }
        if let Some(tok) = self.try_match2() {
            return Some(tok);
        }
        let tok = match b {
            b'(' => Token::LParen,
            b')' => Token::RParen,
            b'[' => Token::LBracket,
            b']' => Token::RBracket,
            b'{' => Token::LBrace,
            b'}' => Token::RBrace,
            b',' => Token::Comma,
            b'.' => Token::Dot,
            b':' => Token::Colon,
            b';' => Token::Semicolon,
            b'+' => Token::Plus,
            b'-' => Token::Minus,
            b'*' => Token::Star,
            b'/' => Token::Slash,
            b'%' => Token::Percent,
            b'<' => Token::Lt,
            b'>' => Token::Gt,
            b'=' => Token::Eq,
            b'!' => Token::Bang,
            b'&' => Token::Amp,
            b'|' => Token::Pipe,
            b'^' => Token::Caret,
            b'~' => Token::Tilde,
            b'?' => Token::Question,
            b'@' => Token::At,
            _ => {
                let ch_start = self.pos;
                self.advance_utf8_char();
                let span = self.make_span(ch_start, self.pos);
                let bad = &self.src[ch_start..self.pos];
                self.emit_error(span, format!("unexpected character `{}`", bad));
                return self.next_token();
            }
        };
        self.pos += 1;
        Some(tok)
    }

    fn try_match3(&mut self) -> Option<Token> {
        let a = self.peek()?;
        let b = self.peek_at(1)?;
        let c = self.peek_at(2)?;
        let tok = match (a, b, c) {
            (b'.', b'.', b'<') => Token::DotDotLt,
            (b'.', b'.', b'=') => Token::DotDotEq,
            (b'.', b'.', b'.') => Token::DotDotDot,
            (b'<', b'<', b'=') => Token::LtLtEq,
            (b'>', b'>', b'=') => Token::GtGtEq,
            _ => return None,
        };
        self.pos += 3;
        Some(tok)
    }

    fn try_match2(&mut self) -> Option<Token> {
        let a = self.peek()?;
        let b = self.peek_at(1)?;
        let tok = match (a, b) {
            (b'-', b'>') => Token::Arrow,
            (b'=', b'>') => Token::FatArrow,
            (b'=', b'=') => Token::EqEq,
            (b'!', b'=') => Token::BangEq,
            (b'<', b'=') => Token::LtEq,
            (b'>', b'=') => Token::GtEq,
            (b'<', b'<') => Token::LtLt,
            (b'>', b'>') => Token::GtGt,
            (b'&', b'&') => Token::AmpAmp,
            (b'|', b'|') => Token::PipePipe,
            (b'+', b'=') => Token::PlusEq,
            (b'-', b'=') => Token::MinusEq,
            (b'*', b'=') => Token::StarEq,
            (b'/', b'=') => Token::SlashEq,
            (b'%', b'=') => Token::PercentEq,
            // Wrapping-arithmetic explicit-mode operators per `spec-sweep-locks.md` S1.
            (b'+', b'%') => Token::PlusPct,
            (b'-', b'%') => Token::MinusPct,
            (b'*', b'%') => Token::StarPct,
            // Checked-arithmetic explicit-mode operators per `spec-sweep-locks.md` S1.
            (b'+', b'?') => Token::PlusQuestion,
            (b'-', b'?') => Token::MinusQuestion,
            (b'*', b'?') => Token::StarQuestion,
            (b'%', b'?') => Token::PercentQuestion,
            // Saturating-arithmetic explicit-mode operators per CLAUDE.md §"Numeric operators".
            (b'+', b'|') => Token::PlusPipe,
            (b'-', b'|') => Token::MinusPipe,
            (b'*', b'|') => Token::StarPipe,
            (b'&', b'=') => Token::AmpEq,
            (b'|', b'=') => Token::PipeEq,
            (b'^', b'=') => Token::CaretEq,
            (b'.', b'.') => Token::DotDot,
            _ => return None,
        };
        self.pos += 2;
        Some(tok)
    }
}
