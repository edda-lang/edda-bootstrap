//! Statement parsing — `let` / `var` bindings, assignment statements,
//! expression statements — plus the block-parsing logic that decides
//! between an expression statement and a trailing block-result expression.

use crate::ast::{AssignOp, Attribute, BindingMode, Block, Expr, ExprKind, Stmt, StmtKind};
use crate::token::Token;

use super::Parser;

fn assign_op_for(t: Token) -> Option<AssignOp> {
    Some(match t {
        Token::Eq => AssignOp::Plain,
        Token::PlusEq => AssignOp::Add,
        Token::MinusEq => AssignOp::Sub,
        Token::StarEq => AssignOp::Mul,
        Token::SlashEq => AssignOp::Div,
        Token::PercentEq => AssignOp::Mod,
        Token::AmpEq => AssignOp::BitAnd,
        Token::PipeEq => AssignOp::BitOr,
        Token::CaretEq => AssignOp::BitXor,
        Token::LtLtEq => AssignOp::Shl,
        Token::GtGtEq => AssignOp::Shr,
        _ => return None,
    })
}

/// True when `e` is admitted as the LHS of an assignment per
/// `corpus/edda-codex/docs/syntax/expressions.md` §"LHS production for
/// assignment". The shape is an identifier path, or a call / method-call
/// (assignment through a return-position `mutable`/`let` borrow —
/// `at_mut(mutable o).val = 42`), followed by zero or more `.field` or
/// `[index]` projections. The parser collapses a leading chain of
/// dot-fields into a single multi-segment `Path` node, so the receiver
/// chain bottoms out at a `Path` of any length (length >= 1) or a call.
/// Every other expression form — `(a, b)`, struct literals, casts — is
/// rejected.
fn is_valid_assign_lhs(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Path(p) => !p.segments.is_empty(),
        ExprKind::Call { .. } | ExprKind::MethodCall { .. } => true,
        ExprKind::Field { receiver, .. } => is_valid_assign_lhs(receiver),
        ExprKind::TupleIndex { receiver, .. } => is_valid_assign_lhs(receiver),
        // Comptime-indexed field assignment `out.(i) = …` (D-22). Valid
        // only inside a `comptime for`; the pass-2 expansion rewrites it
        // to a plain `Field` LHS before lowering.
        ExprKind::CompField { receiver, .. } => is_valid_assign_lhs(receiver),
        ExprKind::Index { receiver, .. } => is_valid_assign_lhs(receiver),
        _ => false,
    }
}

impl<'a> Parser<'a> {
    /// Parse a `{ stmts; trailing }` block. The leading `LBrace` is
    /// expected at the cursor; a recovery diagnostic is emitted otherwise.
    pub(crate) fn parse_block(&mut self) -> Block {
        let start = self.pos;
        if !self.eat(Token::LBrace) {
            let span = self.peek().span;
            self.emit_error(span, "expected `{` to open block");
            return Block {
                span,
                stmts: Vec::new(),
                trailing: None,
            };
        }
        let mut stmts: Vec<Stmt> = Vec::new();
        let mut trailing: Option<Box<Expr>> = None;
        while !self.at(Token::RBrace) && !self.at_eof() {
            let iter_start = self.pos;
            self.skip_doc_trivia();
            if self.at(Token::RBrace) {
                break;
            }
            let stmt_start = self.pos;
            // Leading `@name(args)` attributes (e.g. site-level `@trust` /
            // `@unverified`) — the same parser the item layer uses; an
            // empty list when the cursor is not at `@`. Statement-level
            // attributes are not yet honoured at discharge (that needs the
            // body-induced obligation pass), but parsing + round-tripping
            // them lets `.ea` source use the V1.0 site-granular form now
            // instead of an over-trusting whole-function `@unverified`.
            let attributes = self.parse_item_attributes();
            // Let / var bindings are unambiguous at statement position.
            if matches!(self.peek_kind(), Token::Let | Token::Var | Token::Uninit) {
                let stmt = self.parse_let_stmt(attributes);
                self.eat(Token::Semicolon);
                stmts.push(stmt);
                continue;
            }
            // Otherwise: an expression, possibly the LHS of an assignment or
            // the block's trailing value.
            let expr = self.parse_expr();
            if let Some(op) = assign_op_for(self.peek_kind()) {
                self.bump();
                if !is_valid_assign_lhs(&expr) {
                    self.emit_error(
                        expr.span,
                        "invalid assignment target — expected `<name>` followed by zero or more `.field` or `[index]` projections",
                    );
                }
                let rhs = self.parse_expr();
                self.eat(Token::Semicolon);
                stmts.push(Stmt {
                    span: self.span_from(stmt_start),
                    attributes,
                    kind: StmtKind::Assign {
                        target: expr,
                        op,
                        rhs,
                    },
                });
                continue;
            }
            // No assignment operator → expression statement, or trailing
            // expression if we're at the closing `}`. An *attributed*
            // expression is always a statement (a block's result value is
            // never annotated), so only an unattributed expression at the
            // closing brace becomes the block's trailing value.
            if self.at(Token::RBrace) && attributes.is_empty() {
                trailing = Some(Box::new(expr));
                break;
            }
            // Optional `;`. The spec marks the semicolon non-load-bearing,
            // so a missing one is fine when the next token can plausibly
            // start the next statement.
            self.eat(Token::Semicolon);
            stmts.push(Stmt {
                span: self.span_from(stmt_start),
                attributes,
                kind: StmtKind::Expr(expr),
            });
            if self.pos == iter_start {
                // parse_expr's catch-all recovers without bumping; force
                // progress so the loop terminates in O(tokens).
                self.bump();
            }
        }
        self.expect(Token::RBrace, "expected `}` to close block");
        Block {
            span: self.span_from(start),
            stmts,
            trailing,
        }
    }

    fn parse_let_stmt(&mut self, attributes: Vec<Attribute>) -> Stmt {
        let start = self.pos;
        let mutability = match self.peek_kind() {
            Token::Let => BindingMode::Immutable,
            Token::Var => BindingMode::Mutable,
            Token::Uninit => BindingMode::Uninit,
            _ => unreachable!("parse_let_stmt called on non-binding token"),
        };
        let keyword_span = self.peek().span;
        self.bump(); // `let` / `var` / `uninit`
        let pat = self.parse_pat();
        let ty = if self.eat(Token::Colon) {
            Some(self.parse_ty())
        } else {
            None
        };
        let init = if self.eat(Token::Eq) {
            Some(self.parse_expr())
        } else {
            None
        };
        // Enforce the uninit invariant: type required, initialiser forbidden.
        if mutability == BindingMode::Uninit {
            if ty.is_none() {
                self.emit_error(
                    keyword_span,
                    "`uninit` declaration requires an explicit type annotation",
                );
            }
            if init.is_some() {
                self.emit_error(
                    keyword_span,
                    "`uninit` declaration must not have an initialiser; use `let` or `var` for initialised bindings",
                );
            }
        }
        Stmt {
            span: self.span_from(start),
            attributes,
            kind: StmtKind::Let {
                mutability,
                pat,
                ty,
                init,
            },
        }
    }
}
