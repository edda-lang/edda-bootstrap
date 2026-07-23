//! Item-level attribute AST.
//!
//! Attributes are `@name(arg, ..., key = value, ...)` clauses attached to
//! an [`super::Item`] between its leading doc-comments and its visibility
//! modifier. The parser produces them without semantic interpretation;
//! `edda-types` validates each attribute against the locked registry
//! (`@export`, `@abi`, `@align`, `@repr`, `@layout`) in a later slice.

use edda_intern::Symbol;
use edda_span::Span;

use super::Ident;
use crate::token::IntBase;

/// A single `@name(args)` attribute attached to an [`super::Item`].
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Attribute {
    /// Source range covering the entire attribute, from `@` through `)`.
    pub span: Span,
    /// Attribute name (e.g. `export`, `abi`, `align`, `repr`, `layout`).
    pub name: Ident,
    /// Argument list in source order. May be empty for `@name()`.
    pub args: Vec<AttrArg>,
}

/// One argument of an [`Attribute`]. Positional (`Ident` / `Lit`) and
/// named (`key = value`) forms.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum AttrArg {
    /// Bare identifier — e.g. `@repr(C)` or `@layout(natural)`.
    Ident(Ident),
    /// Bare literal — e.g. `@export("name")` or `@align(8)`.
    Lit {
        /// Source range covering the literal token.
        span: Span,
        /// Literal payload.
        lit: AttrLit,
    },
    /// `<key> = <value>` named arg — e.g. `@align(value = 16)`.
    Named {
        /// Source range covering `<key> = <value>`.
        span: Span,
        /// LHS of the `=` — argument keyword.
        key: Ident,
        /// RHS of the `=`. Always an `Ident` or `Lit` in well-formed
        /// source; nested `Named` is rejected by the parser.
        value: Box<AttrArg>,
    },
}

/// Literal values admitted inside an [`AttrArg`]. A strict subset of
/// [`super::Literal`] — only the three forms the locked registry needs.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum AttrLit {
    /// `"..."` plain string literal (escape-resolved by the lexer).
    Str(Symbol),
    /// Unsigned integer literal. The original `base` is preserved so the
    /// printer can round-trip the user's source spelling.
    Int {
        /// Parsed value.
        value: u128,
        /// Original numeric base.
        base: IntBase,
    },
    /// `true` or `false`.
    Bool(bool),
}
