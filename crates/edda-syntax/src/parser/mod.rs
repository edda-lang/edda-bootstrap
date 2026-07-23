//! Recursive-descent + Pratt parser for the Edda language surface.
//!
//! Covers the expression-level parser:
//!   - all locked operators with their `expressions.md` §"Operator
//!     precedence" precedence and associativity;
//!   - control-flow expressions (`if`, `match`, `loop`, `for`,
//!     `block`, `comptime`, `scope`);
//!   - statements (`let`, `var`, assignment, expression statement);
//!   - types and patterns to support `as`, `match`-arm heads, and
//!     `let`/`for` binding forms.
//!
//! Also covers top-level items (`function`, `type`, `spec`, `import`,
//! `module`) and the file-level entry point.
//!
//! # Error recovery
//!
//! Lexical errors are already reported by the lexer; parse errors emit a
//! [`DiagnosticClass::ParseError`] and advance to a synchronisation point
//! (statement terminator, closing brace, EOF). Unparseable subtrees yield
//! `*Kind::Error` AST sentinels; downstream passes treat them as
//! already-diagnosed and do not double-report.

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_intern::Interner;
use edda_span::Span;

use crate::ast::{Block, Expr, ExprKind, Ident};
use crate::token::{Lexed, Token};

mod attr;
mod control;
mod expr;
mod function;
mod item;
mod misc_items;
mod type_decl;
#[cfg(test)]
mod item_tests;
mod op_guardrail;
mod pat;
mod spec;
mod stmt;
#[cfg(test)]
mod tests;
mod ty;

pub use item::parse_file;

