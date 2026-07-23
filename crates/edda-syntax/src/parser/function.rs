//! Function-declaration parsing: signature, parameters, outbound generics,
//! the `extern "sym"` body-form, and the value-vs-comptime parameter split.

use edda_intern::Symbol;
use edda_span::Span;

use crate::ast::{
    AttrArg, AttrLit, Attribute, FnBody, FnDecl, GenericKind, GenericParam, Param, ParamMode, Stability, Visibility,
};
use crate::token::Token;

use super::Parser;

/// Outcome of [`Parser::parse_function_param`]: either a value parameter or a
/// `comptime <name>: <Type>` generic that lifts into the function's
/// `generics` list per `comptime.md` §102.
enum FnParamOrGeneric {
    Value(Param),
    Comptime(GenericParam),
}

impl<'a> Parser<'a> {
    pub(super) fn parse_function(
        &mut self,
        stability: Option<Stability>,
        visibility: Visibility,
        refinement_stable: bool,
        abi_target: Option<(Span, Symbol)>,
    ) -> FnDecl {
        let start = self.pos;
        self.bump(); // `function`
        let name = self.expect_ident("expected function name");
        let outbound_generics = if self.at(Token::Lt) {
            self.parse_outbound_generics()
        } else {
            Vec::new()
        };
        self.expect(Token::LParen, "expected `(` after function name");
        let mut params = Vec::new();
        let mut generics = Vec::new();
        if !self.at(Token::RParen) {
            self.push_function_param(&mut params, &mut generics);
            while self.eat(Token::Comma) {
                if self.at(Token::RParen) {
                    break;
                }
                self.push_function_param(&mut params, &mut generics);
            }
        }
        self.expect(Token::RParen, "expected `)` after function parameters");
        // Return type is mandatory per declarations.md §116. We still
        // produce an `Error` sentinel type when omitted so the AST stays
        // well-formed and downstream passes do not re-diagnose.
        // An optional borrow-mode keyword sits between `->` and the
        // return type: `-> let T` / `-> mutable T`. No keyword is the
        // by-value default. `take`/`init` are parameter-only modes and
        // are not admitted here.
        let mut return_mode = crate::ast::ReturnMode::ByValue;
        let return_ty = if self.eat(Token::Arrow) {
            return_mode = match self.peek_kind() {
                Token::Let => {
                    self.bump();
                    crate::ast::ReturnMode::Let
                }
                Token::Mutable => {
                    self.bump();
                    crate::ast::ReturnMode::Mutable
                }
                _ => crate::ast::ReturnMode::ByValue,
            };
            Some(self.parse_ty())
        } else {
            let span = self.peek().span;
            self.emit_error(
                span,
                "expected `->` — function return type is mandatory",
            );
            Some(crate::ast::Type {
                span,
                kind: crate::ast::TypeKind::Error,
            })
        };
        let effects = if self.at(Token::With) {
            Some(self.parse_effect_row())
        } else {
            None
        };
        let mut refinements = Vec::new();
        while matches!(
            self.peek_kind(),
            Token::Requires | Token::Ensures | Token::Decreases | Token::Where
        ) {
            refinements.push(self.parse_refinement_clause());
        }
        let body = self.parse_fn_body(abi_target);
        FnDecl {
            span: self.span_from(start),
            stability,
            visibility,
            refinement_stable,
            name,
            outbound_generics,
            generics,
            params,
            return_ty,
            return_mode,
            effects,
            refinements,
            body,
        }
    }

    fn parse_fn_body(&mut self, abi_target: Option<(Span, Symbol)>) -> FnBody {
        if self.at(Token::Extern) {
            let extern_start = self.pos;
            self.bump(); // `extern`
            let lit_lexed = self.peek();
            match lit_lexed.kind {
                Token::Str(sym) => {
                    self.bump();
                    let dll = self.parse_extern_from_clause();
                    FnBody::Extern {
                        span: self.span_from(extern_start),
                        name_span: lit_lexed.span,
                        name: sym,
                        dll,
                    }
                }
                _ => {
                    self.emit_error(
                        lit_lexed.span,
                        "expected `\"symbol\"` string literal after `extern`",
                    );
                    FnBody::Extern {
                        span: self.span_from(extern_start),
                        name_span: lit_lexed.span,
                        name: edda_intern::Symbol::DUMMY,
                        dll: None,
                    }
                }
            }
        } else if self.at(Token::LBrace) {
            FnBody::Block(self.parse_block())
        } else if let Some((name_span, name)) = abi_target {
            // `@abi("name")` no longer licenses bodylessness — the
            // external-implementation fact is structural and belongs in the
            // body slot as `extern "name"`. Diagnose, then recover by
            // synthesising the `Extern` body from the attribute payload so
            // downstream passes (resolver / typechecker / MIR lowering)
            // stay on the no-body path without re-diagnosing.
            self.emit_error(
                name_span,
                "`@abi(\"name\")` does not license a bodyless function — write the `extern \"name\"` body-form in the body slot",
            );
            let here = self.peek().span;
            FnBody::Extern {
                span: Span::new(here.file, here.lo, here.lo),
                name_span,
                name,
                dll: None,
            }
        } else {
            // No `{`, no `extern`, no `@abi` — original "expected body"
            // diagnostic surfaced by `parse_block`'s recovery.
            FnBody::Block(self.parse_block())
        }
    }

