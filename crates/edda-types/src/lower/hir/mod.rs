//! AST → HIR structural lowering.
//!
//! Walks an `edda_syntax::ast::*` tree and produces the structurally
//! equivalent `crate::Hir*` tree. The mapping is mechanical: every
//! `ast::ExprKind` / `ast::PatKind` / `ast::StmtKind` variant has a
//! matching HIR variant; the lowering recurses through `Box` / `Vec`
//! and copies `Copy`-typed payloads (operators, literals, idents,
//! ranges, modes) verbatim.
//!
//! # Type-carrier fields
//!
//! Every `HirExpr` / `HirBlock` / `HirPat` produced by this pass has
//! `ty = TyInterner::error()`. The bidirectional inference pass is
//! what populates real types; the structural lowering pass is purely
//! shape-preserving. Downstream passes that consume HIR before that
//! inference pass runs must treat the `ty` field as a placeholder.
//!
//! # Embedded type lowering
//!
//! Two AST forms carry an `ast::Type` directly inside an expression /
//! statement and need an immediate [`TyId`]:
//!
//! - `Cast { expr, ty }` — the cast's target type lowers via
//!   [`lower_type`] and ends up in `HirExprKind::Cast.target_ty`.
//! - `StmtKind::Let { ty, .. }` — the annotated type, when present,
//!   lowers via [`lower_type`] and ends up in `HirStmtKind::Let.ty`.
//!
//! Both inherit `lower_type`'s diagnostics for invalid target types.
//!
//! # No diagnostics from the structural mapping itself
//!
//! Per-variant mapping is total — every AST variant has a HIR
//! counterpart. Diagnostics arise only from `lower_type` (cast / let
//! annotation cascade) and are surfaced through the caller's
//! [`Diagnostics`] take.
//!
//! Module layout:
//!
//! - [`expr`] — `lower_expr` / `lower_block` and the expression arms.
//! - [`stmt`] — `lower_stmt`.
//! - [`pat`] — `lower_pat`.
//! - [`path`] — bound-head Path / Call decomposition.

mod expr;
mod path;
mod pat;
mod stmt;

pub(crate) use expr::{lower_block, lower_expr};
#[cfg(test)]
pub(in crate::lower) use pat::lower_pat;
#[cfg(test)]
pub(in crate::lower) use stmt::lower_stmt;

use crate::ty::TyId;

// Suppress an unused-let warning: `_ = lower_type(...)` is fine but
// rustc warns on `TyId` if we ever forget to use it. The `_: TyId =`
// pattern keeps the type assertion visible without the warning.
const _: fn(TyId) = |_id| {};

#[cfg(test)]
use super::LowerCx;
#[cfg(test)]
use crate::hir::{
    HirExpr, HirExprKind, HirPat, HirPatKind, HirStmt, HirStmtKind, HirVariantPatPayload,
};

#[cfg(test)]
#[path = "hir_tests.rs"]
mod tests;
