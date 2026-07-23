//! Postfix-operator parsing: the `.field` / `.method(...)` / `[idx]` /
//! `?` / `.await` / `.(comptime-index)` chain, tuple positional access,
//! and the call-argument list. Split out from the Pratt core for
//! file-size reasons; every method extends the same `impl Parser` block.

use edda_span::Span;

use crate::ast::{CallArg, CallMode, Expr, ExprKind, Ident};
use crate::token::{IntBase, Token};

use super::super::Parser;

impl<'a> Parser<'a> {
    pub(super) fn parse_postfix(&mut self, mut lhs: Expr) -> Expr {
        loop {
            // Postfix tokens that could plausibly start a new
            // statement / expression on their own line must not be
            // greedily glued to the receiver. Currently `(`, `[`, and `.`
            // — any of which can also lead an expression
            // (parenthesised / tuple, slice literal, leading-dot variant
            // shorthand). `?` is excluded — it is never a leading token.
            let next = self.peek();
            if next.leading_newline
                && matches!(next.kind, Token::LParen | Token::LBracket | Token::Dot)
            {
                return lhs;
            }
            match self.peek_kind() {
                Token::Dot => {
                    self.bump();
                    if self.peek_kind() == Token::Await {
                        let aw = self.bump();
                        let span = lhs.span.join(aw.span);
                        lhs = Expr {
                            span,
                            kind: ExprKind::Await(Box::new(lhs)),
                        };
                        continue;
                    }
                    // Comptime-indexed field access: `.(<expr>)` (D-22).
                    // The parenthesised expression is a comptime-evaluable
                    // field index; the pass-2 comptime expansion rewrites
                    // this to a plain `Field` once the receiver's type is
                    // concrete. Distinct from `.<int>` tuple access (the
                    // index here is an arbitrary comptime expression, not a
                    // literal) and from a method call `.name(...)`.
                    if self.peek_kind() == Token::LParen {
                        self.bump();
                        let index = self.parse_expr();
                        let close = self.peek().span;
                        self.expect(Token::RParen, "expected `)` after comptime field index");
                        let span = Span::new(lhs.span.file, lhs.span.lo, close.hi);
                        lhs = Expr {
                            span,
                            kind: ExprKind::CompField {
                                receiver: Box::new(lhs),
                                index: Box::new(index),
                            },
                        };
                        continue;
                    }
                    // Tuple positional access: `.<integer>`. The integer
                    // must be a base-10 literal whose value fits a `u32`.
                    if let Token::Int { value, base } = self.peek_kind() {
                        lhs = self.finish_tuple_index(lhs, value, base);
                        continue;
                    }
                    let name = self.expect_ident("expected field or method name after `.`");
                    if self.at(Token::LParen) {
                        self.bump();
                        let args = self.parse_call_args();
                        let close = self.peek().span;
                        self.expect(Token::RParen, "expected `)` after method arguments");
                        let span = Span::new(lhs.span.file, lhs.span.lo, close.hi);
                        lhs = Expr {
                            span,
                            kind: ExprKind::MethodCall {
                                receiver: Box::new(lhs),
                                name,
                                args,
                            },
                        };
                    } else {
                        let span = lhs.span.join(name.span);
                        lhs = Expr {
                            span,
                            kind: ExprKind::Field {
                                receiver: Box::new(lhs),
                                name,
                            },
                        };
                    }
                }
                Token::LParen => {
                    self.bump();
                    let args = self.parse_call_args();
                    let close = self.peek().span;
                    self.expect(Token::RParen, "expected `)` after call arguments");
                    let span = Span::new(lhs.span.file, lhs.span.lo, close.hi);
                    lhs = Expr {
                        span,
                        kind: ExprKind::Call {
                            callee: Box::new(lhs),
                            args,
                        },
                    };
                }
                Token::LBracket => {
                    self.bump();
                    let index = self.parse_expr();
                    let close = self.peek().span;
                    self.expect(Token::RBracket, "expected `]` after index expression");
                    let span = Span::new(lhs.span.file, lhs.span.lo, close.hi);
                    lhs = Expr {
                        span,
                        kind: ExprKind::Index {
                            receiver: Box::new(lhs),
                            index: Box::new(index),
                        },
                    };
                }
                Token::Question => {
                    let q = self.bump();
                    let span = lhs.span.join(q.span);
                    lhs = Expr {
                        span,
                        kind: ExprKind::Try(Box::new(lhs)),
                    };
                }
                _ => return lhs,
            }
        }
    }

