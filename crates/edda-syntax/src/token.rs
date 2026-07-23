//! Token catalogue and keyword-lookup table for the Edda lexer.
//!
//! [`Token`] is the lexical-category sum; [`Lexed`] pairs a `Token` with its
//! source [`Span`]. Identifier text and literal payloads ride inside the
//! variant so the parser never reaches back into the source buffer.

use edda_intern::Symbol;
use edda_span::Span;

/// Every lexical category the locked Edda surface admits.
///
/// `Token` is `Copy` because every payload variant carries `Copy` data —
/// `Symbol` is a 32-bit handle and `IntBase` is a small enum.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Token {
    // === Identifiers and literals ===
    /// Identifier (interned, includes primitive type names like `i32`).
    Ident(Symbol),
    /// Integer literal, value already parsed from its source form. `base`
    /// is preserved so the pretty-printer can reproduce `0xFF` vs `255`.
    Int { value: u128, base: IntBase },
    /// Float literal; raw source text is interned because IEEE-754 round-trip
    /// through `f64::from_str` may not produce byte-identical output.
    Float(Symbol),
    /// Plain string literal `"..."`; payload is the escape-resolved content.
    Str(Symbol),
    /// `f"..."` interpolated string. Payload is the raw bytes between the
    /// `f"` and the matching `"`, with backslash escapes preserved verbatim.
    /// Structured slot lexing is deferred — the parser re-lexes the raw bytes.
    FString(Symbol),
    /// `true` boolean literal.
    True,
    /// `false` boolean literal.
    False,

    // === Declaration keywords ===
    /// `function` keyword.
    Function,
    /// `module` keyword.
    Module,
    /// `import` keyword.
    Import,
    /// `public` keyword.
    Public,
    /// `type` keyword (lowercase; declares a product or sum type).
    Type,
    /// `case` keyword (sum-type variant introducer).
    Case,
    /// `spec` keyword (codegen-spec declaration or invocation).
    Spec,
    /// `extern` keyword — introduces an extern-symbol body on a
    /// function declaration (`function f(...) -> R extern "sym"`).
    Extern,
    /// `affine` type-level linearity modifier — values of an affine
    /// type may be dropped at most once.
    Affine,
    /// `linear` type-level linearity modifier — values of a linear
    /// type must be consumed exactly once.
    Linear,

    // === Control-flow keywords ===
    /// `if` keyword.
    If,
    /// `else` keyword.
    Else,
    /// `match` keyword.
    Match,
    /// `for` keyword.
    For,
    /// `in` keyword.
    In,
    /// `loop` keyword.
    Loop,
    /// `break` keyword.
    Break,
    /// `continue` keyword.
    Continue,
    /// `return` keyword.
    Return,

    // === Binding keywords ===
    /// `let` keyword.
    Let,
    /// `var` keyword.
    Var,
    /// `uninit` keyword — uninitialised binding (receives an `init`-mode call).
    Uninit,

    // === Type / refinement keywords ===
    /// `where` keyword (refinement / pattern guard / spec constraint).
    Where,
    /// `as` keyword (primitive cast; also reserved for aliased imports).
    As,
    /// `wrapping` keyword — trailing cast-mode modifier on `expr as T`;
    /// two's-complement modular semantics on integer narrowing per
    /// CLAUDE.md §"Numeric operators".
    Wrapping,
    /// `saturating` keyword — trailing cast-mode modifier on `expr as T`;
    /// clamps to destination's MIN/MAX on integer narrowing.
    Saturating,
    /// `checked` keyword — trailing cast-mode modifier on `expr as T`;
    /// raises `err: Overflow` when the value is out of range.
    Checked,
    /// `requires` keyword (precondition clause).
    Requires,
    /// `ensures` keyword (postcondition clause).
    Ensures,
    /// `decreases` keyword (termination-measure clause; see
    /// `corpus/edda-codex/language/03-verification.md` §5).
    Decreases,
    /// `result` keyword (postcondition return-value binder).
    ResultKw,
    /// `forall` keyword — bounded universal quantifier in refinement
    /// predicates per V1.0 refinement-fragment widening
    /// (`corpus/edda-codex/language/03-verification.md` §11).
    /// Surface: `forall <ident> in <iter>: <body>`.
    Forall,
    /// `exists` keyword — bounded existential quantifier in refinement
    /// predicates per V1.0 refinement-fragment widening. Mirror of
    /// [`Token::Forall`].
    Exists,

    // === Effect keywords ===
    /// `with` keyword (effect-row introducer).
    With,
    /// `raise` keyword.
    Raise,
    /// `panic` keyword.
    Panic,
    /// `scope` keyword (`scope(exec)` block).
    ScopeKw,
    /// `await` keyword (task resolution).
    Await,

    // === Parameter-mode keywords ===
    /// `mutable` parameter mode.
    Mutable,
    /// `take` parameter mode.
    Take,
    /// `init` parameter mode.
    Init,

    // === Comptime keywords ===
    /// `comptime` keyword (prefix / block / parameter mode).
    Comptime,
    /// `Type` meta-type keyword (CamelCase; comptime values that are types).
    TypeMeta,

    /// `derive` keyword (top-level closed-vocabulary derive declaration;
    /// see `corpus/edda-codex/language/04-specs-comptime.md` §5).
    Derive,

    // === Reserved for future phases ===
    /// `yield` keyword (reserved for future iteration-as-effect support).
    Yield,

    // === Bracket pairs ===
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `{`
    LBrace,
    /// `}`
    RBrace,

    // === Structural punctuation ===
    /// `,`
    Comma,
    /// `.`
    Dot,
    /// `:`
    Colon,
    /// `;`
    Semicolon,
    /// `_` wildcard / discard.
    Underscore,

    // === Arithmetic operators ===
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `%`
    Percent,
    /// `+%` wrapping addition (modulo two's-complement) per `spec-sweep-locks.md` S1.
    PlusPct,
    /// `-%` wrapping subtraction.
    MinusPct,
    /// `*%` wrapping multiplication.
    StarPct,
    /// `+?` checked addition (raises `err: Overflow`) per `spec-sweep-locks.md` S1.
    PlusQuestion,
    /// `-?` checked subtraction.
    MinusQuestion,
    /// `*?` checked multiplication.
    StarQuestion,
    /// `%?` checked modulo — raises `err: Overflow` on `INT_MIN % -1`
    /// instead of trapping.
    PercentQuestion,
    /// `+|` saturating addition (clamps to operand width's MIN/MAX) per CLAUDE.md §"Numeric operators".
    PlusPipe,
    /// `-|` saturating subtraction.
    MinusPipe,
    /// `*|` saturating multiplication.
    StarPipe,

    // === Comparison / equality ===
    /// `<`
    Lt,
    /// `<=`
    LtEq,
    /// `>`
    Gt,
    /// `>=`
    GtEq,
    /// `==`
    EqEq,
    /// `!=`
    BangEq,

    // === Logical ===
    /// `&&`
    AmpAmp,
    /// `||`
    PipePipe,
    /// `!`
    Bang,

    // === Bitwise ===
    /// `&`
    Amp,
    /// `|`
    Pipe,
    /// `^`
    Caret,
    /// `~`
    Tilde,
    /// `<<`
    LtLt,
    /// `>>`
    GtGt,

    // === Assignment ===
    /// `=`
    Eq,
    /// `+=`
    PlusEq,
    /// `-=`
    MinusEq,
    /// `*=`
    StarEq,
    /// `/=`
    SlashEq,
    /// `%=`
    PercentEq,
    /// `&=`
    AmpEq,
    /// `|=`
    PipeEq,
    /// `^=`
    CaretEq,
    /// `<<=`
    LtLtEq,
    /// `>>=`
    GtGtEq,

    // === Type / function notation ===
    /// `->` function-signature return arrow.
    Arrow,
    /// `=>` match-arm separator.
    FatArrow,

    // === Range operators ===
    /// `..<` half-open range.
    DotDotLt,
    /// `..=` closed range.
    DotDotEq,
    /// `..` open-ended half-open range (slice subrange forms per phase-2-locks Gap 7).
    DotDot,
    /// `...` effect-row spread.
    DotDotDot,

    // === Postfix ===
    /// `?` error-effect propagator.
    Question,

    // === Attribute introducer ===
    /// `@` — introduces an item-level attribute (`@export("name")`,
    /// `@abi("sysv64")`, `@align(8)`, `@repr(C)`, `@layout(natural)`).
    /// Consumed by the parser between an item's leading doc-comments
    /// and its visibility / kind keyword; the AST carries one
    /// [`crate::ast::Attribute`] per `@name(args)` clause and
    /// `edda-types` enforces the registry of admitted names and arg
    /// shapes.
    At,

    // === Doc comments (preserved verbatim for AST consumption) ===
    // Four locked tiers per `01-syntax.md` §3.2. The codex tiers map to AST
    // [`crate::ast::DocTier`] as: `!!!` → High, `/!!` → Medium, `//!` → Low
    // (file-level when at file head). `///` is reserved as a non-parsed
    // visual separator in the locked surface; in the bootstrap it remains a
    // doc-comment tier mapped to Medium until the corpus migration retires
    // the legacy form.
    /// `!!!` high-importance doc-comment line; payload is content after prefix.
    DocHigh(Symbol),
    /// `/!!` medium (default) doc-comment line; payload is content after prefix.
    DocMed(Symbol),
    /// `///` outer doc-comment line; payload is content after prefix.
    /// Legacy tier — pending corpus migration to `/!!` / `//!`.
    OuterDoc(Symbol),
    /// `//!` inner doc-comment line; payload is content after prefix.
    /// File-head form is the file-level doc; item-attached form is low tier.
    InnerDoc(Symbol),

    // === Stream terminator ===
    /// End-of-input sentinel; emitted exactly once as the final token.
    Eof,
}

