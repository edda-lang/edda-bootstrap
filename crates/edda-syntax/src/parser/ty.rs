//! Type-expression parser. Implements every form admitted by
//! `docs/syntax/types.md` plus the satellite types from `effects.md`
//! (effect rows) and `refinements.md` (`where` clauses on types).

use edda_diag::DiagnosticClass;

use crate::ast::{
    EffectMember, EffectRow, FnTypeParam, Ident, ParamMode, Path, RefinementClause, RefinementKind,
    Type, TypeKind,
};
use crate::token::Token;

use super::Parser;

impl<'a> Parser<'a> {
    /// Parse a single type expression, including any trailing `where`
    /// refinement clause.
    pub(crate) fn parse_ty(&mut self) -> Type {
        let base = self.parse_ty_no_where();
        // Trailing `where pred` refines the base type.
        if !self.eat(Token::Where) {
            return base;
        }
        let pred = self.parse_expr_no_struct();
        let span = base.span.join(pred.span);
        let refined = Type {
            span,
            kind: TypeKind::Refined {
                base: Box::new(base),
                pred,
            },
        };
        // Reject stacked inline `where` per refinements.md — combine with `&&`.
        if self.at(Token::Where) {
            let next_span = self.peek().span;
            self.emit_error(
                next_span,
                "multiple inline `where` clauses on one type are not admitted — combine with `&&`: `T where a && b`",
            );
        }
        refined
    }

    /// Parse a type expression without consuming a trailing `where` clause.
    /// Used at every element-position type (slice element, tuple element, function
    /// param / return) where `types.md` admits `where` only at the outermost level.
    pub(crate) fn parse_ty_no_where(&mut self) -> Type {
        self.parse_ty_primary()
    }

    fn parse_ty_primary(&mut self) -> Type {
        match self.peek_kind() {
            Token::LParen => self.parse_ty_paren_or_tuple(),
            Token::LBracket => self.parse_ty_slice(),
            Token::Function => self.parse_ty_function(),
            Token::TypeMeta => {
                let lex = self.bump();
                Type {
                    span: lex.span,
                    kind: TypeKind::Meta,
                }
            }
            Token::Comptime => {
                let span = self.peek().span;
                self.emit_error(
                    span,
                    "`comptime` is not admitted in type position; comptime is locked only as an expression prefix, a block introducer, and a parameter prefix",
                );
                self.bump();
                Type {
                    span,
                    kind: TypeKind::Error,
                }
            }
            Token::Ident(_) => {
                let path = self.parse_path();
                Type {
                    span: path.span,
                    kind: TypeKind::Path(path),
                }
            }
            _ => {
                let span = self.peek().span;
                self.emit_error(span, "expected type");
                Type {
                    span,
                    kind: TypeKind::Error,
                }
            }
        }
    }

    fn parse_ty_paren_or_tuple(&mut self) -> Type {
        let start = self.pos;
        self.bump(); // consume `(`
        if self.eat(Token::RParen) {
            // `()` — unit type.
            return Type {
                span: self.span_from(start),
                kind: TypeKind::Unit,
            };
        }
        let first = self.parse_ty();
        if self.eat(Token::RParen) {
            // `(T)` — parenthesized grouping; carry the inner span.
            return first;
        }
        // Tuple form `(T, U, ...)` — minimum 2 elements.
        let mut elems = vec![first];
        while self.eat(Token::Comma) {
            if self.at(Token::RParen) {
                break; // trailing comma tolerated
            }
            elems.push(self.parse_ty());
        }
        self.expect(Token::RParen, "expected `)` after tuple type");
        Type {
            span: self.span_from(start),
            kind: TypeKind::Tuple(elems),
        }
    }

    fn parse_ty_slice(&mut self) -> Type {
        let start = self.pos;
        self.bump(); // consume `[`
        let inner = self.parse_ty_no_where();
        self.expect(Token::RBracket, "expected `]` after slice element type");
        Type {
            span: self.span_from(start),
            kind: TypeKind::Slice(Box::new(inner)),
        }
    }

    fn parse_ty_function(&mut self) -> Type {
        let start = self.pos;
        self.bump(); // consume `function`
        self.expect(Token::LParen, "expected `(` in function type");
        let mut params = Vec::new();
        if !self.at(Token::RParen) {
            params.push(self.parse_fn_type_param());
            while self.eat(Token::Comma) {
                if self.at(Token::RParen) {
                    break;
                }
                params.push(self.parse_fn_type_param());
            }
        }
        self.expect(Token::RParen, "expected `)` after function parameter list");
        self.expect(Token::Arrow, "expected `->` in function type");
        let ret = self.parse_ty_no_where();
        let effects = if self.at(Token::With) {
            Some(self.parse_effect_row())
        } else {
            None
        };
        Type {
            span: self.span_from(start),
            kind: TypeKind::Function {
                params,
                ret: Box::new(ret),
                effects,
            },
        }
    }

    fn parse_fn_type_param(&mut self) -> FnTypeParam {
        let start = self.pos;
        let name =
            if matches!(self.peek_kind(), Token::Ident(_)) && self.peek_at(1).kind == Token::Colon {
                let id = self.expect_ident("expected parameter name");
                self.bump(); // `:`
                Some(id)
            } else {
                None
            };
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
        FnTypeParam {
            span: self.span_from(start),
            name,
            mode,
            ty,
        }
    }