/// Parse a single expression from `tokens`. Used for testing and for
/// REPL-style inputs; the file-level entry is [`parse_file`].
pub fn parse_expr(
    tokens: &[Lexed],
    interner: &Interner,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Expr {
    let mut parser = Parser::new(tokens, interner, diags, lint_cfg);
    let expr = parser.parse_expr();
    if !parser.at_eof() {
        let span = parser.peek().span;
        parser.emit_error(span, "expected end of input after expression");
    }
    expr
}

/// Parse a single `{ ... }` block from `tokens`. The leading `Token::LBrace`
/// must be at position 0.
pub fn parse_block(
    tokens: &[Lexed],
    interner: &Interner,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Block {
    let mut parser = Parser::new(tokens, interner, diags, lint_cfg);
    parser.parse_block()
}

/// Stateful parser over a token stream. Construct with [`Parser::new`]
/// and call one of the `parse_*` entry methods.
pub struct Parser<'a> {
    tokens: &'a [Lexed],
    pos: usize,
    interner: &'a Interner,
    diags: &'a mut Diagnostics,
    lint_cfg: &'a LintConfig,
}

impl<'a> Parser<'a> {
    /// Construct a parser over `tokens`. The slice must end in `Token::Eof`
    /// (the lexer always emits this); panics in debug if not.
    pub fn new(
        tokens: &'a [Lexed],
        interner: &'a Interner,
        diags: &'a mut Diagnostics,
        lint_cfg: &'a LintConfig,
    ) -> Self {
        debug_assert!(
            tokens.last().map(|t| t.kind) == Some(Token::Eof),
            "edda-syntax: parser token stream must end in Token::Eof"
        );
        Parser {
            tokens,
            pos: 0,
            interner,
            diags,
            lint_cfg,
        }
    }

    // === Cursor primitives ===

    pub(crate) fn peek(&self) -> Lexed {
        self.tokens[self.pos]
    }

    pub(crate) fn peek_at(&self, offset: usize) -> Lexed {
        let i = (self.pos + offset).min(self.tokens.len() - 1);
        self.tokens[i]
    }

    pub(crate) fn peek_kind(&self) -> Token {
        self.peek().kind
    }

    pub(crate) fn bump(&mut self) -> Lexed {
        let lex = self.tokens[self.pos];
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        lex
    }

    pub(crate) fn at(&self, kind: Token) -> bool {
        std::mem::discriminant(&self.peek_kind()) == std::mem::discriminant(&kind)
    }

    pub(crate) fn eat(&mut self, kind: Token) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    pub(crate) fn at_eof(&self) -> bool {
        self.peek_kind() == Token::Eof
    }

    /// Consume `kind` or emit an error citing `what` (e.g. `"expected `}`"`).
    pub(crate) fn expect(&mut self, kind: Token, what: &str) {
        if self.at(kind) {
            self.bump();
        } else {
            let span = self.peek().span;
            self.emit_error(span, what.to_string());
        }
    }

    // declaration-position uses (function name, parameter, type/field name)
    // round-trip cleanly. The suffix form `expr.await` is unaffected because
    // `parse_postfix` checks `Token::Await` after `.` before any identifier
    // consumption, and the lexer continues to emit `Token::Await`.
    // identifier when the locked grammar only treats it as a keyword in
    // postfix position (mirrors the `panic`/`yield` handling in
    // `parse_effect_member_name` and the `result` handling in `parse_primary`)
    /// Pull an identifier off the cursor. Emits an error and returns
    /// `None` if the current token is not an identifier. `await` is admitted
    /// because the locked surface only treats it as a keyword in the postfix
    /// position `expr.await`; in declaration position (function name,
    /// parameter, type/field name) it remains a plain identifier.
    pub(crate) fn eat_ident(&mut self) -> Option<Ident> {
        let lex = self.peek();
        let name = match lex.kind {
            Token::Ident(name) => name,
            Token::Await => self.interner.intern("await"),
            _ => return None,
        };
        self.bump();
        Some(Ident {
            name,
            span: lex.span,
        })
    }

    /// Like [`Self::eat_ident`] but emits an error when the current token
    /// is not an identifier. Returns a recovery `Ident` so callers can
    /// continue building an AST.
    pub(crate) fn expect_ident(&mut self, what: &str) -> Ident {
        if let Some(id) = self.eat_ident() {
            return id;
        }
        let span = self.peek().span;
        self.emit_error(span, what.to_string());
        Ident {
            name: edda_intern::Symbol::DUMMY,
            span,
        }
    }

    #[allow(dead_code)] // used by item parsing, not yet wired up
    pub(crate) fn span_of(&self, lex: Lexed) -> Span {
        lex.span
    }

    /// Build a span from the token at `start_pos` to the token most
    /// recently consumed (`pos - 1`). When no tokens have been consumed
    /// since `start_pos`, falls back to the start token's span.
    pub(crate) fn span_from(&self, start_pos: usize) -> Span {
        let start = self.tokens[start_pos].span;
        let end_pos = if self.pos == start_pos {
            start_pos
        } else {
            self.pos - 1
        };
        let end = self.tokens[end_pos].span;
        Span::new(start.file, start.lo, end.hi)
    }

    /// Emit a `ParseError`-class diagnostic.
    pub(crate) fn emit_error(&mut self, span: Span, msg: impl Into<String>) {
        let sev = self.lint_cfg.effective(DiagnosticClass::ParseError);
        self.diags
            .push(Diagnostic::new(DiagnosticClass::ParseError, sev, span, msg));
    }

    /// Emit a diagnostic with an explicit class. Used for parse-layer
    /// rejections that are spec-classified as something other than
    /// `parse_error` (e.g. graded-effect mixing → `effect_graded_bound_exceeded`).
    pub(crate) fn emit_class(
        &mut self,
        class: DiagnosticClass,
        span: Span,
        msg: impl Into<String>,
    ) {
        let sev = self.lint_cfg.effective(class);
        self.diags.push(Diagnostic::new(class, sev, span, msg));
    }

    /// Skip leading doc-comment tokens. Doc-comments attach to
    /// top-level items only; encountering one inside an expression is
    /// unusual but not fatal — we treat it as trivia for now.
    pub(crate) fn skip_doc_trivia(&mut self) {
        while matches!(
            self.peek_kind(),
            Token::OuterDoc(_) | Token::InnerDoc(_) | Token::DocHigh(_) | Token::DocMed(_)
        ) {
            self.bump();
        }
    }

    /// Advance past tokens until one of the `sync` set is seen or EOF is
    /// reached. The sync token itself is not consumed.
    #[allow(dead_code)] // reserved for item-level error recovery, not yet wired up
    pub(crate) fn recover_to(&mut self, sync: &[Token]) {
        while !self.at_eof() {
            let k = self.peek_kind();
            if sync.iter().any(|s| std::mem::discriminant(s) == std::mem::discriminant(&k)) {
                return;
            }
            self.bump();
        }
    }

    /// Construct an `ExprKind::Error` placeholder spanning `span`. Caller
    /// is expected to have already emitted a diagnostic.
    pub(crate) fn err_expr(&self, span: Span) -> Expr {
        Expr {
            span,
            kind: ExprKind::Error,
        }
    }
}
