//! Statement lowering for the AST → HIR pass.

use edda_diag::{Diagnostics, LintConfig};
use edda_syntax::ast;

use crate::hir::{HirStmt, HirStmtKind};

use super::super::{LowerCx, ty::lower_type};
use super::expr::lower_expr;
use super::pat::lower_pat;

/// Lower an AST statement to its HIR counterpart.
pub(crate) fn lower_stmt(
    stmt: &ast::Stmt,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> HirStmt {
    let kind = match &stmt.kind {
        ast::StmtKind::Let {
            mutability,
            pat,
            ty,
            init,
        } => HirStmtKind::Let {
            mutability: *mutability,
            pat: lower_pat(pat, cx, diags, lint_cfg),
            ty: ty.as_ref().map(|t| lower_type(t, cx, diags, lint_cfg)),
            init: init.as_ref().map(|e| lower_expr(e, cx, diags, lint_cfg)),
        },
        ast::StmtKind::Assign { target, op, rhs } => HirStmtKind::Assign {
            target: lower_expr(target, cx, diags, lint_cfg),
            op: *op,
            rhs: lower_expr(rhs, cx, diags, lint_cfg),
        },
        ast::StmtKind::Expr(e) => HirStmtKind::Expr(lower_expr(e, cx, diags, lint_cfg)),
    };
    HirStmt {
        span: stmt.span,
        kind,
    }
}
