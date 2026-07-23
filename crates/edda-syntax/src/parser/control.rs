//! Control-flow and "abrupt completion" expression parsers: `if`,
//! `match`, `loop`, `for`, `comptime`, `scope`, `return`, `break`,
//! `continue`, `raise`, `panic`. Split out from `expr.rs` for file-size
//! reasons; all methods extend the same `impl Lexer` over [`Parser`].

use crate::ast::{Expr, ExprKind, MatchArm, Pat};
use crate::token::Token;

use super::Parser;

impl<'a> Parser<'a> {
    pub(super) fn parse_if(&mut self) -> Expr {
        let start = self.pos;
        self.bump(); // `if`
        let cond = self.parse_expr_no_struct();
        let then_block = self.parse_block();
        let else_branch = if self.eat(Token::Else) {
            if self.at(Token::If) {
                Some(Box::new(self.parse_if()))
            } else {
                let blk = self.parse_block();
                Some(Box::new(Expr {
                    span: blk.span,
                    kind: ExprKind::Block(blk),
                }))
            }
        } else {
            None
        };
        Expr {
            span: self.span_from(start),
            kind: ExprKind::If {
                cond: Box::new(cond),
                then_block,
                else_branch,
            },
        }
    }

    pub(super) fn parse_match(&mut self) -> Expr {
        let start = self.pos;
        self.bump(); // `match`
        let scrutinee = self.parse_expr_no_struct();
        self.expect(Token::LBrace, "expected `{` to open match arms");
        let mut arms = Vec::new();
        while !self.at(Token::RBrace) && !self.at_eof() {
            let arm_start = self.pos;
            self.parse_match_arm(&mut arms);
            if self.pos == arm_start {
                // Recovery made no progress. Force-bump so the loop terminates.
                self.bump();
            }
        }
        self.expect(Token::RBrace, "expected `}` to close match");
        Expr {
            span: self.span_from(start),
            kind: ExprKind::Match {
                scrutinee: Box::new(scrutinee),
                arms,
            },
        }
    }

    /// Parse one `case` arm, pushing one `MatchArm` per `|`-separated
    /// pattern alternative into `arms`.
    fn parse_match_arm(&mut self, arms: &mut Vec<MatchArm>) {
        let start = self.pos;
        self.expect(Token::Case, "expected `case` to introduce match arm");
        let mut pats = vec![self.parse_match_pat()];
        while self.eat(Token::Pipe) {
            pats.push(self.parse_match_pat());
        }
        let guard = if self.eat(Token::Where) {
            Some(self.parse_expr_no_struct())
        } else {
            None
        };
        self.expect(Token::FatArrow, "expected `=>` in match arm");
        let body = self.parse_expr();
        let span = self.span_from(start);
        if pats.len() > 1 {
            self.check_or_pattern_binder_sets(&pats);
        }
        let mut pats = pats.into_iter();
        // `next_back` keeps the last alternative as the move target so the
        // guard/body clones happen only for the preceding alternatives.
        let last = pats.next_back().expect("at least one pattern parsed");
        for pat in pats {
            arms.push(MatchArm { span, pat, guard: guard.clone(), body: body.clone() });
        }
        arms.push(MatchArm { span, pat: last, guard, body });
    }

    /// Emit `parse_error` on every or-pattern alternative whose bound-name
    /// set differs from the first alternative's.
    fn check_or_pattern_binder_sets(&mut self, pats: &[Pat]) {
        let canonical_names = |p: &Pat| {
            let mut names = Vec::new();
            super::pat::pat_bound_names(p, &mut names);
            names.sort_unstable();
            names.dedup();
            names
        };
        let first_names = canonical_names(&pats[0]);
        for p in &pats[1..] {
            if canonical_names(p) != first_names {
                self.emit_error(
                    p.span,
                    "or-pattern alternatives must bind the same set of names — this \
                     alternative's binders differ from the first alternative's",
                );
            }
        }
    }

