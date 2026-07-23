//! HIR — the typed intermediate form.
//!
//! The HIR mirrors the AST shape from
//! `edda_syntax::ast` but carries a [`TyId`](crate::TyId) on every
//! value-producing node — [`HirExpr`], [`HirBlock`], [`HirPat`]. Each
//! node remembers its source [`Span`] for diagnostics.
//!
//! # Coverage
//!
//! This module lands the *data types only*. AST → HIR lowering and the
//! bidirectional inference rules that fill in the `ty` fields live
//! elsewhere. Until those run, the only way to build a [`HirExpr`] is
//! by hand-construction (used by the inline tests); no production code
//! produces HIR yet.
//!
//! Item-level HIR (`HirFile`, `HirFunction`, `HirTypeDecl`, etc.) is
//! also deferred. The value layer (expressions, statements, patterns,
//! blocks) is the substantial part; item wrappers fit on top once the
//! resolution-side work in `edda-resolve` lands and we know
//! exactly which name-resolution data each item should carry.
//!
//! # Re-used AST types
//!
//! The HIR uses several `edda_syntax::ast` types directly because the
//! data shape doesn't change between AST and HIR:
//!
//! - [`ast::BinOp`](edda_syntax::ast::BinOp), [`ast::UnOp`](edda_syntax::ast::UnOp),
//!   [`ast::RangeKind`](edda_syntax::ast::RangeKind) — operator catalogues.
//! - [`ast::AssignOp`](edda_syntax::ast::AssignOp),
//!   [`ast::BindingMode`](edda_syntax::ast::BindingMode) — statement-level enums.
//! - [`ast::Literal`](edda_syntax::ast::Literal) — literal-value carrier;
//!   the parsed integer value and base prefix come over verbatim.
//! - [`ast::Ident`](edda_syntax::ast::Ident) — name + span; used for
//!   per-segment path entries, field names, method names, and loop labels.
//!
//! # Resolution status
//!
//! [`HirPath`] is structurally identical to `ast::Path` —
//! a non-empty sequence of [`Ident`]s with a span. When
//! `edda-resolve` exposes item-level binding ids, [`HirPath`]
//! will gain a resolution field; the variant set in [`HirExprKind`] and
//! [`HirPatKind`] will not need to change.

mod expr;
mod pat;
mod stmt;

pub use expr::{
    HirBlock, HirCallArg, HirCallMode, HirCapture, HirClosure, HirClosureParam, HirExpr,
    HirExprKind, HirFStringPart, HirMatchArm, HirSpawn, HirSpawnArg, HirStructLitField,
};
pub use pat::{HirPat, HirPatKind, HirStructPatField, HirVariantPatPayload};
pub use stmt::{HirStmt, HirStmtKind};

use edda_span::Span;
use edda_syntax::ast::Ident;

/// A path reference inside the HIR — a non-empty sequence of identifier
/// segments. Identical in shape to
/// [`ast::Path`](edda_syntax::ast::Path); the future resolution field
/// will be added without renaming, so consumers that match on
/// [`HirPath::segments`] continue to compile.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct HirPath {
    /// Source range covering the whole path.
    pub span: Span,
    /// Path segments in source order. Always at least one segment.
    pub segments: Box<[Ident]>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use edda_intern::Interner;

    fn ident(interner: &Interner, name: &str) -> Ident {
        Ident {
            name: interner.intern(name),
            span: Span::DUMMY,
        }
    }

    #[test]
    fn hir_path_roundtrips_through_clone_and_equality() {
        let interner = Interner::new();
        let a = HirPath {
            span: Span::DUMMY,
            segments: Box::from([ident(&interner, "std"), ident(&interner, "fs")]),
        };
        let b = a.clone();
        assert_eq!(a, b);

        let c = HirPath {
            span: Span::DUMMY,
            segments: Box::from([ident(&interner, "std")]),
        };
        assert_ne!(a, c);
    }
}
