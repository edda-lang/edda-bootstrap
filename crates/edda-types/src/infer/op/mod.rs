//! Binary and unary operator inference.
//!
//! Implements the operator rules from `docs/types/inference-rules.md
//! §1a.4`:
//!
//! - **T-BinaryArith-Int** — arithmetic operators (`+ - * / %`) unify
//!   the operand types and require both to be numeric. Floats follow
//!   the same shape (not separately listed in §1a.4; treated as a
//!   uniform `T-BinaryArith` here).
//! - **Comparison** (`== != < <= > >=`) — operands unify; result is
//!   `bool`. `<`/`<=`/`>`/`>=` further require the unified type to be
//!   numeric (ordering relations aren't admitted on `bool` / `String`
//!   / unit / never).
//! - **Logical** (`&& ||`) — both operands check against `bool`;
//!   result is `bool`.
//! - **Bitwise** (`& | ^`) and **Shift** (`<< >>`) — operands unify;
//!   result equals the unified type; require integer-typed operands.
//! - **Unary** (`- ! ~`) — `Neg` requires numeric, `Not` requires
//!   `bool`, `BitNot` requires integer.
//!
//! Cascade diagnostics from an already-`Error` operand are
//! propagated silently — only the leaf failure emits.
//!
//! Module layout:
//!
//! - [`binary`] — binary-operator synth / check dispatchers + helpers.
//! - [`unary`] — unary-operator synth / check dispatchers.
//! - [`overflow`] — `err: Overflow` row attachment for checked forms.

mod binary;
mod overflow;
mod unary;

pub(in crate::infer) use binary::{check_binary, synth_binary};
pub(in crate::infer) use overflow::attach_overflow_row_for_cast;
pub(in crate::infer) use unary::{check_unary, synth_unary};

#[cfg(test)]
use super::{check_expr, synth_expr};
#[cfg(test)]
use crate::hir::HirExpr;
#[cfg(test)]
use crate::infer::{InferCx, TyEnv};
#[cfg(test)]
use crate::prim::Primitive;
#[cfg(test)]
use crate::ty::TyId;
#[cfg(test)]
use edda_span::Span;
#[cfg(test)]
use edda_syntax::ast::{BinOp, UnOp};

#[cfg(test)]
#[path = "op_tests.rs"]
mod tests;
