//! Cheap pre-scan for the D-22 comptime forms + generic child traversal.
//!
//! Before cloning a function body the expander asks
//! [`block_has_comptime_construct`] whether any `comptime for`,
//! `comptime if`, `CompField`, or bare `field_count(...)` call appears
//! anywhere inside it — bodies that need no expansion are typechecked
//! against the original borrowed AST with no clone. The `visit_child_*`
//! helpers are the generic structural walkers the pre-scan recurses
//! through.

use edda_intern::Interner;
use edda_syntax::ast::{Block, Expr, ExprKind, Stmt, StmtKind};

/// Cheap pre-scan: does this block contain a `comptime for`, a
/// `comptime if`, a `CompField`, or a bare `field_count(...)` call
/// anywhere? Avoids cloning bodies that need no expansion.
pub(super) fn block_has_comptime_construct(block: &Block, interner: &Interner) -> bool {
    block.stmts.iter().any(|s| stmt_has_construct(s, interner))
        || block.trailing.as_deref().is_some_and(|e| expr_has_construct(e, interner))
}

fn stmt_has_construct(s: &Stmt, interner: &Interner) -> bool {
    match &s.kind {
        StmtKind::Let { init, .. } => {
            init.as_ref().is_some_and(|e| expr_has_construct(e, interner))
        }
        StmtKind::Assign { target, rhs, .. } => {
            expr_has_construct(target, interner) || expr_has_construct(rhs, interner)
        }
        StmtKind::Expr(e) => expr_has_construct(e, interner),
    }
}

fn expr_has_construct(e: &Expr, interner: &Interner) -> bool {
    if let ExprKind::CompField { .. } = &e.kind {
        return true;
    }
    if let ExprKind::Comptime(inner) = &e.kind
        && matches!(inner.kind, ExprKind::For { .. } | ExprKind::If { .. })
    {
        return true;
    }
    // Bare `field_count(T)` call outside a loop bound —
    // the expander folds it wherever
    // it appears, so the pre-scan must not skip cloning a body whose
    // only D-22-adjacent shape is this call.
    if is_field_count_call(e, interner) {
        return true;
    }
    let mut found = false;
    visit_child_exprs(e, &mut |c| found |= expr_has_construct(c, interner));
    if found {
        return true;
    }
    visit_child_blocks(e, &mut |b| found |= block_has_comptime_construct(b, interner));
    found
}

/// Cheap structural test: is `e` a call whose callee's last path segment
/// is named `field_count`? Mirrors the shape
/// [`super::expander::Expander::field_count_target`] resolves precisely
/// (against the [`super::ShapeIndex`]) — the pre-scan only needs to know
/// whether cloning is worthwhile, not whether the call actually folds.
fn is_field_count_call(e: &Expr, interner: &Interner) -> bool {
    let ExprKind::Call { callee, .. } = &e.kind else {
        return false;
    };
    let ExprKind::Path(p) = &callee.kind else {
        return false;
    };
    p.segments.last().is_some_and(|s| interner.resolve(s.name) == "field_count")
}

// --- generic child traversal helpers (used by the pre-scan only) ---

fn visit_child_exprs(e: &Expr, f: &mut impl FnMut(&Expr)) {
    match &e.kind {
        ExprKind::FString(parts) => {
            for part in parts {
                if let edda_syntax::ast::FStringPart::Slot(slot) = part {
                    f(slot);
                }
            }
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            f(lhs);
            f(rhs);
        }
        ExprKind::Unary { expr, .. }
        | ExprKind::Try(expr)
        | ExprKind::Await(expr)
        | ExprKind::Raise(expr)
        | ExprKind::Panic(expr)
        | ExprKind::Comptime(expr) => f(expr),
        ExprKind::Call { callee, args } => {
            f(callee);
            for a in args {
                f(&a.expr);
            }
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            f(receiver);
            for a in args {
                f(&a.expr);
            }
        }
        ExprKind::Field { receiver, .. } | ExprKind::TupleIndex { receiver, .. } => f(receiver),
        ExprKind::CompField { receiver, index } | ExprKind::Index { receiver, index } => {
            f(receiver);
            f(index);
        }
        ExprKind::Cast { expr, .. } => f(expr),
        ExprKind::Range { lo, hi, .. } => {
            if let Some(e) = lo {
                f(e);
            }
            if let Some(e) = hi {
                f(e);
            }
        }
        ExprKind::Tuple(es) | ExprKind::Array(es) => es.iter().for_each(f),
        ExprKind::StructLit { fields, .. } => fields.iter().for_each(|sf| f(&sf.value)),
        ExprKind::If { cond, else_branch, .. } => {
            f(cond);
            if let Some(e) = else_branch {
                f(e);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            f(scrutinee);
            for a in arms {
                if let Some(g) = &a.guard {
                    f(g);
                }
                f(&a.body);
            }
        }
        ExprKind::Loop { decreases, .. } => {
            if let Some(e) = decreases {
                f(e);
            }
        }
        ExprKind::For { iter, .. } => f(iter),
        ExprKind::Return(opt) | ExprKind::Break { value: opt, .. } => {
            if let Some(e) = opt {
                f(e);
            }
        }
        ExprKind::Handle { recovery, .. } => f(recovery),
        ExprKind::Forall { iter, body, .. } | ExprKind::Exists { iter, body, .. } => {
            f(iter);
            f(body);
        }
        ExprKind::Literal(_)
        | ExprKind::Path(_)
        | ExprKind::Block(_)
        | ExprKind::ComptimeBlock(_)
        | ExprKind::Scope { .. }
        | ExprKind::Continue { .. }
        | ExprKind::EffectRow(_)
        | ExprKind::Closure(_)
        | ExprKind::Spawn(_)
        | ExprKind::Error => {}
    }
}

fn visit_child_blocks(e: &Expr, f: &mut impl FnMut(&Block)) {
    match &e.kind {
        ExprKind::Block(b)
        | ExprKind::ComptimeBlock(b)
        | ExprKind::Loop { body: b, .. }
        | ExprKind::For { body: b, .. }
        | ExprKind::Scope { body: b, .. } => f(b),
        ExprKind::If { then_block, else_branch, .. } => {
            f(then_block);
            if let Some(e) = else_branch {
                visit_child_blocks(e, f);
            }
        }
        ExprKind::Handle { body, .. } => f(body),
        _ => {}
    }
}
