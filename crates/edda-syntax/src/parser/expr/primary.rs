//! Primary-expression dispatch: literals, paren/unit/tuple forms,
//! identifier-headed forms (path / struct-literal / spawn-block), the
//! `with { ... }` effect-row literal, and the `result` binder. Split out
//! from the Pratt core for file-size reasons; every method extends the
//! same `impl Parser` block.

use edda_intern::Symbol;
use edda_span::{BytePos, FileId, Span};

use crate::ast::{CallMode, Expr, ExprKind, FStringPart, Literal, StructLitField};
use crate::token::{Lexed, Token};

use super::super::Parser;

impl<'a> Parser<'a> {
    pub(super) fn parse_primary(&mut self, allow_struct: bool) -> Expr {
        match self.peek_kind() {
            Token::Int { value, base } => {
                let lex = self.bump();
                Expr {
                    span: lex.span,
                    kind: ExprKind::Literal(Literal::Int { value, base }),
                }
            }
            Token::Float(sym) => {
                let lex = self.bump();
                Expr {
                    span: lex.span,
                    kind: ExprKind::Literal(Literal::Float(sym)),
                }
            }
            Token::Str(sym) => {
                let lex = self.bump();
                Expr {
                    span: lex.span,
                    kind: ExprKind::Literal(Literal::Str(sym)),
                }
            }
            Token::FString(sym) => {
                let lex = self.bump();
                let parts = self.parse_fstring_parts(sym, lex.span);
                Expr {
                    span: lex.span,
                    kind: ExprKind::FString(parts),
                }
            }
            Token::True => {
                let lex = self.bump();
                Expr {
                    span: lex.span,
                    kind: ExprKind::Literal(Literal::Bool(true)),
                }
            }
            Token::False => {
                let lex = self.bump();
                Expr {
                    span: lex.span,
                    kind: ExprKind::Literal(Literal::Bool(false)),
                }
            }
            Token::LParen => self.parse_paren_unit_or_tuple(),
            Token::LBracket => self.parse_array_literal(),
            Token::LBrace => {
                let block = self.parse_block();
                Expr {
                    span: block.span,
                    kind: ExprKind::Block(block),
                }
            }
            Token::If => self.parse_if(),
            Token::Match => self.parse_match(),
            Token::Loop => self.parse_loop(),
            Token::For => self.parse_for(),
            Token::Comptime => self.parse_comptime(),
            Token::Function => self.parse_closure_literal(),
            Token::ScopeKw => self.parse_scope(),
            Token::Return => self.parse_return(),
            Token::Break => self.parse_break(),
            Token::Continue => self.parse_continue(),
            Token::Raise => self.parse_raise(),
            Token::Panic => self.parse_panic(),
            Token::Forall => self.parse_forall(),
            Token::Exists => self.parse_exists(),
            Token::Ident(sym) if self.interner.resolve(sym) == "handle" => {
                // `handle` is a contextual keyword introducing either
                // `handle <effect-label>: <Type> [as <binder>] -> <recovery> { <body> }`
                // (typed payload, e.g. `err`) or the payload-less
                // `handle <effect-label> -> <recovery> { <body> }` form
                // (`cancellation`, `divergence`). When the cursor is at
                // `handle` followed by `<ident> :` or `<ident> ->`,
                // commit to the handler form; otherwise fall back to
                // normal identifier parsing so user code that binds the
                // name `handle` (e.g. `handle.await`, `handle()`) parses.
                if matches!(self.peek_at(1).kind, Token::Ident(_))
                    && matches!(self.peek_at(2).kind, Token::Colon | Token::Arrow)
                {
                    self.parse_handle_expr()
                } else {
                    self.parse_ident_form(allow_struct)
                }
            }
            Token::Ident(_) => self.parse_ident_form(allow_struct),
            // `await` is admitted as an expression-position identifier so
            // call-site uses (`await(take t)`) and path uses (`task.await`
            // via UFCS) work. The postfix-suffix form `expr.await` is
            // handled earlier in `parse_postfix` and lowers to `ExprKind::Await`.
            Token::Await => self.parse_ident_form(allow_struct),
            Token::DotDot | Token::DotDotLt | Token::DotDotEq => self.parse_prefix_range(),
            Token::With => {
                // `with { ... }` as a comptime literal of type `EffectRow`.
                // Spec lock: `spec-language.md` §136 ("EffectRow literal
                // syntax: with { ... } in expression position"); the canonical
                // surface for row aliases is the module-level
                // `let X: EffectRow = with { ... }` form per
                // `effect-tracking.md` §234 + `spec-language.md` §165.
                // Comptime-pure-position enforcement is downstream
                // (in `edda-types`).
                let row = self.parse_effect_row();
                Expr {
                    span: row.span,
                    kind: ExprKind::EffectRow(row),
                }
            }
            Token::ResultKw => {
                // The `result` keyword is the postcondition return-value
                // binder. Surface it to the AST as an identifier path so
                // resolution treats it like any other binding.
                let lex = self.bump();
                let name = self.interner.intern("result");
                Expr {
                    span: lex.span,
                    kind: ExprKind::Path(crate::ast::Path {
                        segments: vec![crate::ast::Ident {
                            name,
                            span: lex.span,
                        }],
                        span: lex.span,
                    }),
                }
            }
            _ => {
                let span = self.peek().span;
                self.emit_error(span, "expected expression");
                self.err_expr(span)
            }
        }
    }

