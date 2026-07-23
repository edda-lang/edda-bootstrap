//! Free-variable scan for a `group.spawn { body }`'s implicit read-only
//! captures.
//!
//! Unlike a closure literal (mandatory `captures { ... }` clause), a spawn
//! body admits implicit read-only capture of any enclosing binding — only
//! the explicit `take`-mode argument list transfers ownership
//! (`edda_types::HirSpawn` doc comment). MIR lowering re-lifts the body
//! against a fresh [`super::super::ctx::LoweringContext`] whose `bindings`
//! map starts empty, so every implicitly-read outer name must be
//! discovered up front and threaded in as an extra by-value argument
//! before the body is built.

use std::collections::{HashMap, HashSet};

use edda_intern::Symbol;
use edda_types::{
    HirBlock, HirExpr, HirExprKind, HirFStringPart, HirPat, HirPatKind, HirStmt, HirStmtKind,
    HirVariantPatPayload,
};

use crate::ids::LocalId;

//   name shadowing introduced by a `let`/pattern binding *inside* the spawn
//   body, so a name that is both free in an outer scope and re-bound deeper
//   in the body may be reported as an implicit capture it does not actually
//   need. That is always safe (an unused extra by-value argument), never
//   unsound (a missing capture instead fails loudly as `UnknownBinding`
//   when the fresh body's `lower_path` cannot resolve it)
//   key in `parent_bindings` — the enclosing function's per-body local/
//   param/capability map — so references to top-level functions, module
//   consts, and other module-qualified paths (which resolve without any
//   per-invocation capture) are never treated as captures
/// Collect every name the spawn `body` reads that (a) is not one of the
/// spawn's own explicit `take`-arg names and (b) is bound in the enclosing
/// function's `parent_bindings` — the implicit read-only captures the
/// lifted spawn body needs as extra by-value parameters. Returned in
/// first-reference order.
pub(super) fn collect_implicit_reads(
    body: &HirBlock,
    explicit: &HashSet<Symbol>,
    parent_bindings: &HashMap<Symbol, LocalId>,
) -> Vec<Symbol> {
    let mut seen = HashSet::new();
    let mut order = Vec::new();
    walk_block(body, &mut seen, &mut order);
    order
        .into_iter()
        .filter(|name| !explicit.contains(name) && parent_bindings.contains_key(name))
        .collect()
}

fn note(name: Symbol, seen: &mut HashSet<Symbol>, order: &mut Vec<Symbol>) {
    if seen.insert(name) {
        order.push(name);
    }
}

fn walk_block(block: &HirBlock, seen: &mut HashSet<Symbol>, order: &mut Vec<Symbol>) {
    for stmt in block.stmts.iter() {
        walk_stmt(stmt, seen, order);
    }
    if let Some(trailing) = block.trailing.as_deref() {
        walk_expr(trailing, seen, order);
    }
}

fn walk_stmt(stmt: &HirStmt, seen: &mut HashSet<Symbol>, order: &mut Vec<Symbol>) {
    match &stmt.kind {
        HirStmtKind::Let { pat, init, .. } => {
            walk_pat(pat, seen, order);
            if let Some(e) = init {
                walk_expr(e, seen, order);
            }
        }
        HirStmtKind::Assign { target, rhs, .. } => {
            walk_expr(target, seen, order);
            walk_expr(rhs, seen, order);
        }
        HirStmtKind::Expr(e) => walk_expr(e, seen, order),
    }
}

fn walk_pat(pat: &HirPat, seen: &mut HashSet<Symbol>, order: &mut Vec<Symbol>) {
    match &pat.kind {
        HirPatKind::Wildcard
        | HirPatKind::Binding(_)
        | HirPatKind::Literal(_)
        | HirPatKind::Range { .. }
        | HirPatKind::Error => {}
        HirPatKind::Tuple(pats) => {
            for p in pats.iter() {
                walk_pat(p, seen, order);
            }
        }
        HirPatKind::Variant { payload, .. } => match payload {
            HirVariantPatPayload::None => {}
            HirVariantPatPayload::Tuple(pats) => {
                for p in pats.iter() {
                    walk_pat(p, seen, order);
                }
            }
            HirVariantPatPayload::Struct(fields) => {
                for f in fields.iter() {
                    walk_pat(&f.pat, seen, order);
                }
            }
        },
        HirPatKind::Struct { fields, .. } => {
            for f in fields.iter() {
                walk_pat(&f.pat, seen, order);
            }
        }
        HirPatKind::Guard { pat: inner, cond } => {
            walk_pat(inner, seen, order);
            walk_expr(cond, seen, order);
        }
        HirPatKind::AtBinding { inner, .. } => walk_pat(inner, seen, order),
        HirPatKind::Slice { prefix, suffix, .. } => {
            for p in prefix.iter() {
                walk_pat(p, seen, order);
            }
            for p in suffix.iter() {
                walk_pat(p, seen, order);
            }
        }
    }
}