    pub(super) fn parse_loop(&mut self) -> Expr {
        let start = self.pos;
        self.bump(); // `loop`
        // Optional `decreases <expr>` measure clause per
        // corpus/edda-codex/language/01-syntax.md §loop. The measure is
        // parsed with struct literals disallowed so the `{` of the body
        // block is not eaten as a struct-literal head.
        let decreases = if self.eat(Token::Decreases) {
            Some(Box::new(self.parse_expr_no_struct()))
        } else {
            None
        };
        let body = self.parse_block();
        Expr {
            span: self.span_from(start),
            kind: ExprKind::Loop {
                body,
                label: None,
                decreases,
            },
        }
    }

    pub(super) fn parse_for(&mut self) -> Expr {
        let start = self.pos;
        self.bump(); // `for`
        let pat = self.parse_pat();
        self.expect(Token::In, "expected `in` after `for` pattern");
        let iter = self.parse_expr_no_struct();
        let body = self.parse_block();
        Expr {
            span: self.span_from(start),
            kind: ExprKind::For {
                pat: Box::new(pat),
                iter: Box::new(iter),
                body,
                label: None,
            },
        }
    }

    pub(super) fn parse_comptime(&mut self) -> Expr {
        let start = self.pos;
        self.bump(); // `comptime`
        if self.at(Token::LBrace) {
            let block = self.parse_block();
            return Expr {
                span: self.span_from(start),
                kind: ExprKind::ComptimeBlock(block),
            };
        }
        let inner = self.parse_expr();
        Expr {
            span: self.span_from(start),
            kind: ExprKind::Comptime(Box::new(inner)),
        }
    }

    pub(super) fn parse_scope(&mut self) -> Expr {
        let start = self.pos;
        self.bump(); // `scope`
        // The surface admits both `scope(exec)` and `scope(coherence)` per
        // `05-concurrency-coherence.md` §3. The lexer does not reserve
        // `exec` or `coherence`; we resolve the kind from the identifier
        // text and reject unknown spellings.
        let mut kind = crate::ast::ScopeKind::Exec;
        if self.eat(Token::LParen) {
            if let Token::Ident(sym) = self.peek_kind() {
                let lex = self.bump();
                let text = self.interner.resolve(sym);
                kind = match text {
                    "exec" => crate::ast::ScopeKind::Exec,
                    "coherence" => crate::ast::ScopeKind::Coherence,
                    _ => {
                        self.emit_error(
                            lex.span,
                            format!(
                                "unknown scope kind `{text}` — expected `exec` or `coherence`"
                            ),
                        );
                        crate::ast::ScopeKind::Exec
                    }
                };
            }
            self.expect(Token::RParen, "expected `)` after scope kind");
        }
        // Optional binder name between `(exec)` / `(coherence)` and the body block:
        // `scope(coherence) build { ... }`.
        let name = if matches!(self.peek_kind(), Token::Ident(_)) {
            self.eat_ident()
        } else {
            None
        };
        let body = self.parse_block();
        Expr {
            span: self.span_from(start),
            kind: ExprKind::Scope { kind, name, body },
        }
    }

    pub(super) fn parse_return(&mut self) -> Expr {
        let start = self.pos;
        self.bump();
        let value = if can_start_expr(self.peek_kind()) {
            Some(Box::new(self.parse_expr()))
        } else {
            None
        };
        Expr {
            span: self.span_from(start),
            kind: ExprKind::Return(value),
        }
    }

    pub(super) fn parse_break(&mut self) -> Expr {
        let start = self.pos;
        self.bump();
        let value = if can_start_expr(self.peek_kind()) {
            Some(Box::new(self.parse_expr()))
        } else {
            None
        };
        Expr {
            span: self.span_from(start),
            kind: ExprKind::Break { label: None, value },
        }
    }

    pub(super) fn parse_continue(&mut self) -> Expr {
        let start = self.pos;
        self.bump();
        Expr {
            span: self.span_from(start),
            kind: ExprKind::Continue { label: None },
        }
    }

