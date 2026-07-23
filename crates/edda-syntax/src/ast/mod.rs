//! Abstract-syntax-tree data types for Edda.
//!
//! The AST is the parser's output: a span-decorated tree representing the
//! locked language surface (declarations, expressions, types, effects,
//! refinements, comptime). Every node carries a [`Span`] so diagnostics
//! and structural-edit operations can address arbitrary subtrees.
//!
//! # Conventions
//!
//! - Every AST node is a struct wrapper `{ span, kind }`. The `kind` enum
//!   carries the actual variant data. This matches rustc's pattern and
//!   keeps the per-variant inline doc cohesive.
//! - Recursion goes through `Box<T>` (for single children) and `Vec<T>`
//!   (for sequences). No node carries a parent pointer; consumers walk
//!   the tree explicitly.
//! - No `NodeId` field at AST level — IDs are an `edda-resolve` concern,
//!   added when name-resolution lands.
//! - The parser produces `kind: ExprKind::Error` (and similar `Error`
//!   variants on `Pat`, `Type`) when it recovers from a syntax error;
//!   downstream passes treat these as already-diagnosed.

use edda_intern::Symbol;
use edda_span::Span;

mod attr;
mod expr;
mod item;
mod pat;
mod stmt;
mod ty;
pub mod visit;

pub use attr::{AttrArg, AttrLit, Attribute};
pub use expr::{
    BinOp, Block, CallArg, CallMode, Capture, CaptureMode, CastMode, Closure, Expr, ExprKind,
    FStringPart, Literal, MatchArm, RangeKind, ScopeKind, SpawnArg, SpawnExpr, StructLitField,
    UnOp,
};
pub use item::{
    AdmitsConstraint, Derive, FnBody, FnDecl, GenericKind, GenericParam, Import, Item, ItemKind,
    LetDecl, Linearity, ModuleDecl, Param, Spec, SpecInvocation, Stability, TypeDecl, TypeDeclKind,
    TypeField, Variant, VariantPayload, Visibility,
};
pub use pat::{Pat, PatKind, StructPatField, VariantPatPayload};
pub use stmt::{AssignOp, BindingMode, Stmt, StmtKind};
pub use ty::{
    EffectMember, EffectRow, FnTypeParam, ParamMode, RefinementClause, RefinementKind, ReturnMode,
    Type, TypeKind,
};

/// A single source identifier.
///
/// Distinct from [`Path`]: an `Ident` is one segment; a `Path` is one or
/// more dot-separated segments. The parser emits `Ident` for binding
/// occurrences (function name, parameter name, type-decl name) and
/// `Path` for any reference that admits a qualified spelling.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct Ident {
    /// Interned text of the identifier.
    pub name: Symbol,
    /// Source range covering exactly the identifier bytes.
    pub span: Span,
}

/// Dot-separated identifier path used wherever a qualified name appears
/// (`std.fs.read`, `MyType.variant`, `package_root.module.item`).
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct Path {
    /// One or more dot-separated segments, in source order.
    pub segments: Vec<Ident>,
    /// Source range covering the entire path.
    pub span: Span,
}

/// Importance tier of a doc-comment line per `01-syntax.md` §3.2.
///
/// The structure map surfaces these to the LLM author at different weights:
/// [`DocTier::High`] is for load-bearing claims, [`DocTier::Medium`] is the
/// default item-level tier, [`DocTier::Low`] is the file-level / quiet tier.
/// [`DocTier::Legacy`] covers `///` lines pending the corpus migration to
/// the codex-locked markers.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum DocTier {
    /// `!!!` — high-importance load-bearing claim.
    High,
    /// `/!!` — medium tier (the default for item-level docs).
    Medium,
    /// `//!` — low tier. The file-level form is the file's module doc.
    Low,
    /// `///` — legacy tier from the bootstrap-side structmap protocol;
    /// in-flight pending migration to `/!!` (item-level) or `//!` (file-level).
    Legacy,
}

/// One doc-comment line carrying its tier, source span, and interned body text.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct DocLine {
    /// Tier marker the line was authored with.
    pub tier: DocTier,
    /// Source range covering the marker and the body (excluding the newline).
    pub span: Span,
    /// Interned body text, with the leading space and trailing whitespace trimmed.
    pub body: Symbol,
}

/// The top-level AST for a single source file. One file is one module.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct File {
    /// Source range covering the entire file.
    pub span: Span,
    /// File-head doc-comments. The codex defines the file-level doc as
    /// `//!` at the file's top; the parser admits any tier here and the
    /// structure map weights them per [`DocTier`].
    pub doc: Vec<DocLine>,
    /// Items declared in the file, in source order.
    pub items: Vec<Item>,
}