fn walk_expr(expr: &HirExpr, seen: &mut HashSet<Symbol>, order: &mut Vec<Symbol>) {
    match &expr.kind {
        HirExprKind::Literal(_) | HirExprKind::Continue { .. } | HirExprKind::EffectRow(_) => {}
        HirExprKind::Error => {}
        HirExprKind::FString(parts) => {
            for part in parts {
                if let HirFStringPart::Slot(e) = part {
                    walk_expr(e, seen, order);
                }
            }
        }
        HirExprKind::Path(path) => {
            if let [seg] = path.segments.as_ref() {
                note(seg.name, seen, order);
            }
        }
        HirExprKind::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, seen, order);
            walk_expr(rhs, seen, order);
        }
        HirExprKind::Unary { expr: inner, .. } => walk_expr(inner, seen, order),
        HirExprKind::Call { callee, args } => {
            walk_expr(callee, seen, order);
            for a in args.iter() {
                walk_expr(&a.expr, seen, order);
            }
        }
        HirExprKind::MethodCall { receiver, args, .. } => {
            walk_expr(receiver, seen, order);
            for a in args.iter() {
                walk_expr(&a.expr, seen, order);
            }
        }
        HirExprKind::Field { receiver, .. } => walk_expr(receiver, seen, order),
        HirExprKind::TupleIndex { receiver, .. } => walk_expr(receiver, seen, order),
        HirExprKind::Index { receiver, index } => {
            walk_expr(receiver, seen, order);
            walk_expr(index, seen, order);
        }
        HirExprKind::If { cond, then_block, else_branch } => {
            walk_expr(cond, seen, order);
            walk_block(then_block, seen, order);
            if let Some(e) = else_branch.as_deref() {
                walk_expr(e, seen, order);
            }
        }
        HirExprKind::Match { scrutinee, arms } => {
            walk_expr(scrutinee, seen, order);
            for arm in arms.iter() {
                walk_pat(&arm.pat, seen, order);
                if let Some(g) = &arm.guard {
                    walk_expr(g, seen, order);
                }
                walk_expr(&arm.body, seen, order);
            }
        }
        HirExprKind::Block(block) => walk_block(block, seen, order),
        HirExprKind::Cast { expr: inner, .. } => walk_expr(inner, seen, order),
        HirExprKind::Range { lo, hi, .. } => {
            if let Some(e) = lo.as_deref() {
                walk_expr(e, seen, order);
            }
            if let Some(e) = hi.as_deref() {
                walk_expr(e, seen, order);
            }
        }
        HirExprKind::Tuple(elems) | HirExprKind::Array(elems) => {
            for e in elems.iter() {
                walk_expr(e, seen, order);
            }
        }
        HirExprKind::StructLit { fields, .. } => {
            for f in fields.iter() {
                walk_expr(&f.value, seen, order);
            }
        }
        HirExprKind::Loop { body, .. } => walk_block(body, seen, order),
        HirExprKind::For { iter, body, .. } => {
            walk_expr(iter, seen, order);
            walk_block(body, seen, order);
        }
        HirExprKind::Try(inner)
        | HirExprKind::Await(inner)
        | HirExprKind::Raise(inner)
        | HirExprKind::Panic(inner)
        | HirExprKind::Comptime(inner) => walk_expr(inner, seen, order),
        HirExprKind::ComptimeBlock(block) => walk_block(block, seen, order),
        HirExprKind::Scope { body, .. } => walk_block(body, seen, order),
        HirExprKind::Return(v) => {
            if let Some(e) = v.as_deref() {
                walk_expr(e, seen, order);
            }
        }
        HirExprKind::Break { value, .. } => {
            if let Some(e) = value.as_deref() {
                walk_expr(e, seen, order);
            }
        }
        HirExprKind::Handle { recovery, body, .. } => {
            walk_expr(recovery, seen, order);
            walk_block(body, seen, order);
        }
        HirExprKind::Forall { iter, body, .. } | HirExprKind::Exists { iter, body, .. } => {
            walk_expr(iter, seen, order);
            walk_expr(body, seen, order);
        }
        // A nested closure's own body is governed by its mandatory
        // `captures { ... }` clause, not this scan — but each capture name
        // is itself a read of the (here: spawn body's) enclosing scope.
        HirExprKind::Closure(closure) => {
            for cap in closure.captures.iter() {
                note(cap.name.name, seen, order);
            }
        }
        // A nested spawn's explicit take-arg initialisers are evaluated in
        // *this* scope, so they can reference names this walk must find.
        // The nested spawn's own body is scanned independently, when it is
        // itself lowered.
        HirExprKind::Spawn(spawn) => {
            for a in spawn.args.iter() {
                walk_expr(&a.init, seen, order);
            }
        }
    }
}
