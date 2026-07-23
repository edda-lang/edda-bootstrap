//! Read-only visitor over the locked [`super::ExprKind`] surface.
//!
//! Implements the classic Rust visitor pattern (cf. `syn::visit::Visit`,
//! `rustc_ast::visit::Visitor`): the [`Visitor`] trait carries one
//! `visit_*` method per AST shape with a default impl that forwards
//! to the matching `walk_*` free function. Concrete passes implement
//! [`Visitor`] for their state struct, override only the variants they
//! care about, and inside each override call the matching `walk_*`
//! function to recurse into children.
//!
//! The trait is parametrised over `'ast`, the lifetime of the AST being
//! visited, so visitor state can borrow references back out of the tree
//! (e.g., a `Vec<&'ast Expr>` collector). Visitors that don't need to
//! hold references still pay only the syntactic tax of writing
//! `<'ast>` in the `impl` header.
//!
//! The trait deliberately offers methods only for the AST shapes the
//! post-typecheck passes traverse — [`Block`], [`Stmt`],
//! [`Expr`], [`MatchArm`]. Patterns, types, refinements, and items
//! are not visited here because no current consumer walks them;
//! extension is future work.
//!
//! # Example
//!
//! ```ignore
//! use edda_syntax::ast::visit::{Visitor, walk_expr};
//! use edda_syntax::ast::{Expr, ExprKind};
//!
//! struct CallCounter { count: usize }
//!
//! impl<'ast> Visitor<'ast> for CallCounter {
//!     fn visit_expr(&mut self, expr: &'ast Expr) {
//!         if let ExprKind::Call { .. } = &expr.kind {
//!             self.count += 1;
//!         }
//!         walk_expr(self, expr);
//!     }
//! }
//! ```

use super::{Block, Expr, ExprKind, MatchArm, Stmt, StmtKind};

/// Read-only visitor over [`Block`], [`Stmt`], [`Expr`], and [`MatchArm`].
///
/// Override only the `visit_*` methods that pertain to your pass; call
/// the matching `walk_*` function inside an override to continue
/// recursion into the node's children.
pub trait Visitor<'ast> {
    /// Visit a [`Block`]. The default recurses through every statement
    /// in source order and then the optional trailing expression via
    /// [`walk_block`].
    fn visit_block(&mut self, block: &'ast Block) {
        walk_block(self, block);
    }

    /// Visit a [`Stmt`]. The default recurses through the statement's
    /// child expressions (`init`, `target` + `rhs`, or wrapped `Expr`)
    /// via [`walk_stmt`].
    fn visit_stmt(&mut self, stmt: &'ast Stmt) {
        walk_stmt(self, stmt);
    }

    /// Visit an [`Expr`]. The default recurses through every child
    /// expression of the variant via [`walk_expr`].
    fn visit_expr(&mut self, expr: &'ast Expr) {
        walk_expr(self, expr);
    }

    /// Visit a [`MatchArm`]. The default recurses through the optional
    /// guard and the arm body via [`walk_match_arm`].
    fn visit_match_arm(&mut self, arm: &'ast MatchArm) {
        walk_match_arm(self, arm);
    }
}

/// Recurse into a block's statements and trailing expression in source
/// order. Each child is delivered through [`Visitor::visit_stmt`] /
/// [`Visitor::visit_expr`] so any per-pass override participates.
pub fn walk_block<'ast, V: Visitor<'ast> + ?Sized>(visitor: &mut V, block: &'ast Block) {
    for stmt in &block.stmts {
        visitor.visit_stmt(stmt);
    }
    if let Some(trailing) = &block.trailing {
        visitor.visit_expr(trailing);
    }
}

/// Recurse into a statement's child expressions: `init` for `Let`,
/// then `target` and `rhs` for `Assign`, the wrapped expression for
/// `Expr`. Each is delivered through [`Visitor::visit_expr`].
pub fn walk_stmt<'ast, V: Visitor<'ast> + ?Sized>(visitor: &mut V, stmt: &'ast Stmt) {
    match &stmt.kind {
        StmtKind::Let { init, .. } => {
            if let Some(e) = init {
                visitor.visit_expr(e);
            }
        }
        StmtKind::Assign { target, rhs, .. } => {
            visitor.visit_expr(target);
            visitor.visit_expr(rhs);
        }
        StmtKind::Expr(e) => visitor.visit_expr(e),
    }
}

