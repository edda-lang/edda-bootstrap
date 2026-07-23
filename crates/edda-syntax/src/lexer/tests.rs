//! End-to-end tests for the lexer state machine. Each test runs the
//! public [`lex`] entry-point against a small source snippet and asserts
//! on the resulting token stream and diagnostic output.

use super::{Lexed, Lexer, lex};
use crate::token::{IntBase, Token};
use edda_diag::{Diagnostics, LintConfig};
use edda_intern::Interner;
use edda_span::SourceMap;
use std::path::PathBuf;

fn lex_str(src: &str) -> (Vec<Lexed>, Interner, Diagnostics) {
    let map = SourceMap::new();
    let file = map.add_file(PathBuf::from("test.ea"), src.to_string());
    let interner = Interner::new();
    let mut diags = Diagnostics::new();
    let cfg = LintConfig::new();
    let toks = lex(src, file, &interner, &mut diags, &cfg);
    (toks, interner, diags)
}

fn kinds(toks: &[Lexed]) -> Vec<Token> {
    toks.iter().map(|t| t.kind).collect()
}

#[test]
fn empty_source_yields_only_eof() {
    let (toks, _, diags) = lex_str("");
    assert!(!diags.has_errors());
    assert_eq!(toks.len(), 1);
    assert_eq!(toks[0].kind, Token::Eof);
}

#[test]
fn line_comment_is_rejected() {
    // The V1.0 no-comment design lock: `.ea` source admits no comments.
    // `lex` rejects every comment form with `comment_not_admitted` and
    // recovers to end-of-line so the real token stream after the comment still lexes.
    let (toks, _, diags) = lex_str("  // comment\nlet\n");
    assert!(diags.has_errors());
    let ks = kinds(&toks);
    assert_eq!(ks[0], Token::Let);
}

#[test]
fn block_comment_is_rejected() {
    let (toks, _, diags) = lex_str("/* outer /* inner */ still outer */ x");
    assert!(diags.has_errors());
    let ks = kinds(&toks);
    assert!(matches!(ks[0], Token::Ident(_)));
}

#[test]
fn unterminated_block_comment_reports_error() {
    let (_, _, diags) = lex_str("/* unclosed");
    assert!(diags.has_errors());
}

#[test]
fn identifiers_and_keywords() {
    let (toks, interner, _) = lex_str("function let var foo bar_baz Type type");
    let ks = kinds(&toks);
    assert_eq!(ks[0], Token::Function);
    assert_eq!(ks[1], Token::Let);
    assert_eq!(ks[2], Token::Var);
    let Token::Ident(sym_foo) = ks[3] else {
        panic!("expected Ident");
    };
    let Token::Ident(sym_bar) = ks[4] else {
        panic!("expected Ident");
    };
    assert_eq!(interner.resolve(sym_foo), "foo");
    assert_eq!(interner.resolve(sym_bar), "bar_baz");
    assert_eq!(ks[5], Token::TypeMeta); // CamelCase
    assert_eq!(ks[6], Token::Type); // lowercase
    assert_eq!(ks[7], Token::Eof);
}

#[test]
fn underscore_alone_is_wildcard() {
    let (toks, _, _) = lex_str("_ _x x_y");
    let ks = kinds(&toks);
    assert_eq!(ks[0], Token::Underscore);
    assert!(matches!(ks[1], Token::Ident(_)));
    assert!(matches!(ks[2], Token::Ident(_)));
}

#[test]
fn integer_literals_in_every_base() {
    let (toks, _, diags) = lex_str("0 42 1_000_000 0xFF 0b1010_0011 0o755");
    assert!(!diags.has_errors());
    let ks = kinds(&toks);
    assert_eq!(
        ks[0],
        Token::Int {
            value: 0,
            base: IntBase::Dec
        }
    );
    assert_eq!(
        ks[1],
        Token::Int {
            value: 42,
            base: IntBase::Dec
        }
    );
    assert_eq!(
        ks[2],
        Token::Int {
            value: 1_000_000,
            base: IntBase::Dec
        }
    );
    assert_eq!(
        ks[3],
        Token::Int {
            value: 0xFF,
            base: IntBase::Hex
        }
    );
    assert_eq!(
        ks[4],
        Token::Int {
            value: 0b1010_0011,
            base: IntBase::Bin
        }
    );
    assert_eq!(
        ks[5],
        Token::Int {
            value: 0o755,
            base: IntBase::Oct
        }
    );
}

