//! Item-level attribute parser.
//!
//! Attributes (`@name(args)`) attach to an item between its leading
//! doc-comments and its visibility / kind keyword. The grammar:
//!
//! ```text
//! attribute     = "@" ident "(" arg_list ")"
//! arg_list      = ( arg ( "," arg )* )?
//! arg           = ident "=" arg_value      // named
//!               | arg_value                // positional
//! arg_value     = ident | str | int | bool
//! ```
//!
//! Positional args precede named args; once a named arg appears, every
//! subsequent arg in the list must also be named. The parser admits any
//! well-formed `@name(args)` clause without consulting a registry —
//! `edda-types` validates names and arg shapes in a later slice.

use crate::ast::{AttrArg, AttrLit, Attribute, Ident};
use crate::token::Token;

use super::Parser;

impl<'a> Parser<'a> {
    /// Collect a leading run of `@name(args)` attributes.
    pub(super) fn parse_item_attributes(&mut self) -> Vec<Attribute> {
        let mut out = Vec::new();
        while self.at(Token::At) {
            out.push(self.parse_one_attribute());
        }
        out
    }

    fn parse_one_attribute(&mut self) -> Attribute {
        let start = self.pos;
        self.bump(); // `@`
        let name = self.expect_ident("expected attribute name after `@`");
        self.expect(Token::LParen, "expected `(` after attribute name");
        let mut args = Vec::new();
        let mut seen_named = false;
        if !self.at(Token::RParen) && !self.at_eof() {
            let arg_start = self.pos;
            let arg = self.parse_attr_arg(seen_named);
            seen_named |= matches!(arg, AttrArg::Named { .. });
            args.push(arg);
            // Defensive: force progress so a malformed arg cannot push
            // entries until the Vec OOMs.
            if self.pos == arg_start {
                self.bump();
            }
            while self.eat(Token::Comma) {
                if self.at(Token::RParen) || self.at_eof() {
                    break;
                }
                let inner_start = self.pos;
                let arg = self.parse_attr_arg(seen_named);
                seen_named |= matches!(arg, AttrArg::Named { .. });
                args.push(arg);
                if self.pos == inner_start {
                    self.bump();
                }
            }
        }
        self.expect(
            Token::RParen,
            "expected `)` to close attribute argument list",
        );
        Attribute {
            span: self.span_from(start),
            name,
            args,
        }
    }

    fn parse_attr_arg(&mut self, positional_must_be_named: bool) -> AttrArg {
        let start = self.pos;
        // Lookahead: `<ident> :` or `<ident> =` is a Named arg.
        let next = self.peek_at(1).kind;
        if matches!(self.peek_kind(), Token::Ident(_))
            && matches!(next, Token::Colon | Token::Eq)
        {
            let key = self.expect_ident("expected attribute argument key");
            self.bump(); // `:` or `=`
            let value = self.parse_attr_arg_value();
            return AttrArg::Named {
                span: self.span_from(start),
                key,
                value: Box::new(value),
            };
        }
        if positional_must_be_named {
            let span = self.peek().span;
            self.emit_error(
                span,
                "positional attribute argument cannot follow a named argument",
            );
        }
        self.parse_attr_arg_value()
    }

    fn parse_attr_arg_value(&mut self) -> AttrArg {
        let lex = self.peek();
        match lex.kind {
            Token::Ident(name) => {
                self.bump();
                AttrArg::Ident(Ident {
                    name,
                    span: lex.span,
                })
            }
            Token::Str(sym) => {
                self.bump();
                AttrArg::Lit {
                    span: lex.span,
                    lit: AttrLit::Str(sym),
                }
            }
            Token::Int { value, base } => {
                self.bump();
                AttrArg::Lit {
                    span: lex.span,
                    lit: AttrLit::Int { value, base },
                }
            }
            Token::True => {
                self.bump();
                AttrArg::Lit {
                    span: lex.span,
                    lit: AttrLit::Bool(true),
                }
            }
            Token::False => {
                self.bump();
                AttrArg::Lit {
                    span: lex.span,
                    lit: AttrLit::Bool(false),
                }
            }
            _ => {
                self.emit_error(
                    lex.span,
                    "expected attribute argument — identifier, string, integer, or boolean literal",
                );
                AttrArg::Ident(Ident {
                    name: edda_intern::Symbol::DUMMY,
                    span: lex.span,
                })
            }
        }
    }
}