    //            consumed the preceding `Token::Dot`
    //            from `lhs.span.lo` to the integer literal's `span.hi`
    /// Finish parsing a tuple positional-field access (`receiver.N`)
    /// whose `Int` token is at the cursor. Non-decimal bases and values
    /// that overflow `u32` emit a `ParseError` and recover with index 0
    /// so downstream passes still see a well-shaped node.
    fn finish_tuple_index(&mut self, receiver: Expr, value: u128, base: IntBase) -> Expr {
        let tok = self.bump();
        let span = receiver.span.join(tok.span);
        if base != IntBase::Dec {
            self.emit_error(
                tok.span,
                "tuple-index after `.` must be a base-10 integer literal".to_string(),
            );
            return Expr {
                span,
                kind: ExprKind::TupleIndex {
                    receiver: Box::new(receiver),
                    index: 0,
                },
            };
        }
        let index = match u32::try_from(value) {
            Ok(i) => i,
            Err(_) => {
                self.emit_error(
                    tok.span,
                    format!("tuple-index {value} exceeds the maximum admitted ({})", u32::MAX),
                );
                0
            }
        };
        Expr {
            span,
            kind: ExprKind::TupleIndex {
                receiver: Box::new(receiver),
                index,
            },
        }
    }

    fn parse_call_args(&mut self) -> Vec<CallArg> {
        let mut args = Vec::new();
        if self.at(Token::RParen) {
            return args;
        }
        args.push(self.parse_call_arg());
        while self.eat(Token::Comma) {
            if self.at(Token::RParen) {
                break;
            }
            args.push(self.parse_call_arg());
        }
        args
    }

    /// Parse one call argument. Preserves any leading mode keyword
    /// (`mutable` / `take` / `init`) so the mode tracker in `edda-types`
    /// can apply the §4 state transitions, and any leading payload-field
    /// name (`field: expr`) so the typechecker can validate
    /// variant-constructor calls per `declarations.md` §252.
    fn parse_call_arg(&mut self) -> CallArg {
        let (mode, mode_span) = match self.peek_kind() {
            Token::Mutable => {
                let tok = self.bump();
                (Some(CallMode::Mutable), Some(tok.span))
            }
            Token::Take => {
                let tok = self.bump();
                (Some(CallMode::Take), Some(tok.span))
            }
            Token::Init => {
                let tok = self.bump();
                (Some(CallMode::Init), Some(tok.span))
            }
            _ => (None, None),
        };
        // 2-token lookahead for the named-payload form: `Ident : expr`.
        // The parser cannot tell a function call from a variant
        // constructor here, so both forms parse into the same shape and
        // edda-types validates downstream.
        let name = if matches!(self.peek_kind(), Token::Ident(_))
            && self.peek_at(1).kind == Token::Colon
        {
            let ident_tok = self.bump();
            let Token::Ident(sym) = ident_tok.kind else {
                unreachable!("guarded by peek_kind match above")
            };
            self.bump();
            Some(Ident {
                name: sym,
                span: ident_tok.span,
            })
        } else {
            None
        };
        let expr = self.parse_expr();
        let start_span = mode_span
            .or_else(|| name.as_ref().map(|n| n.span))
            .unwrap_or(expr.span);
        let span = start_span.join(expr.span);
        CallArg {
            span,
            mode,
            name,
            expr,
        }
    }
}
