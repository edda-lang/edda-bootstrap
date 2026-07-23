//! Pattern parser. The surface admits wildcards, identifier bindings,
//! literal patterns, tuple-destructuring (paren-form), and sum-variant
//! patterns (`.variant(<sub-pats>)`, positional payload only). Pattern
//! guards (`where`) attach to match arms, not to the pattern itself.
//!
//! Struct-destructuring patterns (`Path { field: pat, .. }`), variant
//! struct-payload patterns (`.variant { field: pat }`), and the rest
//! pattern (`..`) are **not** admitted by `expressions.md` §"Pattern
//! grammar" and are rejected here with a `parse_error` diagnostic.

use edda_intern::Symbol;

use crate::ast::{Ident, Literal, Pat, PatKind, Path, RangeKind, VariantPatPayload};
use crate::token::Token;

use super::Parser;

impl<'a> Parser<'a> {
    /// Parse a single pattern in **permissive** mode — bare identifiers
    /// produce `PatKind::Binding`. Used by `let`/`var` statements and `for`
    /// loops, where the surrounding keyword already marks the binding.
    pub(crate) fn parse_pat(&mut self) -> Pat {
        self.parse_pat_impl(false)
    }

    /// Parse a single pattern in **strict** mode — bare identifiers are a
    /// `parse_error` rather than a binding. Used exclusively from match-arm
    /// parsing.
    pub(crate) fn parse_match_pat(&mut self) -> Pat {
        self.parse_pat_impl(true)
    }

    /// Shared pattern parser. `strict = true` rejects bare identifier
    /// patterns; sub-patterns inherit the mode of their enclosing pattern.
    fn parse_pat_impl(&mut self, strict: bool) -> Pat {
        match self.peek_kind() {
            Token::Underscore => {
                let lex = self.bump();
                Pat {
                    span: lex.span,
                    kind: PatKind::Wildcard,
                }
            }
            Token::True => {
                let lex = self.bump();
                Pat {
                    span: lex.span,
                    kind: PatKind::Literal(Literal::Bool(true)),
                }
            }
            Token::False => {
                let lex = self.bump();
                Pat {
                    span: lex.span,
                    kind: PatKind::Literal(Literal::Bool(false)),
                }
            }
            Token::Int { value, base } => {
                let start = self.pos;
                self.bump();
                self.finish_literal_or_range(start, Literal::Int { value, base })
            }
            Token::Float(sym) => {
                let start = self.pos;
                self.bump();
                self.finish_literal_or_range(start, Literal::Float(sym))
            }
            Token::Str(sym) => {
                let lex = self.bump();
                Pat {
                    span: lex.span,
                    kind: PatKind::Literal(Literal::Str(sym)),
                }
            }
            Token::LParen => self.parse_pat_paren_or_tuple(strict),
            Token::LBracket => self.parse_pat_slice(),
            Token::Let => self.parse_pat_let_binding(),
            Token::Dot => self.parse_pat_leading_dot_variant(strict),
            Token::Ident(_) => self.parse_pat_path_form(strict),
            _ => {
                let span = self.peek().span;
                self.emit_error(span, "expected pattern");
                Pat {
                    span,
                    kind: PatKind::Error,
                }
            }
        }
    }

    fn parse_pat_let_binding(&mut self) -> Pat {
        let start = self.pos;
        self.bump(); // `let`
        let ident = self.expect_ident("expected identifier after `let` in pattern");
        Pat {
            span: self.span_from(start),
            kind: PatKind::Binding(ident),
        }
    }

    /// Given an already-consumed leading literal `lo` (the token at
    /// `start`), produce a `PatKind::Range` if a `..<` / `..=` follows,
    /// else the plain `PatKind::Literal`. The upper bound must itself be
    /// an integer or float literal.
    fn finish_literal_or_range(&mut self, start: usize, lo: Literal) -> Pat {
        let kind = match self.peek_kind() {
            Token::DotDotLt => RangeKind::HalfOpen,
            Token::DotDotEq => RangeKind::Closed,
            _ => {
                return Pat {
                    span: self.span_from(start),
                    kind: PatKind::Literal(lo),
                };
            }
        };
        self.bump(); // `..<` / `..=`
        let hi = match self.peek_kind() {
            Token::Int { value, base } => {
                self.bump();
                Literal::Int { value, base }
            }
            Token::Float(sym) => {
                self.bump();
                Literal::Float(sym)
            }
            _ => {
                let span = self.peek().span;
                self.emit_error(
                    span,
                    "range pattern bound must be an integer or float literal",
                );
                return Pat {
                    span: self.span_from(start),
                    kind: PatKind::Error,
                };
            }
        };
        Pat {
            span: self.span_from(start),
            kind: PatKind::Range { lo, hi, kind },
        }
    }