    fn parse_paren_unit_or_tuple(&mut self) -> Expr {
        let start = self.pos;
        self.bump(); // `(`
        if self.eat(Token::RParen) {
            return Expr {
                span: self.span_from(start),
                kind: ExprKind::Literal(Literal::Unit),
            };
        }
        let first = self.parse_expr();
        if self.eat(Token::RParen) {
            return first;
        }
        let mut elems = vec![first];
        while self.eat(Token::Comma) {
            if self.at(Token::RParen) {
                break;
            }
            elems.push(self.parse_expr());
        }
        self.expect(Token::RParen, "expected `)` after tuple expression");
        Expr {
            span: self.span_from(start),
            kind: ExprKind::Tuple(elems),
        }
    }

    fn parse_array_literal(&mut self) -> Expr {
        let start = self.pos;
        self.bump(); // `[`
        let mut elems = Vec::new();
        while !self.at(Token::RBracket) && !self.at_eof() {
            elems.push(self.parse_expr());
            if !self.eat(Token::Comma) {
                break;
            }
        }
        self.expect(Token::RBracket, "expected `]` to close array literal");
        Expr {
            span: self.span_from(start),
            kind: ExprKind::Array(elems),
        }
    }

    fn parse_ident_form(&mut self, allow_struct: bool) -> Expr {
        let path = self.parse_path();

        // Spawn-block disambiguation. Must run before the struct-literal
        // check because both forms start with `{` after a path; spawn-block
        // is the locked structured-concurrency surface from
        // `effects.md` §"Structured concurrency". The `allow_struct` flag
        // does not gate spawn-block — `if cond { ... }` cannot be confused
        // with `if (group.spawn { body }) { outer }` because the scope-
        // binder convention requires a two-segment path ending in `spawn`.
        if self.looks_like_spawn_block(&path) {
            return self.parse_spawn_tail(path);
        }

        if allow_struct && self.at(Token::LBrace) {
            return self.parse_struct_literal_tail(path);
        }
        Expr {
            span: path.span,
            kind: ExprKind::Path(path),
        }
    }

    fn parse_struct_literal_tail(&mut self, path: crate::ast::Path) -> Expr {
        let start_span = path.span;
        self.bump(); // `{`
        let mut fields = Vec::new();
        while !self.at(Token::RBrace) && !self.at_eof() {
            let field_start = self.pos;
            let name = self.expect_ident("expected field name in struct literal");
            let (mode, value) = if self.eat(Token::Colon) {
                // An explicit ownership/borrow keyword may prefix the
                // value (`x: take p`).
                // Mirrors `parse_call_arg`'s mode prefix.
                let mode = match self.peek_kind() {
                    Token::Take => {
                        self.bump();
                        Some(CallMode::Take)
                    }
                    Token::Mutable => {
                        self.bump();
                        Some(CallMode::Mutable)
                    }
                    Token::Init => {
                        self.bump();
                        Some(CallMode::Init)
                    }
                    _ => None,
                };
                (mode, self.parse_expr())
            } else {
                // Shorthand `{ name }` ≡ `{ name: name }`.
                (
                    None,
                    Expr {
                        span: name.span,
                        kind: ExprKind::Path(crate::ast::Path {
                            segments: vec![name],
                            span: name.span,
                        }),
                    },
                )
            };
            fields.push(StructLitField {
                span: self.span_from(field_start),
                name,
                mode,
                value,
            });
            if !self.eat(Token::Comma) {
                break;
            }
        }
        self.expect(Token::RBrace, "expected `}` to close struct literal");
        let end_span = self.tokens[self.pos.saturating_sub(1)].span;
        Expr {
            span: Span::new(start_span.file, start_span.lo, end_span.hi),
            kind: ExprKind::StructLit { path, fields },
        }
    }

