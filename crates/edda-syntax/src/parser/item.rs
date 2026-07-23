//! Top-level item parser: functions, type declarations, specs, imports,
//! and rare `module` overrides. Plus the file-level entry point that
//! collects doc-comments and assembles the parsed `File`.

use edda_span::Span;

use crate::ast::{
    DocLine, DocTier, File,
    Import, Item, ItemKind, LetDecl, Linearity, Stability, Visibility,
};
use crate::token::Token;

use super::Parser;

impl<'a> Parser<'a> {
    /// Parse a complete source file: the file-head `//!` block (if any),
    /// then a sequence of items each carrying its own preceding doc-comment
    /// block (all four tiers admitted at item position).
    pub fn parse_file(&mut self) -> File {
        let start_pos = self.pos;
        let file_doc = self.collect_file_head_doc_block();
        let mut items = Vec::new();
        while !self.at_eof() {
            // Doc-comments preceding an item attach to it.
            let item_doc = self.collect_item_doc_block();
            if self.at_eof() {
                // Trailing doc-comment block with no item to attach to.
                // Drop on the floor — the AST has no carrier for these.
                break;
            }
            let item = self.parse_item(item_doc);
            items.push(item);
        }
        File {
            span: self.span_from(start_pos),
            doc: file_doc,
            items,
        }
    }

    /// File-head doc-block: consume the `//!` run that opens the file.
    /// Other tiers at the file head fall through and attach to the first
    /// item per the locked file-head rule.
    pub(super) fn collect_file_head_doc_block(&mut self) -> Vec<DocLine> {
        let mut lines = Vec::new();
        while let Token::InnerDoc(sym) = self.peek_kind() {
            let span = self.peek().span;
            lines.push(DocLine {
                tier: DocTier::Low,
                span,
                body: sym,
            });
            self.bump();
        }
        lines
    }

    /// Item-attached doc-block: consume any contiguous run of doc-comment
    /// tokens at the cursor and return one [`DocLine`] per line in source
    /// order, tier-tagged per `01-syntax.md` §3.2.
    pub(super) fn collect_item_doc_block(&mut self) -> Vec<DocLine> {
        let mut lines = Vec::new();
        loop {
            let lexed = self.peek();
            let (tier, body) = match lexed.kind {
                Token::DocHigh(sym) => (DocTier::High, sym),
                Token::DocMed(sym) => (DocTier::Medium, sym),
                Token::InnerDoc(sym) => (DocTier::Low, sym),
                Token::OuterDoc(sym) => (DocTier::Legacy, sym),
                _ => break,
            };
            lines.push(DocLine {
                tier,
                span: lexed.span,
                body,
            });
            self.bump();
        }
        lines
    }

    pub(super) fn parse_item(&mut self, doc: Vec<DocLine>) -> Item {
        let start = self.pos;
        let attributes = self.parse_item_attributes();
        // Single-job-keyword (§3.7 / D-19): API stability is the
        // `stable` / `unstable` keyword on `function` and `type`
        // declarations — `@stable` / `@unstable` are not attributes
        // (`edda-types` rejects them as `unknown_attribute`). The
        // keyword may lead the declaration (`stable type T`) or sit in
        // post-visibility position (`public stable type T`, the form
        // the stdlib uses); `eat_stability` captures the leading form
        // and the post-visibility form is folded in below.
        let stability = self.eat_stability();
        // `@abi("name")` never substitutes for a body — pre-extract
        // it so `parse_fn_body` can diagnose a bodyless `@abi` function
        // (and recover with the attribute's symbol). `None` for any item
        // that doesn't carry `@abi` (the common case).
        let abi_target = self.abi_target_from_attributes(&attributes);
        let visibility = if self.eat(Token::Public) {
            Visibility::Public
        } else {
            Visibility::Module
        };
        // §7 — a contextual `stable` between visibility and `function`
        // marks the function refinement-stable. Orthogonal to API
        // stability; admitted only before `function` (not `type`,
        // `spec`, etc.).
        let refinement_stable = if matches!(self.stability_kw_at(0), Some(Stability::Stable { .. }))
            && matches!(self.peek_at(1).kind, Token::Function)
        {
            self.bump();
            true
        } else {
            false
        };
        // §3.7 / D-19 — a `stable` / `unstable` keyword between
        // visibility and `type` is the type's API-stability modifier
        // (`public stable type T`). The leading and post-visibility
        // positions are mutually exclusive on a well-formed item, so
        // `or_else` simply takes whichever fired.
        let stability = stability.or_else(|| self.eat_type_stability());
        // Linearity modifier (`affine` / `linear`) sits between
        // visibility and `type` per the codex's first-class
        // linear/affine discipline. Admitted only before `type`; on
        // any other item kind the parser leaves the token alone so the
        // dispatch below reports an `expected item` diagnostic.
        let linearity = self.eat_linearity();
        let kind = match self.peek_kind() {
            Token::Function => ItemKind::Function(Box::new(self.parse_function(
                stability,
                visibility,
                refinement_stable,
                abi_target,
            ))),
            Token::Type => ItemKind::TypeDecl(Box::new(self.parse_type_decl(
                stability,
                visibility,
                linearity,
            ))),
            Token::Spec => self.parse_spec_or_invocation(stability, visibility),
            Token::Let => ItemKind::Let(Box::new(self.parse_let_item(visibility))),
            Token::Import => ItemKind::Import(self.parse_import()),
            Token::Module => ItemKind::Module(self.parse_module()),
            Token::Derive => ItemKind::Derive(Box::new(self.parse_derive())),
            _ => {
                let span = self.peek().span;
                self.emit_error(
                    span,
                    "expected item — `function`, `type`, `spec`, `let`, `import`, `module`, or `derive`",
                );
                // Recover by skipping to the next plausible item keyword.
                self.recover_to(&[
                    Token::Function,
                    Token::Type,
                    Token::Spec,
                    Token::Let,
                    Token::Import,
                    Token::Module,
                    Token::Derive,
                    Token::Public,
                    Token::At,
                    Token::Eof,
                ]);
                ItemKind::Import(Import {
                    span,
                    path: crate::ast::Path {
                        segments: vec![crate::ast::Ident {
                            name: edda_intern::Symbol::DUMMY,
                            span,
                        }],
                        span,
                    },
                    alias: None,
                    selection: None,
                })
            }
        };
        Item {
            span: self.span_from(start),
            doc,
            attributes,
            kind,
        }
    }