    /// Parse a dotted identifier path: `i32`, `std.fs.read_to_string`.
    pub(crate) fn parse_path(&mut self) -> Path {
        let start = self.pos;
        let mut segments = Vec::new();
        segments.push(self.expect_ident("expected identifier in path"));
        while self.peek_kind() == Token::Dot {
            // Don't eat the dot until we know the next token is an ident:
            // `x.field` (in expression position) shares the same dot.
            if !matches!(self.peek_at(1).kind, Token::Ident(_)) {
                break;
            }
            self.bump(); // `.`
            segments.push(self.expect_ident("expected identifier after `.` in path"));
        }
        Path {
            span: self.span_from(start),
            segments,
        }
    }

    /// Parse a `with { ... }` effect row.
    pub(crate) fn parse_effect_row(&mut self) -> EffectRow {
        let start = self.pos;
        self.expect(Token::With, "expected `with` to introduce an effect row");
        self.expect(Token::LBrace, "expected `{` after `with`");
        let mut members = Vec::new();
        if !self.at(Token::RBrace) {
            members.push(self.parse_effect_member());
            while self.eat(Token::Comma) {
                if self.at(Token::RBrace) {
                    break;
                }
                members.push(self.parse_effect_member());
            }
        }
        self.expect(Token::RBrace, "expected `}` to close effect row");
        // §5.6 — mixing a graded and an ungraded entry of the same kind
        // (`alloc(bytes <= N)` alongside bare `alloc`) is a parse error.
        self.check_graded_mixing(&members);
        EffectRow {
            span: self.span_from(start),
            members,
        }
    }

    //            cannot appear both graded and ungraded in one row
    /// Detect violations of the graded/ungraded mixing rule (§5.6).
    /// Emits `effect_graded_bound_exceeded` on each Capability entry whose
    /// name collides with a Graded entry's kind.
    fn check_graded_mixing(&mut self, members: &[EffectMember]) {
        use std::collections::HashMap;
        let mut graded_spans: HashMap<edda_intern::Symbol, edda_span::Span> = HashMap::new();
        for m in members {
            if let EffectMember::Graded { kind, .. } = m {
                graded_spans.entry(kind.name).or_insert(kind.span);
            }
        }
        if graded_spans.is_empty() {
            return;
        }
        for m in members {
            if let EffectMember::Capability(name) = m {
                if graded_spans.contains_key(&name.name) {
                    let kind_str = self.interner.resolve(name.name).to_string();
                    self.emit_class(
                        DiagnosticClass::EffectGradedBoundExceeded,
                        name.span,
                        format!(
                            "row entry `{kind_str}` is both graded and ungraded — \
                             a row either bounds a resource or doesn't"
                        ),
                    );
                }
            }
        }
    }

    fn parse_effect_member(&mut self) -> EffectMember {
        if self.eat(Token::DotDotDot) {
            // `...Path` spread — splice in another row alias.
            let path = self.parse_path();
            return EffectMember::Spread(path);
        }
        let name = match self.parse_effect_member_name() {
            Some(id) => id,
            None => {
                let span = self.peek().span;
                self.emit_error(span, "expected effect member name");
                Ident {
                    name: edda_intern::Symbol::DUMMY,
                    span,
                }
            }
        };
        if self.eat(Token::Colon) {
            let ty = self.parse_ty();
            EffectMember::Named { name, ty }
        } else if self.at(Token::LParen) {
            // Graded form per `02-modes-effects-refinements.md` §5:
            // `kind(<bound>)`. The bound is an LIA refinement expression
            // over caller parameters; v0.1 admits `alloc`, `io`, `time`
            // as the kind. Semantic-layer enforcement (kind whitelist,
            // bound discharge) happens in edda-types.
            self.bump(); // `(`
            let bound = self.parse_expr_no_struct();
            self.expect(Token::RParen, "expected `)` to close graded bound");
            EffectMember::Graded {
                kind: name,
                bound: Box::new(bound),
            }
        } else {
            EffectMember::Capability(name)
        }
    }

    /// Pull an effect-member name token off the cursor. Accepts a bare
    /// identifier (capability parameter or user effect-kind name) and the
    /// locked pure-effect-kind keywords `panic` and `yield`, which the lexer
    /// tokenises as `Token::Panic` / `Token::Yield` but the effect-row
    /// grammar admits as bare names.
    fn parse_effect_member_name(&mut self) -> Option<Ident> {
        let lex = self.peek();
        let name = match lex.kind {
            Token::Ident(sym) => sym,
            Token::Panic => self.interner.intern("panic"),
            Token::Yield => self.interner.intern("yield"),
            _ => return None,
        };
        self.bump();
        Some(Ident {
            name,
            span: lex.span,
        })
    }

    /// Parse a `requires`/`ensures` refinement clause. The leading
    /// keyword must already be visible to the cursor.
    #[allow(dead_code)] // wired up when function items land
    pub(crate) fn parse_refinement_clause(&mut self) -> RefinementClause {
        let start = self.pos;
        let kind = match self.peek_kind() {
            Token::Requires => {
                self.bump();
                RefinementKind::Requires
            }
            Token::Ensures => {
                self.bump();
                RefinementKind::Ensures
            }
            Token::Where => {
                self.bump();
                RefinementKind::Where
            }
            Token::Decreases => {
                self.bump();
                RefinementKind::Decreases
            }
            _ => {
                let span = self.peek().span;
                self.emit_error(
                    span,
                    "expected `requires`, `ensures`, `decreases`, or `where` clause keyword",
                );
                RefinementKind::Where
            }
        };
        let pred = self.parse_expr_no_struct();
        RefinementClause {
            span: self.span_from(start),
            kind,
            pred,
        }
    }
}
