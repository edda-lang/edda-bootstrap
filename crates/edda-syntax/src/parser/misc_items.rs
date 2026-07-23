//! Miscellaneous top-level items: `import` (with `.{...}` selection
//! and/or `as` alias), `module` overrides, and `derive ... for <Path>`
//! declarations.


use crate::ast::{
    Ident, Import, ModuleDecl,
};
use crate::token::Token;

use super::Parser;

impl<'a> Parser<'a> {
    /// Parse an `import dot.path` declaration, optionally followed by a
    /// `.{name, name, ...}` selected-name clause
    /// (`import std.os.fs.{read, write}`) and/or an `as <ident>` alias
    /// clause (`import std.core.cmp as ccmp`) — the two are mutually
    /// exclusive.
    pub(super) fn parse_import(&mut self) -> Import {
        let start = self.pos;
        self.bump(); // `import`
        let path = self.parse_path();

        let selection = if self.at(Token::Dot) && matches!(self.peek_at(1).kind, Token::LBrace) {
            self.bump(); // `.`
            self.bump(); // `{`
            Some(self.parse_import_selection())
        } else {
            None
        };

        let alias = if self.eat(Token::As) {
            let id = self.expect_ident("expected alias name after `as`");
            if selection.is_some() {
                self.emit_error(
                    id.span,
                    "`as <alias>` and `.{...}` selection are mutually exclusive on the same `import`",
                );
            }
            Some(id)
        } else {
            None
        };
        Import {
            span: self.span_from(start),
            path,
            alias,
            selection,
        }
    }

    /// Parse the comma-separated identifier list of a `.{name, name,
    /// ...}` selected-name import clause. The opening `{` has already
    /// been consumed by the caller.
    fn parse_import_selection(&mut self) -> Vec<Ident> {
        let mut names = Vec::new();
        if self.at(Token::RBrace) {
            let span = self.peek().span;
            self.emit_error(
                span,
                "selected-name import must name at least one entry between `{` and `}`",
            );
            self.bump();
            return names;
        }
        loop {
            match self.eat_ident() {
                Some(id) => names.push(id),
                None => {
                    let span = self.peek().span;
                    self.emit_error(span, "expected identifier in selected-name import");
                    break;
                }
            }
            if self.eat(Token::Comma) {
                if self.at(Token::RBrace) || self.at_eof() {
                    break;
                }
            } else {
                break;
            }
        }
        self.expect(Token::RBrace, "expected `}` to close selected-name import");
        names
    }

    pub(super) fn parse_module(&mut self) -> ModuleDecl {
        let start = self.pos;
        self.bump(); // `module`
        let path = self.parse_path();
        ModuleDecl {
            span: self.span_from(start),
            path,
        }
    }

    /// Parse a `derive <ident>, <ident>, ... for <Path>` top-level
    /// declaration. The leading `derive` keyword must be at the cursor.
    pub(super) fn parse_derive(&mut self) -> crate::ast::Derive {
        let start = self.pos;
        self.bump(); // `derive`
        let mut items = Vec::new();
        items.push(self.expect_ident("expected derive item name (e.g. `eq`, `hash`, `properties`)"));
        while self.eat(Token::Comma) {
            // Tolerate trailing comma before `for`.
            if self.at(Token::For) {
                break;
            }
            items.push(self.expect_ident("expected derive item name after `,`"));
        }
        if !self.eat(Token::For) {
            let span = self.peek().span;
            self.emit_error(
                span,
                "expected `for` between derive item list and target type",
            );
            let dummy = crate::ast::Ident {
                name: edda_intern::Symbol::DUMMY,
                span,
            };
            return crate::ast::Derive {
                span: self.span_from(start),
                items,
                target: crate::ast::Path {
                    segments: vec![dummy],
                    span,
                },
            };
        }
        let target = self.parse_path();
        crate::ast::Derive {
            span: self.span_from(start),
            items,
            target,
        }
    }
}
