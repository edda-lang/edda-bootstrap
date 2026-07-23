//! Type-declaration parsing: `type` decls, product fields, and sum variants.


use crate::ast::{
    Linearity, Stability, TypeDecl,
    TypeDeclKind, TypeField, Variant, VariantPayload, Visibility,
};
use crate::token::Token;

use super::Parser;

impl<'a> Parser<'a> {
    pub(super) fn parse_type_decl(
        &mut self,
        stability: Option<Stability>,
        visibility: Visibility,
        linearity: Option<Linearity>,
    ) -> TypeDecl {
        let start = self.pos;
        self.bump(); // `type`
        let name = self.expect_ident("expected type name");
        self.expect(Token::LBrace, "expected `{` to open type body");
        self.skip_doc_trivia();
        let kind = if self.at(Token::Case) {
            let mut variants = Vec::new();
            loop {
                self.skip_doc_trivia();
                if !self.eat(Token::Case) {
                    break;
                }
                variants.push(self.parse_variant());
                // Variants can be comma-separated or newline-separated.
                self.eat(Token::Comma);
            }
            TypeDeclKind::Sum { variants }
        } else {
            let mut fields = Vec::new();
            while !self.at(Token::RBrace) && !self.at_eof() {
                self.skip_doc_trivia();
                if self.at(Token::RBrace) || self.at_eof() {
                    break;
                }
                let field_start = self.pos;
                fields.push(self.parse_type_field());
                self.eat(Token::Comma);
                if self.pos == field_start {
                    self.bump();
                }
            }
            TypeDeclKind::Product { fields }
        };
        self.expect(Token::RBrace, "expected `}` to close type body");
        TypeDecl {
            span: self.span_from(start),
            stability,
            visibility,
            linearity,
            name,
            generics: Vec::new(),
            kind,
        }
    }

    fn parse_type_field(&mut self) -> TypeField {
        let start = self.pos;
        let name = self.expect_ident("expected field name");
        self.expect(Token::Colon, "expected `:` after field name");
        let ty = self.parse_ty();
        let refinement = if self.eat(Token::Where) {
            Some(self.parse_expr_no_struct())
        } else {
            None
        };
        TypeField {
            span: self.span_from(start),
            name,
            ty,
            refinement,
        }
    }

    fn parse_variant(&mut self) -> Variant {
        let start = self.pos;
        let name = self.expect_ident("expected variant name");
        let payload = match self.peek_kind() {
            Token::LParen => {
                self.bump();
                // Two shapes: positional `case foo(T, U)` or named-tuple
                // `case foo(r: u8, g: u8)`. Disambiguate by peeking for
                // `<ident>:`.
                if matches!(self.peek_kind(), Token::Ident(_))
                    && self.peek_at(1).kind == Token::Colon
                {
                    let mut fields = Vec::new();
                    fields.push(self.parse_type_field());
                    while self.eat(Token::Comma) {
                        if self.at(Token::RParen) {
                            break;
                        }
                        fields.push(self.parse_type_field());
                    }
                    self.expect(Token::RParen, "expected `)` after variant fields");
                    VariantPayload::Struct(fields)
                } else {
                    let mut tys = Vec::new();
                    if !self.at(Token::RParen) {
                        tys.push(self.parse_ty());
                        while self.eat(Token::Comma) {
                            if self.at(Token::RParen) {
                                break;
                            }
                            tys.push(self.parse_ty());
                        }
                    }
                    self.expect(Token::RParen, "expected `)` after variant payload");
                    VariantPayload::Tuple(tys)
                }
            }
            Token::LBrace => {
                self.bump();
                let mut fields = Vec::new();
                while !self.at(Token::RBrace) && !self.at_eof() {
                    self.skip_doc_trivia();
                    if self.at(Token::RBrace) || self.at_eof() {
                        break;
                    }
                    let field_start = self.pos;
                    fields.push(self.parse_type_field());
                    self.eat(Token::Comma);
                    if self.pos == field_start {
                        self.bump();
                    }
                }
                self.expect(Token::RBrace, "expected `}` after variant fields");
                VariantPayload::Struct(fields)
            }
            _ => VariantPayload::Unit,
        };
        Variant {
            span: self.span_from(start),
            name,
            payload,
        }
    }
}
