//! Call-site collection over function bodies and trust-attribute parsing.
//!
//! `collect_calls_*` walk an Edda function body and accumulate every direct
//! call / method-call name; the trust helpers read `@unverified` / `@trust`.

use edda_intern::Interner;
use edda_syntax::ast::{
    Attribute, AttrArg, AttrLit, Block, CallArg, Expr, ExprKind, MatchArm, Stmt, StmtKind,
};

use super::items::path_string;
use super::render::interner_text;
use crate::model::TrustKind;

pub(super) fn collect_calls_block(block: &Block, interner: &Interner, out: &mut Vec<String>) {
    for stmt in &block.stmts {
        collect_calls_stmt(stmt, interner, out);
    }
    if let Some(trailing) = &block.trailing {
        collect_calls_expr(trailing, interner, out);
    }
}

fn collect_calls_stmt(stmt: &Stmt, interner: &Interner, out: &mut Vec<String>) {
    match &stmt.kind {
        StmtKind::Let { init, .. } => {
            if let Some(e) = init {
                collect_calls_expr(e, interner, out);
            }
        }
        StmtKind::Assign { target, rhs, .. } => {
            collect_calls_expr(target, interner, out);
            collect_calls_expr(rhs, interner, out);
        }
        StmtKind::Expr(e) => collect_calls_expr(e, interner, out),
    }
}

fn collect_calls_expr(expr: &Expr, interner: &Interner, out: &mut Vec<String>) {
    match &expr.kind {
        ExprKind::Literal(_) | ExprKind::Path(_) | ExprKind::Error => {}
        ExprKind::FString(parts) => {
            for part in parts {
                if let edda_syntax::ast::FStringPart::Slot(slot) = part {
                    collect_calls_expr(slot, interner, out);
                }
            }
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            collect_calls_expr(lhs, interner, out);
            collect_calls_expr(rhs, interner, out);
        }
        ExprKind::Unary { expr, .. } => collect_calls_expr(expr, interner, out),
        ExprKind::Call { callee, args } => {
            if let ExprKind::Path(p) = &callee.kind {
                out.push(path_string(p, interner));
            } else {
                collect_calls_expr(callee, interner, out);
            }
            collect_calls_args(args, interner, out);
        }
        ExprKind::MethodCall { receiver, name, args } => {
            collect_calls_expr(receiver, interner, out);
            out.push(format!(".{}", interner_text(interner, name.name)));
            collect_calls_args(args, interner, out);
        }
        ExprKind::Field { receiver, .. } => collect_calls_expr(receiver, interner, out),
        ExprKind::TupleIndex { receiver, .. } => collect_calls_expr(receiver, interner, out),
        ExprKind::CompField { receiver, index } => {
            collect_calls_expr(receiver, interner, out);
            collect_calls_expr(index, interner, out);
        }
        ExprKind::Index { receiver, index } => {
            collect_calls_expr(receiver, interner, out);
            collect_calls_expr(index, interner, out);
        }
        ExprKind::If { cond, then_block, else_branch } => {
            collect_calls_expr(cond, interner, out);
            collect_calls_block(then_block, interner, out);
            if let Some(e) = else_branch {
                collect_calls_expr(e, interner, out);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_calls_expr(scrutinee, interner, out);
            for arm in arms {
                collect_calls_arm(arm, interner, out);
            }
        }
        ExprKind::Block(b)
        | ExprKind::ComptimeBlock(b) => collect_calls_block(b, interner, out),
        ExprKind::Cast { expr, .. } => collect_calls_expr(expr, interner, out),
        ExprKind::Range { lo, hi, .. } => {
            if let Some(e) = lo {
                collect_calls_expr(e, interner, out);
            }
            if let Some(e) = hi {
                collect_calls_expr(e, interner, out);
            }
        }
        ExprKind::Tuple(elems) | ExprKind::Array(elems) => {
            for e in elems {
                collect_calls_expr(e, interner, out);
            }
        }
        ExprKind::StructLit { fields, .. } => {
            for f in fields {
                collect_calls_expr(&f.value, interner, out);
            }
        }
        ExprKind::Loop { body, decreases, .. } => {
            collect_calls_block(body, interner, out);
            if let Some(d) = decreases {
                collect_calls_expr(d, interner, out);
            }
        }
        ExprKind::For { iter, body, .. } => {
            collect_calls_expr(iter, interner, out);
            collect_calls_block(body, interner, out);
        }
        ExprKind::Try(e)
        | ExprKind::Await(e)
        | ExprKind::Raise(e)
        | ExprKind::Panic(e)
        | ExprKind::Comptime(e) => collect_calls_expr(e, interner, out),
        ExprKind::Scope { body, .. } => collect_calls_block(body, interner, out),
        ExprKind::Return(opt) => {
            if let Some(e) = opt {
                collect_calls_expr(e, interner, out);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(e) = value {
                collect_calls_expr(e, interner, out);
            }
        }
        ExprKind::Continue { .. } | ExprKind::EffectRow(_) => {}
        ExprKind::Closure(c) => collect_calls_block(&c.body, interner, out),
        ExprKind::Handle { recovery, body, .. } => {
            collect_calls_expr(recovery, interner, out);
            collect_calls_block(body, interner, out);
        }
        ExprKind::Spawn(_) => {
            // Spawn payload is `Box<SpawnExpr>` — its body is a Block;
            // structmap deliberately stops at the spawn boundary so the
            // call list reflects the enclosing function's direct effect
            // exposure rather than the spawned task's body. The task's
            // own structmap entry (when nested-function support lands)
            // will report its own calls.
        }
        ExprKind::Forall { iter, body, .. } | ExprKind::Exists { iter, body, .. } => {
            // Bounded quantifiers are refinement-only forms; their iter
            // and body sub-expressions may still carry calls (e.g.
            // `forall i in 0..<xs.len(): ...` calls `.len()`).
            collect_calls_expr(iter, interner, out);
            collect_calls_expr(body, interner, out);
        }
    }
}

fn collect_calls_args(args: &[CallArg], interner: &Interner, out: &mut Vec<String>) {
    for a in args {
        collect_calls_expr(&a.expr, interner, out);
    }
}

fn collect_calls_arm(arm: &MatchArm, interner: &Interner, out: &mut Vec<String>) {
    if let Some(g) = &arm.guard {
        collect_calls_expr(g, interner, out);
    }
    collect_calls_expr(&arm.body, interner, out);
}

pub(super) fn trust_kind_for(attr: &Attribute, interner: &Interner) -> Option<TrustKind> {
    match interner.try_resolve(attr.name.name)? {
        "unverified" => Some(TrustKind::Unverified),
        "trust" => Some(TrustKind::Trust),
        _ => None,
    }
}

pub(super) fn trust_reason(attr: &Attribute, interner: &Interner) -> Option<String> {
    // Look for a `reason = "..."` named argument or a single literal string.
    for arg in &attr.args {
        match arg {
            AttrArg::Named { key, value, .. } => {
                if interner.try_resolve(key.name) == Some("reason")
                    && let AttrArg::Lit { lit: AttrLit::Str(sym), .. } = value.as_ref()
                {
                    return Some(interner_text(interner, *sym).to_string());
                }
            }
            AttrArg::Lit { lit: AttrLit::Str(sym), .. } => {
                return Some(interner_text(interner, *sym).to_string());
            }
            _ => {}
        }
    }
    None
}
