//! Comment and trivia handling: whitespace / line- and block-comment
//! skipping, and the comment-rejection / span-recording paths for the
//! V1.0 no-comment design lock.
//! These methods form a single `impl Lexer` block split out from `mod.rs`
//! for file-size reasons; they share the [`Lexer`]'s private cursor and
//! diagnostic state through the descendant-module visibility rule.

use edda_diag::{Diagnostic, DiagnosticClass};

use super::Lexer;

impl<'a> Lexer<'a> {
    /// Consume bytes up to (not including) the next `\n` or EOF — swallows
    /// a line-comment / doc-comment body before [`reject_comment`] reports it.
    fn consume_line(&mut self) {
        while let Some(b) = self.peek() {
            if b == b'\n' {
                break;
            }
            self.advance_utf8_char();
        }
    }

    /// `true` when the lexer is in either non-default comment mode —
    /// rejecting (emit `comment_not_admitted`) or recording (collect spans).
    /// In both, every comment form is consumed in [`skip_trivia`] rather
    /// than producing a doc token / silent trivia.
    fn handle_comments(&self) -> bool {
        self.reject_comments || self.record_comment_spans
    }

    /// Handle a consumed comment spanning `start..self.pos`: emit
    /// `comment_not_admitted` (reject mode) or record the span (record
    /// mode). Per the V1.0 no-comment design lock, Edda admits no comments.
    fn reject_comment(&mut self, start: usize) {
        let span = self.make_span(start, self.pos);
        if self.reject_comments {
            self.emit_comment_not_admitted(span);
        } else {
            self.comment_spans.push(span);
        }
    }

    /// Push the `comment_not_admitted` diagnostic under the dedicated
    /// [`DiagnosticClass::CommentNotAdmitted`] class (the D-18 / V1.0
    /// no-comment design lock); the message states the rule and where
    /// each comment-function relocates.
    fn emit_comment_not_admitted(&mut self, span: edda_span::Span) {
        let sev = self.lint_cfg.effective(DiagnosticClass::CommentNotAdmitted);
        self.diags.push(Diagnostic::new(
            DiagnosticClass::CommentNotAdmitted,
            sev,
            span,
            "Edda source admits no comments (V1.0 design-lock). \
             Put claims in effect rows / refinements / \
             attributes / the tracker; descriptions are derived into the structure map; \
             rationale lives in a dated decision record",
        ));
    }

    /// Skip whitespace and plain (non-doc) comments. Doc-comments are
    /// tokens, not trivia, so they are left untouched for `next_token`.
    /// Returns `true` when at least one `\n` byte was consumed (either as
    /// raw whitespace or inside a `/* */` block comment); callers thread
    /// the bit into the next emitted token's `leading_newline` field per
    /// `Lexed`'s invariant.
    pub(super) fn skip_trivia(&mut self) -> bool {
        let mut saw_newline = false;
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r') => {
                    self.pos += 1;
                }
                Some(b'\n') => {
                    saw_newline = true;
                    self.pos += 1;
                }
                Some(b'/') => match self.peek_at(1) {
                    Some(b'/') => {
                        if self.handle_comments() {
                            // `//`, `///`, `//!` — rejected / recorded per the
                            // V1.0 no-comment design lock.
                            let start = self.pos;
                            self.consume_line();
                            self.reject_comment(start);
                        } else if matches!(self.peek_at(2), Some(b'/' | b'!')) {
                            return saw_newline; // doc-comment; next_token emits the token
                        } else {
                            self.consume_line(); // plain `//` line comment — legacy trivia
                        }
                    }
                    Some(b'*') => {
                        let start = self.pos;
                        if self.skip_block_comment() {
                            saw_newline = true;
                        }
                        if self.handle_comments() {
                            self.reject_comment(start);
                        }
                    }
                    // `/!!` medium-tier doc.
                    Some(b'!') if self.peek_at(2) == Some(b'!') => {
                        if self.handle_comments() {
                            let start = self.pos;
                            self.consume_line();
                            self.reject_comment(start);
                        } else {
                            return saw_newline; // next_token emits DocMed
                        }
                    }
                    // bare `/` (Slash) or non-doc `/!` — operator; next_token.
                    _ => return saw_newline,
                },
                // `!!!` high-tier doc. A bare `!` / `!=` / `!!` is an
                // operator and falls through to next_token.
                Some(b'!')
                    if self.peek_at(1) == Some(b'!') && self.peek_at(2) == Some(b'!') =>
                {
                    if self.handle_comments() {
                        let start = self.pos;
                        self.consume_line();
                        self.reject_comment(start);
                    } else {
                        return saw_newline; // next_token emits DocHigh
                    }
                }
                _ => return saw_newline,
            }
        }
    }

    /// Consume a `/* ... */` block comment (nested forms supported).
    /// Returns `true` if at least one `\n` byte was inside the comment
    /// so `skip_trivia` can propagate the bit into the next token's
    /// `leading_newline` field.
    fn skip_block_comment(&mut self) -> bool {
        let start = self.pos;
        self.pos += 2; // consume `/*`
        let mut depth: u32 = 1;
        let mut saw_newline = false;
        while depth > 0 {
            match (self.peek(), self.peek_at(1)) {
                (Some(b'/'), Some(b'*')) => {
                    self.pos += 2;
                    depth += 1;
                }
                (Some(b'*'), Some(b'/')) => {
                    self.pos += 2;
                    depth -= 1;
                }
                (Some(b), _) => {
                    if b == b'\n' {
                        saw_newline = true;
                    }
                    self.pos += 1;
                }
                (None, _) => {
                    let span = self.make_span(start, self.pos);
                    self.emit_error(span, "unterminated block comment");
                    return saw_newline;
                }
            }
        }
        saw_newline
    }
}
