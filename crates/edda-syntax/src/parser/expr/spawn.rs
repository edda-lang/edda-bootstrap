//! Spawn-block parsing: the `<scope-name>.spawn (take ...)? { body }`
//! structured-concurrency surface — its lookahead disambiguation against
//! an ordinary `.spawn(...)` method call, and the spawn-argument list.
//! Split out from the Pratt core for file-size reasons; every method
//! extends the same `impl Parser` block.

use edda_span::Span;

use crate::ast::{Expr, ExprKind, SpawnArg, SpawnExpr};
use crate::token::Token;

use super::super::Parser;

impl<'a> Parser<'a> {
    pub(super) fn looks_like_spawn_block(&self, path: &crate::ast::Path) -> bool {
        if path.segments.len() != 2 {
            return false;
        }
        let tail = path.segments.last().expect("path with >=2 segments");
        if self.interner.resolve(tail.name) != "spawn" {
            return false;
        }
        match self.peek_kind() {
            Token::LBrace => true,
            // The `(...)` opener is only a spawn-arg list when the next
            // token is the locked `take` keyword — every spawn arg starts
            // with `take`. Other contents (positional method-call args)
            // fall back to the postfix call path, which is the right
            // interpretation for an ordinary method named `spawn`.
            Token::LParen => matches!(self.peek_at(1).kind, Token::Take),
            _ => false,
        }
    }

    pub(super) fn parse_spawn_tail(&mut self, path: crate::ast::Path) -> Expr {
        // Caller has verified `path.segments.len() == 2` with the tail
        // segment equal to the contextual `spawn` keyword.
        let path_span = path.span;
        let mut segments = path.segments.into_iter();
        let scope_name = segments.next().expect("scope-name segment");

        let args = if self.eat(Token::LParen) {
            self.parse_spawn_args()
        } else {
            Vec::new()
        };

        let body = self.parse_block();
        let span = Span::new(path_span.file, path_span.lo, body.span.hi);
        Expr {
            span,
            kind: ExprKind::Spawn(Box::new(SpawnExpr {
                span,
                scope_name,
                args,
                body,
            })),
        }
    }

    fn parse_spawn_args(&mut self) -> Vec<SpawnArg> {
        let mut args = Vec::new();
        if !self.at(Token::RParen) {
            args.push(self.parse_spawn_arg());
            while self.eat(Token::Comma) {
                if self.at(Token::RParen) {
                    break;
                }
                args.push(self.parse_spawn_arg());
            }
        }
        self.expect(Token::RParen, "expected `)` after spawn arguments");
        args
    }

    fn parse_spawn_arg(&mut self) -> SpawnArg {
        let start = self.pos;
        self.expect(
            Token::Take,
            "expected `take` — spawn arguments are always take-mode per effects.md",
        );
        let name = self.expect_ident("expected spawn argument binding name");
        let ty = if self.eat(Token::Colon) {
            Some(self.parse_ty_no_where())
        } else {
            None
        };
        self.expect(Token::Eq, "expected `=` after spawn argument binder");
        let init = self.parse_expr();
        SpawnArg {
            span: self.span_from(start),
            name,
            ty,
            init,
        }
    }
}
