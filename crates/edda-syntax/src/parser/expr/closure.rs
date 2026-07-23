//! Closure-literal parsing: the `function(params) -> ret [with {row}]
//! [captures {caps}] { body }` form, its parameter list, and the capture
//! clause. Split out from the Pratt core for file-size reasons; every
//! method extends the same `impl Parser` block.

use crate::ast::{Capture, CaptureMode, Closure, Expr, ExprKind, Param, ParamMode};
use crate::token::Token;

use super::super::Parser;

impl<'a> Parser<'a> {
    pub(super) fn parse_closure_literal(&mut self) -> Expr {
        let start = self.pos;
        self.bump(); // `function`
        self.expect(Token::LParen, "expected `(` in closure parameter list");
        let mut params = Vec::new();
        if !self.at(Token::RParen) {
            params.push(self.parse_closure_param());
            while self.eat(Token::Comma) {
                if self.at(Token::RParen) {
                    break;
                }
                params.push(self.parse_closure_param());
            }
        }
        self.expect(Token::RParen, "expected `)` after closure parameters");
        self.expect(
            Token::Arrow,
            "expected `->` — closure return type is mandatory",
        );
        let ret = self.parse_ty_no_where();
        let effects = if self.at(Token::With) {
            Some(self.parse_effect_row())
        } else {
            None
        };
        let captures = if self.at_captures_keyword() {
            Some(self.parse_closure_captures())
        } else {
            None
        };
        let body = self.parse_block();
        let span = self.span_from(start);
        Expr {
            span,
            kind: ExprKind::Closure(Box::new(Closure {
                span,
                params,
                ret,
                effects,
                captures,
                body,
            })),
        }
    }

    fn parse_closure_param(&mut self) -> Param {
        let start = self.pos;
        let name = self.expect_ident("expected closure parameter name");
        self.expect(
            Token::Colon,
            "expected `:` after closure parameter name",
        );
        let mode = match self.peek_kind() {
            Token::Mutable => {
                self.bump();
                ParamMode::Mutable
            }
            Token::Take => {
                self.bump();
                ParamMode::Take
            }
            Token::Init => {
                self.bump();
                ParamMode::Init
            }
            _ => ParamMode::Default,
        };
        let ty = self.parse_ty_no_where();
        Param {
            span: self.span_from(start),
            name,
            mode,
            ty,
        }
    }

    fn parse_closure_captures(&mut self) -> Vec<Capture> {
        self.bump(); // `captures` (matched as a contextual identifier)
        self.expect(Token::LBrace, "expected `{` after `captures`");
        let mut entries = Vec::new();
        if !self.at(Token::RBrace) {
            entries.push(self.parse_capture_entry());
            while self.eat(Token::Comma) {
                if self.at(Token::RBrace) {
                    break;
                }
                entries.push(self.parse_capture_entry());
            }
        }
        self.expect(Token::RBrace, "expected `}` to close `captures`");
        entries
    }

    fn parse_capture_entry(&mut self) -> Capture {
        let start = self.pos;
        let name = self.expect_ident("expected captured binding name");
        let mode = if self.eat(Token::Colon) {
            match self.peek_kind() {
                Token::Take => {
                    self.bump();
                    CaptureMode::Take
                }
                Token::Let => {
                    self.bump();
                    CaptureMode::Let
                }
                Token::Mutable => {
                    let span = self.peek().span;
                    self.emit_error(
                        span,
                        "`mutable` captures are forbidden — mutation must be visible at the call site (phase-2-locks Gap 1 §Capture semantics)",
                    );
                    self.bump();
                    CaptureMode::Let
                }
                _ => {
                    let span = self.peek().span;
                    self.emit_error(
                        span,
                        "expected `let` or `take` after `:` in capture entry",
                    );
                    CaptureMode::Let
                }
            }
        } else {
            CaptureMode::Let
        };
        Capture {
            span: self.span_from(start),
            name,
            mode,
        }
    }

    /// True when the current token is the contextual `captures` keyword.
    fn at_captures_keyword(&self) -> bool {
        if let Token::Ident(sym) = self.peek_kind() {
            self.interner.resolve(sym) == "captures"
        } else {
            false
        }
    }
}
