//! Hand-rolled state-machine lexer for Edda.
//!
//! Single-pass over the source bytes; UTF-8 is preserved verbatim inside
//! string literals and comments but every syntactically-significant byte
//! is ASCII. The lexer emits diagnostics into a [`Diagnostics`] take and
//! always makes forward progress — invalid bytes are skipped with a
//! diagnostic so a malformed input still terminates.
//!
//! Literal-form scanning (numbers, strings, f-strings, escape sequences)
//! lives in the child [`literals`] module to keep this file under the
//! 500-line cap; the methods there form a single `impl Lexer` block.

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_intern::Interner;
use edda_span::{BytePos, FileId, Span};

use crate::token::{Lexed, Token, keyword_token};

mod comments;
mod doc;
mod literals;
mod operators;

#[cfg(test)]
mod tests;

/// Lex `src` (the content of file `file`) into a token stream terminated
/// by [`Token::Eof`]. Lexical errors are reported via `diags` under the
/// [`DiagnosticClass::ParseError`] class.
///
/// This is the source-lexing entry point and rejects every comment form
/// (`//`, `/* */`, and the legacy doc tiers `///` `//!` `/!!` `!!!`) with
/// `error[comment_not_admitted]` per the V1.0 no-comment design lock —
/// `.ea` source admits no comments. Sterility is the default and only mode here; the
/// `comment_spans` / `strip_comments` migration entry points below run in
/// record mode instead so they can still locate comments to strip.
/// Generated codegen artifacts carry a `// @generated` header block; the
/// driver strips it before this lexer runs (the cache-header boundary), so
/// re-lexing the compiler's own output stays comment-free.
pub fn lex(
    src: &str,
    file: FileId,
    interner: &Interner,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Vec<Lexed> {
    let mut lexer = Lexer::new(src, file, interner, diags, lint_cfg);
    lexer.reject_comments = true;
    lexer.run()
}

/// Tokenize `src` and return the byte-[`Span`]s of every comment (all six
/// forms: `//`, `///`, `//!`, `/!!`, `!!!`, `/* */`), for the
/// `edda fmt --strip-comments` migration. Comment rejection is OFF in this
/// mode, so no `comment_not_admitted` fires; only genuine lexical errors
/// (e.g. an unterminated block) emit through `diags`.
pub fn comment_spans(
    src: &str,
    file: FileId,
    interner: &Interner,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Vec<Span> {
    let mut lexer = Lexer::new(src, file, interner, diags, lint_cfg);
    lexer.record_comment_spans = true;
    lexer.run_collecting().1
}

/// Return `src` with every comment removed — the strip backing
/// `edda fmt --strip-comments`. Comment byte-ranges come
/// from [`comment_spans`] (the real tokenizer), so a `//` inside a string
/// or f-string literal is never touched. Genuine lexical errors (e.g. an
/// unterminated block comment) surface through `diags`.
pub fn strip_comments(
    src: &str,
    file: FileId,
    interner: &Interner,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> String {
    let mut spans = comment_spans(src, file, interner, diags, lint_cfg);
    spans.sort_by(|a, b| b.lo.0.cmp(&a.lo.0));
    let mut out = src.to_string();
    for span in spans {
        let lo = span.lo.0 as usize;
        let hi = span.hi.0 as usize;
        if lo <= hi && hi <= out.len() && out.is_char_boundary(lo) && out.is_char_boundary(hi) {
            out.replace_range(lo..hi, "");
        }
    }
    out
}

/// Stateful lexer over a single source file. Construct with [`Lexer::new`]
/// and call [`Lexer::run`] to consume the stream.
pub struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    file: FileId,
    interner: &'a Interner,
    diags: &'a mut Diagnostics,
    lint_cfg: &'a LintConfig,
    /// Comment enforcement (the V1.0 no-comment design lock). When `true` every comment form
    /// (`//`, `/* */`, and the legacy doc tiers `///` `//!` `/!!` `!!!`)
    /// emits `comment_not_admitted` under the dedicated
    /// [`DiagnosticClass::CommentNotAdmitted`] class. The source-lexing
    /// entry point [`lex`] sets this `true` — sterility is the default and
    /// only mode for `.ea` source. [`Lexer::new`] leaves it `false` so the
    /// `comment_spans` / `strip_comments` migration entry points can run in
    /// record mode. Generated codegen artifacts carry a `// @generated`
    /// header that the driver strips before lexing, so re-lexing the
    /// compiler's own output stays comment-free.
    reject_comments: bool,
    /// When `true` the lexer records every comment's byte-[`Span`] into
    /// `comment_spans` (and consumes it) instead of producing doc tokens /
    /// skipping trivia. Drives the strip migration via [`comment_spans`].
    record_comment_spans: bool,
    /// Recorded comment spans (record mode only).
    comment_spans: Vec<Span>,
}

impl<'a> Lexer<'a> {
    /// Construct a lexer over `src`. The caller owns the [`Interner`] and
    /// the [`Diagnostics`] take so the daemon can reuse them across files.
    pub fn new(
        src: &'a str,
        file: FileId,
        interner: &'a Interner,
        diags: &'a mut Diagnostics,
        lint_cfg: &'a LintConfig,
    ) -> Self {
        Lexer {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            file,
            interner,
            diags,
            lint_cfg,
            reject_comments: false,
            record_comment_spans: false,
            comment_spans: Vec::new(),
        }
    }

    /// Drive the state machine to end-of-input, returning every lexed
    /// token in source order with a trailing `Eof`.
    pub fn run(self) -> Vec<Lexed> {
        self.run_collecting().0
    }

    /// Drive the state machine to end-of-input, returning every lexed
    /// token (trailing `Eof`) plus the recorded comment spans (empty in
    /// the default rejecting mode). [`run`](Self::run) wraps this and
    /// discards the spans; [`comment_spans`] keeps them.
    fn run_collecting(mut self) -> (Vec<Lexed>, Vec<Span>) {
        let mut tokens = Vec::new();
        loop {
            let leading_newline = self.skip_trivia();
            let start = self.pos;
            let Some(tok) = self.next_token() else { break };
            tokens.push(Lexed {
                kind: tok,
                span: self.make_span(start, self.pos),
                leading_newline,
            });
        }
        tokens.push(Lexed {
            kind: Token::Eof,
            span: self.make_span(self.pos, self.pos),
            leading_newline: false,
        });
        (tokens, self.comment_spans)
    }

    fn make_span(&self, lo: usize, hi: usize) -> Span {
        Span::new(self.file, BytePos(lo as u32), BytePos(hi as u32))
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek()?;
        self.pos += 1;
        Some(b)
    }

    fn emit_error(&mut self, span: Span, msg: impl Into<String>) {
        let sev = self.lint_cfg.effective(DiagnosticClass::ParseError);
        self.diags
            .push(Diagnostic::new(DiagnosticClass::ParseError, sev, span, msg));
    }

    fn next_token(&mut self) -> Option<Token> {
        let b = self.peek()?;
        match b {
            // f-string prefix (more specific than ident)
            b'f' if self.peek_at(1) == Some(b'"') => Some(self.lex_fstring()),
            // `///` outer doc or `//!` inner doc
            b'/' if self.peek_at(1) == Some(b'/')
                && matches!(self.peek_at(2), Some(b'/' | b'!')) =>
            {
                Some(self.lex_doc_comment())
            }
            // `/!!` medium-tier doc (codex `/!!`)
            b'/' if self.peek_at(1) == Some(b'!') && self.peek_at(2) == Some(b'!') => {
                Some(self.lex_doc_med())
            }
            // `!!!` high-tier doc (codex `!!!`)
            b'!' if self.peek_at(1) == Some(b'!') && self.peek_at(2) == Some(b'!') => {
                Some(self.lex_doc_high())
            }
            // Identifiers, keywords, underscore
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => Some(self.lex_ident_or_underscore()),
            // Numbers
            b'0'..=b'9' => Some(self.lex_number()),
            // Plain string
            b'"' => Some(self.lex_string()),
            _ => self.lex_punct_or_op(),
        }
    }

    fn lex_ident_or_underscore(&mut self) -> Token {
        let start = self.pos;
        self.pos += 1; // first byte already validated as alpha or `_`
        while let Some(b) = self.peek() {
            if b.is_ascii_alphanumeric() || b == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let text = &self.src[start..self.pos];
        if text == "_" {
            return Token::Underscore;
        }
        if let Some(kw) = keyword_token(text) {
            return kw;
        }
        Token::Ident(self.interner.intern(text))
    }

    /// Advance `pos` by one UTF-8 character. The bytes are validated by
    /// `&str`'s invariant; we trust UTF8_CHAR_WIDTH-style probing.
    fn advance_utf8_char(&mut self) {
        let b = self.bytes[self.pos];
        let width = if b < 0x80 {
            1
        } else if b < 0xC0 {
            // Stray continuation byte. Treat as a 1-byte advance; the
            // caller's diagnostic context will surface it as garbage.
            1
        } else if b < 0xE0 {
            2
        } else if b < 0xF0 {
            3
        } else {
            4
        };
        self.pos = (self.pos + width).min(self.bytes.len());
    }

}
