//! Lexer, parser, AST, and pretty-printer.
//!
//! Implements the locked language surface:
//!   - `docs/syntax/declarations.md` — keywords, function form, sum types, modules
//!   - `docs/syntax/expressions.md` — statements vs expressions, operators, precedence
//!   - `docs/syntax/types.md`       — primitives, tuples, slices, parameter modes
//!   - `docs/syntax/effects.md`     — effect rows, `?` propagation, `scope(exec)`
//!   - `docs/syntax/refinements.md` — `where`, `requires`, `ensures`
//!   - `docs/syntax/comptime.md`    — `comptime`, `spec`, the `Type` meta-type
//!
//! The pretty-printer's round-trip rule (`docs/tooling/structural-edits.md`)
//! is owned here: parse → print → parse must produce the same AST.
//!
//! # Status
//!
//! - **Lexer**: complete for the locked token set.
//! - **AST types**: complete for the locked surface.
//! - **Parser**: complete for the locked surface — top-level
//!   items, statements, expressions, types, and patterns.
//! - **Pretty-printer**: complete with the locked round-trip rule
//!   ([`docs/tooling/structural-edits.md`]).

pub mod ast;
mod lexer;
mod parser;
mod printer;
mod token;

pub use lexer::{Lexer, comment_spans, lex, strip_comments};
pub use parser::{Parser, parse_block, parse_expr, parse_file};
pub use printer::{Printer, print_file};
pub use token::{IntBase, Lexed, Token, keyword_token};
