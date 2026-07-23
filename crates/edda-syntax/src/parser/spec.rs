//! Spec declaration and invocation parser.
//!
//! Split out from `item.rs` for file-size reasons. The §334 single-token
//! lookahead past `)` distinguishes declaration (`where`/`{`) from
//! invocation (anything else); see `comptime.md` §312-364.

use crate::ast::{
    AdmitsConstraint, BinOp, Expr, GenericKind, GenericParam, Item, ItemKind, Literal, Path,
    RefinementClause, RefinementKind, Spec, SpecInvocation, Stability, Visibility,
};
use crate::token::Token;

use super::Parser;

impl<'a> Parser<'a> {
    pub(super) fn parse_spec_or_invocation(
        &mut self,
        stability: Option<Stability>,
        visibility: Visibility,
    ) -> ItemKind {
        let start = self.pos;
        self.bump(); // `spec`
        // The invocation form admits a multi-segment path (`std.option.Option`);
        // the declaration form admits only a single identifier. We parse a
        // path here and let the routing decide which form we have.
        let path = self.parse_path();
        // No parens at all → emit error and recover.
        if !self.at(Token::LParen) {
            let span = self.peek().span;
            self.emit_error(span, "expected `(` after spec name");
            return ItemKind::SpecInvocation(Box::new(SpecInvocation {
                span: self.span_from(start),
                path,
                args: Vec::new(),
            }));
        }
        // A multi-segment path can only name an invocation — declarations
        // are single identifiers. This admits the zero-arg invocation form
        // `spec std.collections.hashset.IntSet()`,
        // which the arglist peek below would otherwise route to declaration.
        if path.segments.len() > 1 {
            return self.parse_spec_invocation(start, path);
        }
        // Pre-`(` token-level peek decides the form without dual-parsing
        // the arglist. A leading `comptime` or `<ident> :` shape is always
        // a declaration; anything else is an invocation.
        if self.spec_arglist_looks_like_declaration() {
            self.parse_spec_declaration(start, stability, visibility, path)
        } else {
            self.parse_spec_invocation(start, path)
        }
    }

    fn spec_arglist_looks_like_declaration(&self) -> bool {
        let first = self.peek_at(1).kind;
        // Empty arglist `()` — only single-segment names reach this peek
        // (multi-segment paths already routed to invocation), and a bare
        // `spec Name()` at file scope is the zero-param declaration form;
        // treat as declaration so `where`/`{` afterwards routes correctly.
        if first == Token::RParen {
            return true;
        }
        if first == Token::Comptime {
            return true;
        }
        // `<ident> : ...` → declaration. Otherwise invocation.
        matches!(first, Token::Ident(_)) && self.peek_at(2).kind == Token::Colon
    }

    //   optional `where` clause chain, and the `{ ... }` body
    fn parse_spec_declaration(
        &mut self,
        start: usize,
        stability: Option<Stability>,
        visibility: Visibility,
        path: Path,
    ) -> ItemKind {
        // The declaration form admits only a single-segment name.
        let name = if path.segments.len() == 1 {
            path.segments[0]
        } else {
            self.emit_error(
                path.span,
                "spec declaration name must be a single identifier",
            );
            path.segments[0]
        };
        self.bump(); // `(`
        let mut generics: Vec<GenericParam> = Vec::new();
        if !self.at(Token::RParen) {
            generics.push(self.parse_spec_generic_param());
            while self.eat(Token::Comma) {
                if self.at(Token::RParen) {
                    break;
                }
                generics.push(self.parse_spec_generic_param());
            }
        }
        self.expect(Token::RParen, "expected `)` after spec parameters");
        let where_clauses = self.parse_spec_where_clauses();
        let body = self.parse_spec_body();
        ItemKind::Spec(Box::new(Spec {
            span: self.span_from(start),
            stability,
            visibility,
            name,
            generics,
            where_clauses,
            body,
        }))
    }

    fn parse_spec_where_clauses(&mut self) -> Vec<RefinementClause> {
        let mut clauses = Vec::new();
        while self.at(Token::Where) {
            let cls_start = self.pos;
            self.bump(); // `where`
            let pred = self.parse_expr_no_struct();
            clauses.push(RefinementClause {
                span: self.span_from(cls_start),
                kind: RefinementKind::Where,
                pred,
            });
        }
        clauses
    }

    fn parse_spec_body(&mut self) -> Vec<Item> {
        self.expect(Token::LBrace, "expected `{` to open spec body");
        let mut items: Vec<Item> = Vec::new();
        while !self.at(Token::RBrace) && !self.at_eof() {
            let loop_start = self.pos;
            let item_doc = self.collect_item_doc_block();
            if self.at(Token::RBrace) || self.at_eof() {
                break;
            }
            items.push(self.parse_item(item_doc));
            if self.pos == loop_start {
                self.bump();
            }
        }
        self.expect(Token::RBrace, "expected `}` to close spec body");
        items
    }