/// Numeric base of an [`Token::Int`] literal, preserved so the
/// pretty-printer can reproduce the user's chosen spelling.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum IntBase {
    /// Plain decimal (no prefix).
    Dec,
    /// `0x` / `0X` hexadecimal.
    Hex,
    /// `0b` / `0B` binary.
    Bin,
    /// `0o` / `0O` octal.
    Oct,
}

/// A token paired with the source span it covers.
#[derive(Copy, Clone, Debug)]
pub struct Lexed {
    /// Lexical category and payload.
    pub kind: Token,
    /// Half-open byte range covering the token in source.
    pub span: Span,
    /// `true` when at least one `\n` byte was consumed as trivia immediately
    /// before this token. The parser uses this to decide whether a postfix
    /// `(`, `[`, or `.` continues the preceding expression or starts a new
    /// statement / trailing block expression (`expressions.md`
    /// §"Statement boundaries"). The lexer's `\r` is treated as plain
    /// whitespace; only `\n` flips the bit.
    pub leading_newline: bool,
}

/// Look up the keyword token matching `text` exactly. Returns `None` for
/// identifiers that are not keywords. Case-sensitive (the CamelCase `Type`
/// meta-type is distinct from the lowercase `type` keyword).
pub fn keyword_token(text: &str) -> Option<Token> {
    Some(match text {
        // Declarations
        "function" => Token::Function,
        "module" => Token::Module,
        "import" => Token::Import,
        "public" => Token::Public,
        "type" => Token::Type,
        "case" => Token::Case,
        "spec" => Token::Spec,
        "extern" => Token::Extern,
        "affine" => Token::Affine,
        "linear" => Token::Linear,
        // Control flow
        "if" => Token::If,
        "else" => Token::Else,
        "match" => Token::Match,
        "for" => Token::For,
        "in" => Token::In,
        "loop" => Token::Loop,
        "break" => Token::Break,
        "continue" => Token::Continue,
        "return" => Token::Return,
        // Bindings
        "let" => Token::Let,
        "var" => Token::Var,
        "uninit" => Token::Uninit,
        // Type / refinement
        "where" => Token::Where,
        "as" => Token::As,
        "wrapping" => Token::Wrapping,
        "saturating" => Token::Saturating,
        "checked" => Token::Checked,
        "requires" => Token::Requires,
        "ensures" => Token::Ensures,
        "decreases" => Token::Decreases,
        "result" => Token::ResultKw,
        "forall" => Token::Forall,
        "exists" => Token::Exists,
        // Effects
        "with" => Token::With,
        "raise" => Token::Raise,
        "panic" => Token::Panic,
        "scope" => Token::ScopeKw,
        "await" => Token::Await,
        // Modes
        "mutable" => Token::Mutable,
        "take" => Token::Take,
        "init" => Token::Init,
        // Comptime
        "comptime" => Token::Comptime,
        "Type" => Token::TypeMeta,
        // Derive
        "derive" => Token::Derive,
        // Reserved
        "yield" => Token::Yield,
        // Boolean literals
        "true" => Token::True,
        "false" => Token::False,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_round_trip_for_every_locked_word() {
        // The locked surface reserves these words. Each must map to its
        // dedicated variant; bare identifiers must not.
        let table: &[(&str, Token)] = &[
            ("function", Token::Function),
            ("module", Token::Module),
            ("import", Token::Import),
            ("public", Token::Public),
            ("type", Token::Type),
            ("case", Token::Case),
            ("spec", Token::Spec),
            ("extern", Token::Extern),
            ("if", Token::If),
            ("else", Token::Else),
            ("match", Token::Match),
            ("for", Token::For),
            ("in", Token::In),
            ("loop", Token::Loop),
            ("break", Token::Break),
            ("continue", Token::Continue),
            ("return", Token::Return),
            ("let", Token::Let),
            ("var", Token::Var),
            ("uninit", Token::Uninit),
            ("where", Token::Where),
            ("as", Token::As),
            ("wrapping", Token::Wrapping),
            ("saturating", Token::Saturating),
            ("checked", Token::Checked),
            ("requires", Token::Requires),
            ("ensures", Token::Ensures),
            ("decreases", Token::Decreases),
            ("result", Token::ResultKw),
            ("forall", Token::Forall),
            ("exists", Token::Exists),
            ("with", Token::With),
            ("raise", Token::Raise),
            ("panic", Token::Panic),
            ("scope", Token::ScopeKw),
            ("await", Token::Await),
            ("mutable", Token::Mutable),
            ("take", Token::Take),
            ("init", Token::Init),
            ("comptime", Token::Comptime),
            ("Type", Token::TypeMeta),
            ("derive", Token::Derive),
            ("yield", Token::Yield),
            ("true", Token::True),
            ("false", Token::False),
        ];
        for (word, expected) in table {
            assert_eq!(
                keyword_token(word),
                Some(*expected),
                "keyword {:?} did not map to expected variant",
                word
            );
        }
    }

    #[test]
    fn keyword_lookup_is_case_sensitive() {
        // The CamelCase `Type` is the meta-type; the lowercase `type` is
        // the declaration keyword.
        assert_eq!(keyword_token("Type"), Some(Token::TypeMeta));
        assert_eq!(keyword_token("type"), Some(Token::Type));
        // Other capitalisations are not keywords.
        assert_eq!(keyword_token("Function"), None);
        assert_eq!(keyword_token("IF"), None);
        assert_eq!(keyword_token("True"), None);
    }

    #[test]
    fn keyword_lookup_returns_none_for_non_keywords() {
        assert_eq!(keyword_token(""), None);
        assert_eq!(keyword_token("x"), None);
        assert_eq!(keyword_token("i32"), None); // primitive type, not a keyword
        assert_eq!(keyword_token("bool"), None);
        assert_eq!(keyword_token("usize"), None);
        assert_eq!(keyword_token("foo_bar"), None);
    }

    #[test]
    fn token_is_copy() {
        // Compile-time check: Copy bound is part of the public contract.
        fn assert_copy<T: Copy>() {}
        assert_copy::<Token>();
        assert_copy::<Lexed>();
        assert_copy::<IntBase>();
    }
}