    /// Split an `f"..."` payload into its [`FStringPart`] sequence,
    /// parsing each `{ ... }` slot as a full expression.
    fn parse_fstring_parts(&mut self, raw_sym: Symbol, fstring_span: Span) -> Vec<FStringPart> {
        let raw = self.interner.resolve(raw_sym).to_string();
        // The payload begins two bytes past the token start (`f"`).
        let payload_base = fstring_span.lo.0 + 2;
        let file = fstring_span.file;
        let bytes = raw.as_bytes();
        let mut parts = Vec::new();
        let mut text_start = 0usize;
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] == b'\\' && i + 1 < bytes.len() {
                i += 2;
                continue;
            }
            if bytes[i] != b'{' {
                i += 1;
                continue;
            }
            if i > text_start {
                let sym = self.unescape_fstring_text(&raw[text_start..i], payload_base + text_start as u32, file);
                parts.push(FStringPart::Text(sym));
            }
            let slot_start = i + 1;
            let Some(close) = find_slot_close(bytes, slot_start) else {
                self.emit_error(fstring_span, "unclosed `{` in f-string interpolation slot");
                return parts;
            };
            let slot_base = payload_base + slot_start as u32;
            let slot_expr = self.parse_fstring_slot(&raw[slot_start..close], file, slot_base, fstring_span);
            parts.push(FStringPart::Slot(Box::new(slot_expr)));
            i = close + 1;
            text_start = i;
        }
        if text_start < bytes.len() {
            let sym = self.unescape_fstring_text(&raw[text_start..], payload_base + text_start as u32, file);
            parts.push(FStringPart::Text(sym));
        }
        parts
    }

    /// Decode a literal f-string text segment's escape sequences
    /// (bootstrap parity: f-string literal segments use the
    /// same escape table as a plain string literal) and intern the
    /// decoded text.
    fn unescape_fstring_text(&mut self, text: &str, base: u32, file: FileId) -> Symbol {
        if !text.as_bytes().contains(&b'\\') {
            return self.interner.intern(text);
        }
        let bytes = text.as_bytes();
        let mut out = String::with_capacity(text.len());
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] != b'\\' {
                let ch = text[i..].chars().next().expect("text is valid utf8");
                out.push(ch);
                i += ch.len_utf8();
                continue;
            }
            let esc_start = i;
            i += 1;
            let Some(&esc) = bytes.get(i) else {
                let span = Span::new(file, BytePos(base + esc_start as u32), BytePos(base + i as u32));
                self.emit_error(span, "unterminated escape sequence");
                break;
            };
            i += 1;
            match esc {
                b'\\' => out.push('\\'),
                b'"' => out.push('"'),
                b'{' => out.push('{'),
                b'}' => out.push('}'),
                b'n' => out.push('\n'),
                b'r' => out.push('\r'),
                b't' => out.push('\t'),
                b'0' => out.push('\0'),
                b'x' => i = self.unescape_fstring_hex(bytes, i, esc_start, base, file, &mut out),
                other => {
                    let span = Span::new(file, BytePos(base + esc_start as u32), BytePos(base + i as u32));
                    self.emit_error(span, format!("unknown escape sequence `\\{}`", other as char));
                }
            }
        }
        self.interner.intern(&out)
    }

    /// Decode a `\xHH` escape inside an f-string text segment — parity
    /// with `Lexer::lex_hex_escape` (exactly two hex digits, value in
    /// `0x00..=0x7F`). Returns the byte index past the consumed digits.
    fn unescape_fstring_hex(
        &mut self,
        bytes: &[u8],
        mut i: usize,
        esc_start: usize,
        base: u32,
        file: FileId,
        out: &mut String,
    ) -> usize {
        let digits_start = i;
        let mut value: u32 = 0;
        for _ in 0..2 {
            let Some(&b) = bytes.get(i) else { break };
            let Some(d) = (b as char).to_digit(16) else { break };
            i += 1;
            value = value * 16 + d;
        }
        let span = Span::new(file, BytePos(base + esc_start as u32), BytePos(base + i as u32));
        if i - digits_start != 2 {
            self.emit_error(span, "`\\x` escape requires exactly two hex digits");
            return i;
        }
        if value > 0x7F {
            self.emit_error(span, "`\\x` escape must be in range 0x00..=0x7F");
            return i;
        }
        out.push(value as u8 as char);
        i
    }

    /// Lex + parse one interpolation slot's text into a full [`Expr`].
    fn parse_fstring_slot(
        &mut self,
        slot_text: &str,
        file: FileId,
        base: u32,
        fstring_span: Span,
    ) -> Expr {
        let lead = (slot_text.len() - slot_text.trim_start().len()) as u32;
        let trimmed = slot_text.trim();
        if trimmed.is_empty() {
            self.emit_error(fstring_span, "empty f-string interpolation slot `{}`");
            return Expr { span: fstring_span, kind: ExprKind::Literal(Literal::Unit) };
        }
        let base = base + lead;
        let raw_tokens = crate::lexer::lex(trimmed, file, self.interner, self.diags, self.lint_cfg);
        let shifted: Vec<Lexed> = raw_tokens
            .iter()
            .map(|l| Lexed {
                kind: l.kind,
                span: Span::new(file, BytePos(l.span.lo.0 + base), BytePos(l.span.hi.0 + base)),
                leading_newline: l.leading_newline,
            })
            .collect();
        crate::parser::parse_expr(&shifted, self.interner, self.diags, self.lint_cfg)
    }
}

/// Byte index of the `}` matching a slot's opening `{` (consumed before
/// `start`), or `None` when the slot is unterminated.
fn find_slot_close(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 1usize;
    let mut j = start;
    while j < bytes.len() {
        match bytes[j] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(j);
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
}
