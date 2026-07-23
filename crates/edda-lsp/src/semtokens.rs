//! Semantic-token legend and encoder over an Edda token stream.
//!
//! `textDocument/semanticTokens/full` returns an array of u32 quintuples
//! `(deltaLine, deltaStartChar, length, tokenType, tokenModifierBitmap)`.
//! Each entry is relative to the previous token; the encoder here owns
//! that delta-encoding plus the lex → legend-index mapping.
//!
//! v0.1 derives semantic tokens from the lexer output directly. A later
//! pass can layer richer classification (resolved identifiers as types
//! vs functions vs locals) on top of the resolver's outputs once
//! `edda-daemon` exposes them.

use edda_syntax::{Lexed, Token};
use lsp_types::{SemanticToken, SemanticTokenType};

use crate::position::PositionEncoding;

/// The legend of semantic-token types this server emits, in legend-index
/// order. The LSP `legend.tokenTypes` capability returns the
/// LSP names; this array is the bridge from the internal index to those
/// names.
pub const TOKEN_TYPES: &[SemanticTokenType] = &[
    SemanticTokenType::KEYWORD,
    SemanticTokenType::COMMENT,
    SemanticTokenType::STRING,
    SemanticTokenType::NUMBER,
    SemanticTokenType::OPERATOR,
    SemanticTokenType::TYPE,
    SemanticTokenType::VARIABLE,
];

/// Legend index for keyword tokens.
const KW: u32 = 0;
/// Legend index for comment tokens (doc comments).
const COMMENT: u32 = 1;
/// Legend index for string-literal tokens.
const STRING: u32 = 2;
/// Legend index for number-literal tokens.
const NUMBER: u32 = 3;
/// Legend index for operator/punctuation tokens.
const OPERATOR: u32 = 4;
/// Legend index for the `Type` meta-type keyword (treated as a type).
const TYPE: u32 = 5;
/// Legend index for identifiers (no resolution — every identifier is
/// classified as `variable` in v0.1).
const VARIABLE: u32 = 6;

/// Map a lexed [`Token`] to its legend index, or `None` if it should not
/// produce a semantic token.
fn classify(t: Token) -> Option<u32> {
    Some(match t {
        Token::Ident(_) => VARIABLE,
        Token::Int { .. } | Token::Float(_) => NUMBER,
        Token::Str(_) | Token::FString(_) => STRING,
        Token::True | Token::False => KW,
        Token::TypeMeta => TYPE,
        Token::OuterDoc(_) | Token::InnerDoc(_) => COMMENT,
        Token::Eof => return None,
        // Bracket pairs and punctuation are tagged as operator so editors
        // that style operators distinctly from keywords get useful colour.
        Token::LParen
        | Token::RParen
        | Token::LBracket
        | Token::RBracket
        | Token::LBrace
        | Token::RBrace
        | Token::Comma
        | Token::Dot
        | Token::Colon
        | Token::Semicolon
        | Token::Underscore
        | Token::Plus
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
        | Token::AmpAmp
        | Token::PipePipe
        | Token::Bang
        | Token::Amp
        | Token::Pipe
        | Token::Caret
        | Token::Tilde
        | Token::LtLt
        | Token::GtGt
        | Token::Eq
        | Token::PlusEq
        | Token::MinusEq
        | Token::StarEq
        | Token::SlashEq
        | Token::PercentEq
        | Token::AmpEq
        | Token::PipeEq
        | Token::CaretEq
        | Token::LtLtEq
        | Token::GtGtEq
        | Token::Arrow
        | Token::FatArrow
        | Token::DotDotLt
        | Token::DotDotEq
        | Token::DotDot
        | Token::DotDotDot
        | Token::Question => OPERATOR,
        // Everything else is a reserved keyword. Variants that carry no
        // payload still need classification.
        _ => KW,
    })
}

/// Encode a lexed token stream into LSP semantic-token deltas.
///
/// `text` must be the same buffer the lexer consumed; `encoding`
/// determines the column unit (UTF-8 bytes / UTF-16 units / codepoints).
pub fn encode_semantic_tokens(
    text: &str,
    tokens: &[Lexed],
    encoding: PositionEncoding,
) -> Vec<SemanticToken> {
    let mut out = Vec::with_capacity(tokens.len());
    let mut prev_line: u32 = 0;
    let mut prev_start: u32 = 0;
    // Bounded loop: one iteration per lexed token.
    for lexed in tokens {
        let Some(token_type) = classify(lexed.kind) else {
            continue;
        };
        let lo = lexed.span.lo.to_usize();
        let hi = lexed.span.hi.to_usize();
        if hi <= lo {
            continue;
        }
        let start = crate::position::byte_to_position(text, lo, encoding);
        let end = crate::position::byte_to_position(text, hi, encoding);
        // Multi-line tokens are not part of the wire format — clients
        // expect one delta entry per line. v0.1 drops the trailing lines;
        // a later pass can split a multi-line span into a per-line burst.
        let length = if start.line == end.line {
            end.character.saturating_sub(start.character)
        } else {
            continue;
        };
        if length == 0 {
            continue;
        }
        let delta_line = start.line - prev_line;
        let delta_start = if delta_line == 0 {
            start.character - prev_start
        } else {
            start.character
        };
        out.push(SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type,
            token_modifiers_bitset: 0,
        });
        prev_line = start.line;
        prev_start = start.character;
    }
    out
}