/// Recurse into every child expression of `expr` according to its
/// [`ExprKind`] variant. Block-bearing variants forward through
/// [`Visitor::visit_block`]; arm-bearing variants forward through
/// [`Visitor::visit_match_arm`]; everything else forwards through
/// [`Visitor::visit_expr`].
pub fn walk_expr<'ast, V: Visitor<'ast> + ?Sized>(visitor: &mut V, expr: &'ast Expr) {
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            visitor.visit_expr(callee);
            for a in args {
                visitor.visit_expr(&a.expr);
            }
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            visitor.visit_expr(receiver);
            for a in args {
                visitor.visit_expr(&a.expr);
            }
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            visitor.visit_expr(lhs);
            visitor.visit_expr(rhs);
        }
        ExprKind::Unary { expr: inner, .. } => visitor.visit_expr(inner),
        ExprKind::Field { receiver, .. } => visitor.visit_expr(receiver),
        ExprKind::TupleIndex { receiver, .. } => visitor.visit_expr(receiver),
        ExprKind::CompField { receiver, index } => {
            visitor.visit_expr(receiver);
            visitor.visit_expr(index);
        }
        ExprKind::Index { receiver, index } => {
            visitor.visit_expr(receiver);
            visitor.visit_expr(index);
        }
        ExprKind::If {
            cond,
            then_block,
            else_branch,
        } => {
            visitor.visit_expr(cond);
            visitor.visit_block(then_block);
            if let Some(eb) = else_branch {
                visitor.visit_expr(eb);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            visitor.visit_expr(scrutinee);
            for arm in arms {
                visitor.visit_match_arm(arm);
            }
        }
        ExprKind::Block(b) | ExprKind::ComptimeBlock(b) => visitor.visit_block(b),
        ExprKind::Cast { expr: inner, .. } => visitor.visit_expr(inner),
        ExprKind::Range { lo, hi, .. } => {
            if let Some(s) = lo {
                visitor.visit_expr(s);
            }
            if let Some(e) = hi {
                visitor.visit_expr(e);
            }
        }
        ExprKind::Tuple(elems) => {
            for e in elems {
                visitor.visit_expr(e);
            }
        }
        ExprKind::Array(elems) => {
            for e in elems {
                visitor.visit_expr(e);
            }
        }
        ExprKind::StructLit { fields, .. } => {
            for f in fields {
                visitor.visit_expr(&f.value);
            }
        }
        ExprKind::Loop {
            body, decreases, ..
        } => {
            visitor.visit_block(body);
            if let Some(m) = decreases {
                visitor.visit_expr(m);
            }
        }
        ExprKind::For { iter, body, .. } => {
            visitor.visit_expr(iter);
            visitor.visit_block(body);
        }
        ExprKind::Try(inner)
        | ExprKind::Await(inner)
        | ExprKind::Raise(inner)
        | ExprKind::Panic(inner)
        | ExprKind::Comptime(inner) => visitor.visit_expr(inner),
        ExprKind::Scope { body, .. } => visitor.visit_block(body),
        ExprKind::Return(opt) => {
            if let Some(e) = opt {
                visitor.visit_expr(e);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(v) = value {
                visitor.visit_expr(v);
            }
        }
        ExprKind::Handle { recovery, body, .. } => {
            visitor.visit_expr(recovery);
            visitor.visit_block(body);
        }
        ExprKind::Closure(c) => visitor.visit_block(&c.body),
        ExprKind::Spawn(s) => {
            for arg in &s.args {
                visitor.visit_expr(&arg.init);
            }
            visitor.visit_block(&s.body);
        }
        ExprKind::Forall { iter, body, .. } | ExprKind::Exists { iter, body, .. } => {
            visitor.visit_expr(iter);
            visitor.visit_expr(body);
        }
        ExprKind::FString(parts) => {
            for part in parts {
                if let crate::ast::FStringPart::Slot(e) = part {
                    visitor.visit_expr(e);
                }
            }
        }
        ExprKind::Literal(_)
        | ExprKind::Path(_)
        | ExprKind::Continue { .. }
        | ExprKind::EffectRow(_)
        | ExprKind::Error => {}
    }
}

/// Recurse into a match arm: visits the optional guard, then the
/// body. Each is delivered through [`Visitor::visit_expr`].
pub fn walk_match_arm<'ast, V: Visitor<'ast> + ?Sized>(visitor: &mut V, arm: &'ast MatchArm) {
    if let Some(g) = &arm.guard {
        visitor.visit_expr(g);
    }
    visitor.visit_expr(&arm.body);
}