    /// API-stability discriminant for the contextual `stable` /
    /// `unstable` identifier at `offset`, or `None` when that token is
    /// not one of them. The returned [`Stability`] is `Copy`, so the
    /// caller holds no borrow and may freely [`Self::bump`].
    fn stability_kw_at(&self, offset: usize) -> Option<Stability> {
        let Token::Ident(sym) = self.peek_at(offset).kind else {
            return None;
        };
        match self.interner.try_resolve(sym)? {
            "stable" => Some(Stability::Stable { since: None }),
            "unstable" => Some(Stability::Unstable { since: None }),
            _ => None,
        }
    }

    /// Consume an optional leading `stable` / `unstable` modifier at
    /// the cursor (before any visibility keyword).
    ///
    /// Per §3.7 the contextual keyword is the sole source of API
    /// stability on `function` and `type` declarations; `@stable` /
    /// `@unstable` are not attributes. A leading `stable function` (no
    /// visibility) is left untouched — that spelling is the §7
    /// refinement-stability marker, consumed post-visibility by
    /// [`Self::parse_item`].
    fn eat_stability(&mut self) -> Option<Stability> {
        let stability = self.stability_kw_at(0)?;
        // `stable function` is the refinement-stability marker —
        // leave it for the post-visibility lookahead.
        if matches!(stability, Stability::Stable { .. })
            && matches!(self.peek_at(1).kind, Token::Function)
        {
            return None;
        }
        self.bump();
        Some(stability)
    }

    /// Consume the optional `affine` / `linear` linearity modifier at
    /// the cursor when it precedes a `type` declaration. Returns
    /// `None` (and leaves the cursor untouched) when the next token
    /// is neither keyword, or when the keyword is followed by
    /// anything other than `type` — in that case the item-dispatch
    /// path will report the malformed item.
    fn eat_linearity(&mut self) -> Option<Linearity> {
        let linearity = match self.peek_kind() {
            Token::Affine => Linearity::Affine,
            Token::Linear => Linearity::Linear,
            _ => return None,
        };
        if !matches!(self.peek_at(1).kind, Token::Type) {
            return None;
        }
        self.bump();
        Some(linearity)
    }

    /// Consume a post-visibility `stable` / `unstable` keyword that
    /// modifies a `type` declaration (`public stable type T`,
    /// `public unstable affine type T`) and return its API stability.
    ///
    /// Returns `None` (cursor untouched) when the next token is neither
    /// keyword, or when the keyword does not introduce a `type` — in
    /// particular `stable function` is left for the refinement-stability
    /// lookahead in [`Self::parse_item`].
    fn eat_type_stability(&mut self) -> Option<Stability> {
        let stability = self.stability_kw_at(0)?;
        let next = self.peek_at(1).kind;
        let introduces_type = matches!(next, Token::Type)
            || (matches!(next, Token::Affine | Token::Linear)
                && matches!(self.peek_at(2).kind, Token::Type));
        if !introduces_type {
            return None;
        }
        self.bump();
        Some(stability)
    }

    fn parse_let_item(&mut self, visibility: Visibility) -> LetDecl {
        let start = self.pos;
        self.bump(); // `let`
        let name = self.expect_ident("expected binding name after `let`");
        self.expect(
            Token::Colon,
            "expected `:` — module-level `let` requires a type annotation",
        );
        let ty = self.parse_ty();
        self.expect(
            Token::Eq,
            "expected `=` — module-level `let` requires an initialiser",
        );
        let init = self.parse_expr();
        LetDecl {
            span: self.span_from(start),
            visibility,
            name,
            ty,
            init,
        }
    }
}

/// Parse a full source file. The token stream must terminate in
/// [`Token::Eof`] (the lexer always provides this).
pub fn parse_file(
    tokens: &[crate::token::Lexed],
    interner: &edda_intern::Interner,
    diags: &mut edda_diag::Diagnostics,
    lint_cfg: &edda_diag::LintConfig,
) -> File {
    let mut parser = Parser::new(tokens, interner, diags, lint_cfg);
    let file = parser.parse_file();
    if !parser.at_eof() {
        let span: Span = parser.peek().span;
        parser.emit_error(span, "expected end of input after file");
    }
    file
}