    pub(super) fn parse_raise(&mut self) -> Expr {
        let start = self.pos;
        self.bump();
        let inner = self.parse_expr();
        Expr {
            span: self.span_from(start),
            kind: ExprKind::Raise(Box::new(inner)),
        }
    }

    pub(super) fn parse_panic(&mut self) -> Expr {
        let start = self.pos;
        self.bump();
        let inner = self.parse_expr();
        Expr {
            span: self.span_from(start),
            kind: ExprKind::Panic(Box::new(inner)),
        }
    }

    pub(super) fn parse_forall(&mut self) -> Expr {
        let start = self.pos;
        self.bump(); // `forall`
        let bound = self.expect_ident("expected bound variable name after `forall`");
        self.expect(Token::In, "expected `in` after bound variable in `forall`");
        let iter = self.parse_expr_no_struct();
        self.expect(Token::Colon, "expected `:` after `in <iter>` in `forall`");
        let body = self.parse_expr_no_struct();
        Expr {
            span: self.span_from(start),
            kind: ExprKind::Forall {
                bound,
                iter: Box::new(iter),
                body: Box::new(body),
            },
        }
    }

    pub(super) fn parse_exists(&mut self) -> Expr {
        let start = self.pos;
        self.bump(); // `exists`
        let bound = self.expect_ident("expected bound variable name after `exists`");
        self.expect(Token::In, "expected `in` after bound variable in `exists`");
        let iter = self.parse_expr_no_struct();
        self.expect(Token::Colon, "expected `:` after `in <iter>` in `exists`");
        let body = self.parse_expr_no_struct();
        Expr {
            span: self.span_from(start),
            kind: ExprKind::Exists {
                bound,
                iter: Box::new(iter),
                body: Box::new(body),
            },
        }
    }

    pub(super) fn parse_handle_expr(&mut self) -> Expr {
        let start = self.pos;
        self.bump(); // `handle` (contextual identifier)
        let effect = self.expect_ident("expected effect label after `handle`");
        let (ty, binder) = if self.at(Token::Colon) {
            self.bump(); // `:`
            let ty = self.parse_ty_no_where();
            let binder = if self.at(Token::As) {
                self.bump(); // `as`
                Some(self.expect_ident("expected binder name after `as` in `handle`"))
            } else {
                None
            };
            (Some(Box::new(ty)), binder)
        } else {
            (None, None)
        };
        self.expect(Token::Arrow, "expected `->` after effect label in `handle`");
        let recovery = self.parse_expr_no_struct();
        let body = self.parse_block();
        Expr {
            span: self.span_from(start),
            kind: ExprKind::Handle {
                effect,
                ty,
                binder,
                recovery: Box::new(recovery),
                body,
            },
        }
    }
}

/// True if `t` can plausibly start an expression. Used by `return`/`break`
/// to decide whether a value follows, and by the prefix-/infix-range
/// parser to decide whether an endpoint follows the `..` operator.
pub(super) fn can_start_expr(t: Token) -> bool {
    matches!(
        t,
        Token::Int { .. }
            | Token::Float(_)
            | Token::Str(_)
            | Token::FString(_)
            | Token::True
            | Token::False
            | Token::Ident(_)
            | Token::ResultKw
            | Token::LParen
            | Token::LBracket
            | Token::LBrace
            | Token::If
            | Token::Match
            | Token::Loop
            | Token::For
            | Token::Comptime
            | Token::Function
            | Token::ScopeKw
            | Token::Return
            | Token::Break
            | Token::Continue
            | Token::Raise
            | Token::Panic
            | Token::Forall
            | Token::Exists
            | Token::Await
            | Token::With
            | Token::DotDot
            | Token::DotDotLt
            | Token::DotDotEq
            | Token::Minus
            | Token::Bang
            | Token::Tilde
    )
}