    fn parse_pat_slice(&mut self) -> Pat {
        let start = self.pos;
        self.bump(); // `[`
        let mut prefix: Vec<Pat> = Vec::new();
        let mut suffix: Vec<Pat> = Vec::new();
        let mut rest: Option<Option<Ident>> = None;
        let mut first = true;
        while !self.at(Token::RBracket) && !self.at_eof() {
            // Forward-progress guard: a sub-pattern parse that hits the
            // `_ => "expected pattern"` recovery arm returns without
            // consuming, and a failed comma `expect` likewise leaves the
            // cursor put — without this bump a malformed element (e.g. a
            // reserved keyword where a binder was expected) would spin the
            // loop forever, growing `prefix`/`suffix` until OOM.
            let iter_start = self.pos;
            if !first {
                self.expect(Token::Comma, "expected `,` or `]` in slice pattern");
                if self.at(Token::RBracket) {
                    break;
                }
            }
            first = false;
            if self.at(Token::DotDot) {
                self.bump(); // `..`
                let binding = match self.peek_kind() {
                    Token::Ident(_) => Some(self.expect_ident("expected rest-binding name")),
                    _ => None,
                };
                if rest.is_some() {
                    let span = self.span_from(start);
                    self.emit_error(
                        span,
                        "a slice pattern admits at most one `..` rest element",
                    );
                } else {
                    rest = Some(binding);
                }
            } else {
                let sub = self.parse_pat_impl(false);
                if rest.is_none() {
                    prefix.push(sub);
                } else {
                    suffix.push(sub);
                }
            }
            if self.pos == iter_start {
                self.bump();
            }
        }
        self.expect(Token::RBracket, "expected `]` to close slice pattern");
        Pat {
            span: self.span_from(start),
            kind: PatKind::Slice {
                prefix,
                rest,
                suffix,
            },
        }
    }

    fn parse_pat_leading_dot_variant(&mut self, strict: bool) -> Pat {
        let start = self.pos;
        self.bump(); // `.`
        let name = self.expect_ident("expected variant name after `.` in pattern");
        let path = Path {
            segments: vec![Ident {
                name: name.name,
                span: name.span,
            }],
            span: name.span,
        };
        let payload = self.parse_variant_pat_payload(strict);
        Pat {
            span: self.span_from(start),
            kind: PatKind::Variant { path, payload },
        }
    }

    /// Parse a variant payload `(<sub-pats>)` or nothing. Shared between
    /// the leading-dot shorthand (`.<variant>(...)`) and fully-qualified
    /// path forms (`Type.<variant>(...)`). The named-payload brace form
    /// `.<variant> { field: pat }` is not admitted.
    fn parse_variant_pat_payload(&mut self, strict: bool) -> VariantPatPayload {
        match self.peek_kind() {
            Token::LParen => {
                self.bump();
                let mut payload = Vec::new();
                if !self.at(Token::RParen) {
                    payload.push(self.parse_pat_impl(strict));
                    while self.eat(Token::Comma) {
                        if self.at(Token::RParen) {
                            break;
                        }
                        payload.push(self.parse_pat_impl(strict));
                    }
                }
                self.expect(Token::RParen, "expected `)` in variant payload");
                VariantPatPayload::Tuple(payload)
            }
            Token::LBrace => {
                let span = self.peek().span;
                self.emit_error(
                    span,
                    "named variant payload patterns `.<variant> { ... }` are not admitted; \
                     per `expressions.md` §\"Pattern grammar\", variant payloads are positional — \
                     use `.<variant>(let <name>, ...)`",
                );
                self.skip_brace_block();
                VariantPatPayload::None
            }
            _ => VariantPatPayload::None,
        }
    }

    /// Consume a balanced `{ ... }` block on the cursor for error recovery
    /// after rejecting a not-admitted pattern form. The leading `{` must
    /// be at the cursor; nested braces are tracked so the consumer is
    /// resynchronised at the matching `}` (or EOF) without leaking the
    /// inner tokens to the next parser call.
    fn skip_brace_block(&mut self) {
        if !self.at(Token::LBrace) {
            return;
        }
        self.bump(); // opening `{`
        let mut depth: usize = 1;
        while depth > 0 && !self.at_eof() {
            match self.peek_kind() {
                Token::LBrace => {
                    self.bump();
                    depth += 1;
                }
                Token::RBrace => {
                    self.bump();
                    depth -= 1;
                }
                _ => {
                    self.bump();
                }
            }
        }
    }

    fn parse_pat_paren_or_tuple(&mut self, strict: bool) -> Pat {
        let start = self.pos;
        self.bump(); // `(`
        if self.eat(Token::RParen) {
            // `()` — unit literal pattern.
            return Pat {
                span: self.span_from(start),
                kind: PatKind::Literal(Literal::Unit),
            };
        }
        let first = self.parse_pat_impl(strict);
        if self.eat(Token::RParen) {
            // `(p)` — grouping; no one-tuple syntax is admitted.
            return first;
        }
        let mut elems = vec![first];
        while self.eat(Token::Comma) {
            if self.at(Token::RParen) {
                break;
            }
            elems.push(self.parse_pat_impl(strict));
        }
        self.expect(Token::RParen, "expected `)` after tuple pattern");
        Pat {
            span: self.span_from(start),
            kind: PatKind::Tuple(elems),
        }
    }