    //   declaration is forward-looking; per `comptime.md` §312 the invocation
    //   form is the directive shape `spec Path(args)` and does not admit `public`
    fn parse_spec_invocation(&mut self, start: usize, path: Path) -> ItemKind {
        self.bump(); // `(`
        let mut args: Vec<Expr> = Vec::new();
        if !self.at(Token::RParen) {
            args.push(self.parse_expr());
            while self.eat(Token::Comma) {
                if self.at(Token::RParen) {
                    break;
                }
                args.push(self.parse_expr());
            }
        }
        self.expect(Token::RParen, "expected `)` after spec arguments");
        ItemKind::SpecInvocation(Box::new(SpecInvocation {
            span: self.span_from(start),
            path,
            args,
        }))
    }

    fn parse_spec_generic_param(&mut self) -> GenericParam {
        let start = self.pos;
        let is_comptime = self.eat(Token::Comptime);
        let name = self.expect_ident("expected spec parameter name");
        self.expect(Token::Colon, "expected `:` after spec parameter name");
        // Use `parse_ty_no_where` so a trailing `where` is reserved for the
        // admits clause, not folded into a refinement on the inner type.
        let ty = self.parse_ty_no_where();
        let admits = if self.at(Token::Where) {
            self.parse_admits_clause(name)
        } else {
            Vec::new()
        };
        let kind = if is_comptime {
            GenericKind::Comptime
        } else {
            GenericKind::Type
        };
        GenericParam {
            span: self.span_from(start),
            kind,
            name,
            ty: Some(ty),
            admits,
        }
    }

    fn parse_admits_clause(&mut self, param_name: crate::ast::Ident) -> Vec<AdmitsConstraint> {
        self.bump(); // `where`
        let bound = self.expect_ident("expected the spec parameter name after `where`");
        if bound.name != param_name.name && bound.name != edda_intern::Symbol::DUMMY {
            self.emit_error(
                bound.span,
                "the `where` clause must rebind the spec parameter introduced just before it",
            );
        }
        if !self.at_admits_keyword() {
            let span = self.peek().span;
            self.emit_error(span, "expected `provides` in spec-parameter constraint clause");
            return Vec::new();
        }
        self.bump(); // contextual `provides`
        let mut atoms = Vec::new();
        atoms.push(self.parse_admits_atom());
        // After each atom, a `,` could either continue the admits list
        // or terminate this spec parameter and start the next one. We
        // disambiguate by 1-token lookahead past `,`: if the token
        // after the comma looks like an atom-start, the list continues.
        // Otherwise the admits clause ends and the comma belongs to the
        // outer spec-param separator.
        while self.peek_kind() == Token::Comma && self.next_token_starts_admits_atom() {
            self.bump(); // `,`
            atoms.push(self.parse_admits_atom());
        }
        atoms
    }

    fn next_token_starts_admits_atom(&self) -> bool {
        matches!(
            self.peek_at(1).kind,
            Token::Plus
                | Token::Minus
                | Token::Star
                | Token::Slash
                | Token::Percent
                | Token::Lt
                | Token::LtEq
                | Token::Gt
                | Token::GtEq
                | Token::EqEq
                | Token::BangEq
                | Token::Int { .. }
                | Token::Ident(_)
        )
    }

    fn at_admits_keyword(&self) -> bool {
        if let Token::Ident(sym) = self.peek_kind() {
            self.interner.resolve(sym) == "provides"
        } else {
            false
        }
    }

    fn parse_admits_atom(&mut self) -> AdmitsConstraint {
        // Operator atoms.
        let op_for_token = |tok: Token| -> Option<BinOp> {
            Some(match tok {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                Token::Percent => BinOp::Mod,
                Token::Lt => BinOp::Lt,
                Token::LtEq => BinOp::Le,
                Token::Gt => BinOp::Gt,
                Token::GtEq => BinOp::Ge,
                Token::EqEq => BinOp::Eq,
                Token::BangEq => BinOp::Ne,
                _ => return None,
            })
        };
        if let Some(op) = op_for_token(self.peek_kind()) {
            let tok = self.bump();
            return AdmitsConstraint::Op { span: tok.span, op };
        }
        // Constant atoms (currently only integer literals).
        if let Token::Int { value, base } = self.peek_kind() {
            let tok = self.bump();
            return AdmitsConstraint::Literal {
                span: tok.span,
                lit: Literal::Int { value, base },
            };
        }
        // Member-shape atoms: `name: <type>` (typically `name: function(...) -> ...`).
        // Used on `comptime A: Module` parameters per phase-2-locks Gap 6.
        if matches!(self.peek_kind(), Token::Ident(_)) && self.peek_at(1).kind == Token::Colon {
            let start = self.pos;
            let name = self.expect_ident("expected member name in admits clause");
            self.bump(); // `:`
            let ty = self.parse_ty_no_where();
            return AdmitsConstraint::Member {
                span: self.span_from(start),
                name,
                ty,
            };
        }
        let span = self.peek().span;
        self.emit_error(span, "expected admits-clause atom");
        // Recovery: synthesize a placeholder Op so the caller list isn't empty.
        AdmitsConstraint::Op {
            span,
            op: BinOp::Add,
        }
    }
}