    /// Parse the optional `from "dll"` clause after an extern-symbol
    /// literal. Returns the interned DLL
    /// name, or `None` when no `from` clause is present.
    fn parse_extern_from_clause(&mut self) -> Option<Symbol> {
        match self.peek_kind() {
            Token::Ident(sym) if self.interner.resolve(sym) == "from" => {}
            _ => return None,
        }
        self.bump(); // `from` (contextual identifier)
        let lit_lexed = self.peek();
        match lit_lexed.kind {
            Token::Str(sym) => {
                self.bump();
                Some(sym)
            }
            _ => {
                self.emit_error(
                    lit_lexed.span,
                    "expected `\"dll\"` string literal after `from`",
                );
                None
            }
        }
    }

    /// Extract the `@abi("name")` payload from an item's attribute list.
    /// Returns `Some((name_span, name))` for the first `@abi(<string
    /// literal>)` entry; `None` otherwise.
    pub(super) fn abi_target_from_attributes(
        &self,
        attrs: &[Attribute],
    ) -> Option<(Span, Symbol)> {
        for attr in attrs {
            let attr_name = self.interner.resolve(attr.name.name);
            if attr_name != "abi" {
                continue;
            }
            for arg in &attr.args {
                if let AttrArg::Lit {
                    span,
                    lit: AttrLit::Str(sym),
                } = arg
                {
                    return Some((*span, *sym));
                }
            }
        }
        None
    }

    fn parse_outbound_generics(&mut self) -> Vec<GenericParam> {
        self.bump(); // `<`
        let mut generics = Vec::new();
        if !self.at(Token::Gt) {
            generics.push(self.parse_outbound_generic_entry());
            while self.eat(Token::Comma) {
                if self.at(Token::Gt) {
                    break;
                }
                generics.push(self.parse_outbound_generic_entry());
            }
        }
        self.expect(Token::Gt, "expected `>` to close outbound generic list");
        generics
    }

    fn parse_outbound_generic_entry(&mut self) -> GenericParam {
        let start = self.pos;
        if !self.eat(Token::Comptime) {
            let span = self.peek().span;
            self.emit_error(
                span,
                "expected `comptime` — outbound generic parameters must be comptime per phase-2-locks Gap 3",
            );
        }
        let name = self.expect_ident("expected outbound generic parameter name");
        self.expect(
            Token::Colon,
            "expected `:` after outbound generic parameter name",
        );
        let ty = self.parse_ty_no_where();
        GenericParam {
            span: self.span_from(start),
            kind: GenericKind::Comptime,
            name,
            ty: Some(ty),
            admits: Vec::new(),
        }
    }

    /// Dispatch the next function-parameter entry into either `params` or
    /// `generics` based on whether the entry starts with `comptime`.
    fn push_function_param(
        &mut self,
        params: &mut Vec<Param>,
        generics: &mut Vec<GenericParam>,
    ) {
        match self.parse_function_param() {
            FnParamOrGeneric::Value(p) => params.push(p),
            FnParamOrGeneric::Comptime(g) => generics.push(g),
        }
    }

    fn parse_function_param(&mut self) -> FnParamOrGeneric {
        let start = self.pos;
        let is_comptime = self.eat(Token::Comptime);
        let name = self.expect_ident("expected parameter name");
        self.expect(Token::Colon, "expected `:` after parameter name");
        if is_comptime {
            // `comptime <name>: <Type>` lifts into FnDecl.generics. Mode
            // prefixes are not admitted on a comptime generic.
            let ty = self.parse_ty();
            return FnParamOrGeneric::Comptime(GenericParam {
                span: self.span_from(start),
                kind: GenericKind::Comptime,
                name,
                ty: Some(ty),
                admits: Vec::new(),
            });
        }
        // The mode prefix sits *between* the colon and the type:
        // `buf: mutable TokenBuf`. Default mode (no prefix) is the
        // language's immutable-by-reference read-only mode.
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
        let ty = self.parse_ty();
        FnParamOrGeneric::Value(Param {
            span: self.span_from(start),
            name,
            mode,
            ty,
        })
    }
}