#[test]
fn missing_digits_after_base_prefix_is_error() {
    let (_, _, diags) = lex_str("0x");
    assert!(diags.has_errors());
}

#[test]
fn float_literals() {
    let (toks, interner, diags) = lex_str("1.0 3.14 1e6 1.5e-3");
    assert!(!diags.has_errors());
    let ks = kinds(&toks);
    for k in &ks[..4] {
        let Token::Float(sym) = k else {
            panic!("expected float, got {:?}", k)
        };
        assert!(!interner.resolve(*sym).is_empty());
    }
    assert_eq!(
        interner.resolve(match ks[0] {
            Token::Float(s) => s,
            _ => unreachable!(),
        }),
        "1.0"
    );
}

#[test]
fn dot_after_int_is_not_a_float() {
    // `1.foo()` must lex as Int(1) Dot Ident(foo) LParen RParen.
    let (toks, _, _) = lex_str("1.foo()");
    let ks = kinds(&toks);
    assert_eq!(
        ks[0],
        Token::Int {
            value: 1,
            base: IntBase::Dec
        }
    );
    assert_eq!(ks[1], Token::Dot);
    assert!(matches!(ks[2], Token::Ident(_)));
    assert_eq!(ks[3], Token::LParen);
    assert_eq!(ks[4], Token::RParen);
}