    /// `Path.name(...)` → variant tuple payload; multi-segment `Path`
    /// alone → variant unit; bare ident alone is a binding (permissive)
    /// or a parse error (strict). The `Path { ... }` struct-destructure
    /// form is **not** admitted.
    fn parse_pat_path_form(&mut self, strict: bool) -> Pat {
        let start = self.pos;
        let path = self.parse_path();
        if self.at(Token::At) {
            return self.parse_pat_at_binding(start, path, strict);
        }
        match self.peek_kind() {
            Token::LBrace => {
                let brace_span = self.peek().span;
                let span = self.span_from(start);
                self.emit_error(
                    brace_span,
                    "struct-destructure patterns `Path { field: pat, ... }` are not admitted; \
                     per `expressions.md` §\"Pattern grammar\", patterns admit literal / `let <name>` / \
                     `_` / `.<variant>(<sub-pats>)` / tuple forms only",
                );
                self.skip_brace_block();
                Pat {
                    span,
                    kind: PatKind::Error,
                }
            }
            Token::LParen => {
                self.bump();
                let mut payload = Vec::new();
                if !self.at(Token::RParen) {
                    payload.push(self.parse_pat_impl(strict));
                    while self.eat(Token::Comma) {
                        if self.at(Token::RParen) {
                            break;
                        }
                        payload.push(self.parse_pat_impl(strict));
                    }
                }
                self.expect(Token::RParen, "expected `)` in variant payload");
                Pat {
                    span: self.span_from(start),
                    kind: PatKind::Variant {
                        path,
                        payload: VariantPatPayload::Tuple(payload),
                    },
                }
            }
            _ => {
                if path.segments.len() == 1 {
                    let ident = path.segments.into_iter().next().unwrap();
                    if strict {
                        self.emit_error(
                            ident.span,
                            "bare identifier patterns are reserved; use `let <name>` to bind or `.<variant>` to match a sum-type variant",
                        );
                        Pat {
                            span: ident.span,
                            kind: PatKind::Error,
                        }
                    } else {
                        Pat {
                            span: ident.span,
                            kind: PatKind::Binding(ident),
                        }
                    }
                } else {
                    // Qualified path with no payload — sum-variant unit.
                    Pat {
                        span: path.span,
                        kind: PatKind::Variant {
                            path,
                            payload: VariantPatPayload::None,
                        },
                    }
                }
            }
        }
    }

    fn parse_pat_at_binding(&mut self, start: usize, path: Path, strict: bool) -> Pat {
        let name = if path.segments.len() == 1 {
            path.segments.into_iter().next().unwrap()
        } else {
            self.emit_error(path.span, "`@`-binding name must be a single identifier");
            self.bump(); // `@`
            let _ = self.parse_pat_impl(strict);
            return Pat {
                span: self.span_from(start),
                kind: PatKind::Error,
            };
        };
        self.bump(); // `@`
        let inner = self.parse_pat_impl(strict);
        Pat {
            span: self.span_from(start),
            kind: PatKind::AtBinding {
                name,
                inner: Box::new(inner),
            },
        }
    }
}

/// Collect every name this pattern binds. Drives the or-pattern
/// same-binder-set rule in `parse_match_arm`: every `|`-separated alternative in a
/// match arm must bind the same set of names (types and modes are then
/// enforced implicitly, since each alternative's duplicated arm body is
/// typechecked independently).
pub(super) fn pat_bound_names(p: &Pat, out: &mut Vec<Symbol>) {
    match &p.kind {
        PatKind::Binding(ident) => out.push(ident.name),
        PatKind::Wildcard | PatKind::Literal(_) | PatKind::Error | PatKind::Range { .. } => {}
        PatKind::Tuple(ps) => ps.iter().for_each(|p| pat_bound_names(p, out)),
        PatKind::Variant { payload, .. } => match payload {
            VariantPatPayload::None => {}
            VariantPatPayload::Tuple(ps) => ps.iter().for_each(|p| pat_bound_names(p, out)),
            VariantPatPayload::Struct(fields) => {
                fields.iter().for_each(|f| pat_bound_names(&f.pat, out))
            }
        },
        PatKind::Struct { fields, .. } => {
            fields.iter().for_each(|f| pat_bound_names(&f.pat, out))
        }
        PatKind::Guard { pat, .. } => pat_bound_names(pat, out),
        // `name @ inner` binds `name`, plus whatever `inner` binds.
        PatKind::AtBinding { name, inner } => {
            out.push(name.name);
            pat_bound_names(inner, out);
        }
        PatKind::Slice {
            prefix,
            rest,
            suffix,
        } => {
            prefix.iter().for_each(|p| pat_bound_names(p, out));
            if let Some(Some(name)) = rest {
                out.push(name.name);
            }
            suffix.iter().for_each(|p| pat_bound_names(p, out));
        }
    }
}
