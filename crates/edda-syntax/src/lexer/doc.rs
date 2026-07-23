//! Doc-comment token lexing for the four legacy tiers (`///` / `//!` /
//! `/!!` / `!!!`) plus the shared body reader. These methods form a
//! single `impl Lexer` block split out from `mod.rs` for file-size
//! reasons; they share the [`Lexer`]'s private cursor state through the
//! descendant-module visibility rule.

use edda_intern::Symbol;

use super::Lexer;
use crate::token::Token;

impl<'a> Lexer<'a> {
    /// Lex one of `///` or `//!` (legacy `OuterDoc` / `InnerDoc`).
    pub(super) fn lex_doc_comment(&mut self) -> Token {
        self.pos += 3; // consume `///` or `//!`
        let is_inner = self.bytes[self.pos - 1] == b'!';
        let sym = self.read_doc_body();
        if is_inner {
            Token::InnerDoc(sym)
        } else {
            Token::OuterDoc(sym)
        }
    }

    /// Lex `/!!` medium-tier doc-comment line.
    pub(super) fn lex_doc_med(&mut self) -> Token {
        self.pos += 3; // consume `/!!`
        Token::DocMed(self.read_doc_body())
    }

    /// Lex `!!!` high-tier doc-comment line.
    pub(super) fn lex_doc_high(&mut self) -> Token {
        self.pos += 3; // consume `!!!`
        Token::DocHigh(self.read_doc_body())
    }

    /// Read the body of a doc-comment line: every byte up to the next `\n`,
    /// then trim a single leading space and any trailing whitespace.
    fn read_doc_body(&mut self) -> Symbol {
        let body_start = self.pos;
        while let Some(b) = self.peek() {
            if b == b'\n' {
                break;
            }
            self.advance_utf8_char();
        }
        let body = self.src[body_start..self.pos]
            .trim_start_matches(' ')
            .trim_end_matches(['\r', ' ', '\t']);
        self.interner.intern(body)
    }
}