#[test]
fn plain_string_with_escapes() {
    let (toks, interner, diags) = lex_str(r#" "hello\nworld\t\"quoted\"" "#);
    assert!(!diags.has_errors());
    let Token::Str(sym) = toks[0].kind else {
        panic!("expected Str");
    };
    assert_eq!(interner.resolve(sym), "hello\nworld\t\"quoted\"");
}

#[test]
fn hex_and_unicode_escapes() {
    let (toks, interner, diags) = lex_str(r#" "A=\x41 smile=\u{1F600}" "#);
    assert!(!diags.has_errors());
    let Token::Str(sym) = toks[0].kind else {
        panic!("expected Str");
    };
    assert_eq!(interner.resolve(sym), "A=A smile=\u{1F600}");
}

#[test]
fn unterminated_string_reports_error() {
    let (_, _, diags) = lex_str("\"oops");
    assert!(diags.has_errors());
}

#[test]
fn fstring_is_lexed_as_single_token() {
    let (toks, interner, diags) = lex_str(r#" f"hello, {name}!" "#);
    assert!(!diags.has_errors());
    let Token::FString(sym) = toks[0].kind else {
        panic!("expected FString, got {:?}", toks[0].kind);
    };
    assert_eq!(interner.resolve(sym), "hello, {name}!");
}

#[test]
fn fstring_brace_depth_in_slot() {
    // `f"{Point { x: 1.0 }}"` — outer braces wrap a slot whose expression
    // contains nested braces. Lexer must find the matching closing quote,
    // not stop at the first `"`.
    let (toks, interner, _) = lex_str(r#" f"{Point { x: 1.0 }}" "#);
    let Token::FString(sym) = toks[0].kind else {
        panic!("expected FString");
    };
    assert_eq!(interner.resolve(sym), "{Point { x: 1.0 }}");
}

#[test]
fn three_two_and_one_char_operators() {
    let (toks, _, _) =
        lex_str("..< ..= ... <<= >>= -> => == != <= >= << >> && || += < + ?");
    let ks = kinds(&toks);
    assert_eq!(ks[0], Token::DotDotLt);
    assert_eq!(ks[1], Token::DotDotEq);
    assert_eq!(ks[2], Token::DotDotDot);
    assert_eq!(ks[3], Token::LtLtEq);
    assert_eq!(ks[4], Token::GtGtEq);
    assert_eq!(ks[5], Token::Arrow);
    assert_eq!(ks[6], Token::FatArrow);
    assert_eq!(ks[7], Token::EqEq);
    assert_eq!(ks[8], Token::BangEq);
    assert_eq!(ks[9], Token::LtEq);
    assert_eq!(ks[10], Token::GtEq);
    assert_eq!(ks[11], Token::LtLt);
    assert_eq!(ks[12], Token::GtGt);
    assert_eq!(ks[13], Token::AmpAmp);
    assert_eq!(ks[14], Token::PipePipe);
    assert_eq!(ks[15], Token::PlusEq);
    assert_eq!(ks[16], Token::Lt);
    assert_eq!(ks[17], Token::Plus);
    assert_eq!(ks[18], Token::Question);
}

#[test]
fn attribute_introducer_lexes_as_at_token() {
    let (toks, interner, diags) = lex_str("@export(\"name\")");
    assert!(!diags.has_errors());
    let ks = kinds(&toks);
    assert_eq!(ks[0], Token::At);
    let Token::Ident(sym) = ks[1] else {
        panic!("expected Ident after `@`, got {:?}", ks[1]);
    };
    assert_eq!(interner.resolve(sym), "export");
    assert_eq!(ks[2], Token::LParen);
    assert!(matches!(ks[3], Token::Str(_)));
    assert_eq!(ks[4], Token::RParen);
}

#[test]
fn at_token_spans_exactly_one_byte() {
    let (toks, _, _) = lex_str("@");
    assert_eq!(toks[0].kind, Token::At);
    assert_eq!(toks[0].span.lo.to_usize(), 0);
    assert_eq!(toks[0].span.hi.to_usize(), 1);
}

#[test]
fn brackets_and_punctuation() {
    let (toks, _, _) = lex_str("()[]{},.:;");
    let ks = kinds(&toks);
    assert_eq!(
        &ks[..ks.len() - 1],
        &[
            Token::LParen,
            Token::RParen,
            Token::LBracket,
            Token::RBracket,
            Token::LBrace,
            Token::RBrace,
            Token::Comma,
            Token::Dot,
            Token::Colon,
            Token::Semicolon,
        ]
    );
}

#[test]
fn doc_comments_are_rejected() {
    // The legacy doc tiers (`///`, `//!`) are comments too, per the V1.0
    // no-comment design lock — rejected with `comment_not_admitted`, never
    // emitted as doc tokens.
    let (toks, _, diags) = lex_str("/// outer\n//! inner\nlet");
    assert!(diags.has_errors());
    let ks = kinds(&toks);
    assert!(!ks
        .iter()
        .any(|k| matches!(k, Token::OuterDoc(_) | Token::InnerDoc(_))));
    assert_eq!(ks[0], Token::Let);
}

#[test]
fn four_doc_tiers_are_rejected() {
    // All four legacy doc tiers (`!!!`, `/!!`, `//!`, `///`) reject as
    // comments and the lexer recovers to the next real token.
    let (toks, _, diags) = lex_str("!!! high\n/!! medium\n//! low\n/// legacy\nlet");
    assert!(diags.has_errors());
    let ks = kinds(&toks);
    assert!(!ks.iter().any(|k| matches!(
        k,
        Token::DocHigh(_) | Token::DocMed(_) | Token::InnerDoc(_) | Token::OuterDoc(_)
    )));
    assert_eq!(ks[0], Token::Let);
}

#[test]
fn stable_and_unstable_lex_as_identifiers() {
    // §3.7 stability modifiers are contextual soft-keywords (not reserved):
    // the lexer emits them as plain identifiers; the parser recognizes them
    // only in stability position.
    let (toks, interner, _) = lex_str("stable unstable");
    let ks = kinds(&toks);
    let Token::Ident(s0) = ks[0] else {
        panic!("expected `stable` to lex as an identifier, got {:?}", ks[0]);
    };
    let Token::Ident(s1) = ks[1] else {
        panic!("expected `unstable` to lex as an identifier, got {:?}", ks[1]);
    };
    assert_eq!(interner.resolve(s0), "stable");
    assert_eq!(interner.resolve(s1), "unstable");
}

#[test]
fn unexpected_character_reports_error_and_continues() {
    let (toks, _, diags) = lex_str("let \u{00A3} x");
    // £ is not a valid Edda lex char. Lexer emits an error and continues.
    assert!(diags.has_errors());
    let ks = kinds(&toks);
    assert_eq!(ks[0], Token::Let);
    assert!(matches!(ks[1], Token::Ident(_)));
}

#[test]
fn spans_are_correct_for_each_token() {
    let (toks, _, _) = lex_str("ab + 12");
    assert_eq!(toks.len(), 4);
    assert_eq!(toks[0].span.lo.to_usize(), 0);
    assert_eq!(toks[0].span.hi.to_usize(), 2);
    assert_eq!(toks[1].span.lo.to_usize(), 3);
    assert_eq!(toks[1].span.hi.to_usize(), 4);
    assert_eq!(toks[2].span.lo.to_usize(), 5);
    assert_eq!(toks[2].span.hi.to_usize(), 7);
}

#[test]
fn eof_token_is_always_last() {
    let (toks, _, _) = lex_str("a b c");
    assert_eq!(toks.last().unwrap().kind, Token::Eof);
}

#[test]
fn integer_overflow_reports_error() {
    let src = "340282366920938463463374607431768211456"; // 2^128
    let (_, _, diags) = lex_str(src);
    assert!(diags.has_errors());
}

#[test]
fn triple_quoted_string_strips_indent_against_closing_delimiter() {
    // `01-syntax.md` §1 — leading newline after the opener is stripped;
    // the closing delimiter's own-line indentation is removed per line.
    let src = "\"\"\"\n    line one\n    line two\n    \"\"\"";
    let (toks, interner, diags) = lex_str(src);
    assert!(!diags.has_errors(), "unexpected diagnostics");
    let Token::Str(sym) = toks[0].kind else {
        panic!("expected Str token, got {:?}", toks[0].kind);
    };
    assert_eq!(interner.resolve(sym), "line one\nline two\n");
    assert_eq!(toks[1].kind, Token::Eof);
}

#[test]
fn triple_quoted_string_unindented_closer_keeps_line_indent() {
    let src = "\"\"\"\n    a\n    b\n\"\"\"";
    let (toks, interner, diags) = lex_str(src);
    assert!(!diags.has_errors());
    let Token::Str(sym) = toks[0].kind else {
        panic!("expected Str token");
    };
    assert_eq!(interner.resolve(sym), "    a\n    b\n");
}

#[test]
fn triple_quoted_string_single_line_and_escapes() {
    let (toks, interner, diags) = lex_str("\"\"\"a \\t \"quoted\" b\"\"\"");
    assert!(!diags.has_errors());
    let Token::Str(sym) = toks[0].kind else {
        panic!("expected Str token");
    };
    assert_eq!(interner.resolve(sym), "a \t \"quoted\" b");
}

#[test]
fn unterminated_triple_quoted_string_reports_error_not_empty_pair() {
    // This previously mislexed as `""` + an unterminated `"` literal.
    let (toks, _, diags) = lex_str("\"\"\"\nabc\n");
    assert!(diags.has_errors());
    assert!(matches!(toks[0].kind, Token::Str(_)));
    assert_eq!(toks[1].kind, Token::Eof);
}

#[test]
fn lexer_struct_is_directly_usable() {
    // Re-export sanity: the `Lexer` type is reachable for callers that
    // want manual control over construction.
    let map = SourceMap::new();
    let file = map.add_file(PathBuf::from("test.ea"), "x".to_string());
    let interner = Interner::new();
    let mut diags = Diagnostics::new();
    let cfg = LintConfig::new();
    let lexer = Lexer::new("x", file, &interner, &mut diags, &cfg);
    let toks = lexer.run();
    assert_eq!(toks.len(), 2);
    assert!(matches!(toks[0].kind, Token::Ident(_)));
}
