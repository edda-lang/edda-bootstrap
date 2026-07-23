//! Body-cost accumulator for §5.4 graded-bound discharge.
//!
//! Walks a function body's blocks / statements / expressions and folds
//! their per-kind cost into one [`Predicate`]: straight-line code sums,
//! branches (`if` / `match`) take the max, and `for` loops lift to
//! `iter_count * body_cost`.

use edda_refine::Predicate;
use edda_syntax::ast::{Block, Expr, ExprKind, MatchArm, Stmt, StmtKind};

use super::call_cost::cost_of_call;
use super::lia::{add_costs, for_iter_count, mul_predicate, predicate_max, zero};
use super::GradedCx;

/// Sum cost across a block's statements and trailing expression.
pub(super) fn cost_of_block(block: &Block, cx: &GradedCx<'_>) -> Option<Predicate> {
    let mut acc = zero(cx.sort);
    for stmt in &block.stmts {
        acc = add_costs(acc,cost_of_stmt(stmt, cx)?);
    }
    if let Some(trailing) = &block.trailing {
        acc = add_costs(acc,cost_of_expr(trailing, cx)?);
    }
    Some(acc)
}

/// Cost of one statement — sum of any nested expression costs.
fn cost_of_stmt(stmt: &Stmt, cx: &GradedCx<'_>) -> Option<Predicate> {
    match &stmt.kind {
        StmtKind::Let { init, .. } => match init {
            Some(e) => cost_of_expr(e, cx),
            None => Some(zero(cx.sort)),
        },
        StmtKind::Assign { target, rhs, .. } => {
            let l = cost_of_expr(target, cx)?;
            let r = cost_of_expr(rhs, cx)?;
            Some(add_costs(l, r))
        }
        StmtKind::Expr(e) => cost_of_expr(e, cx),
    }
}

/// Cost of one expression — recursive over `ExprKind`.
///
/// Branch composition (`If` / `Match`) applies the branch-max rule via
/// [`predicate_max`]. Loop composition (`For`) applies loop-lift via
/// [`mul_predicate`]. Calls credit the callee's graded bound for this
/// kind, substituting callee params with call-site args. Everything else
/// (path / literal / arithmetic / field / index / cast / scope / try /
/// raise / panic / await / etc.) recursively sums sub-expressions —
/// nothing in those forms credits cost on its own; the cost is whatever
/// their sub-expressions accumulate.
pub(super) fn cost_of_expr(expr: &Expr, cx: &GradedCx<'_>) -> Option<Predicate> {
    match &expr.kind {
        ExprKind::Call { callee, args } => cost_of_call(callee, args, cx),
        ExprKind::If {
            cond,
            then_block,
            else_branch,
        } => {
            let c = cost_of_expr(cond, cx)?;
            let t = cost_of_block(then_block, cx)?;
            let e = match else_branch {
                Some(eb) => cost_of_expr(eb, cx)?,
                None => zero(cx.sort),
            };
            Some(add_costs(c, predicate_max(t, e)))
        }
        ExprKind::Match { scrutinee, arms } => {
            let s = cost_of_expr(scrutinee, cx)?;
            let arms_max = cost_of_arms(arms, cx)?;
            Some(add_costs(s, arms_max))
        }
        ExprKind::For { iter, body, .. } => cost_of_for(iter, body, cx),
        ExprKind::Loop { body, .. } => {
            // Unbounded loop: body cost must be 0 for the obligation to
            // hold. Represent the loop's cost as the body cost
            // unchanged — Z3 sees `caller_bound >= body_cost` and finds
            // sat exactly when the body has nonzero cost for this kind.
            // A literal zero body cost discharges trivially.
            cost_of_block(body, cx)
        }
        ExprKind::Block(b) => cost_of_block(b, cx),
        ExprKind::Binary { lhs, rhs, .. } => {
            let l = cost_of_expr(lhs, cx)?;
            let r = cost_of_expr(rhs, cx)?;
            Some(add_costs(l, r))
        }
        ExprKind::Unary { expr: inner, .. } => cost_of_expr(inner, cx),
        ExprKind::Field { receiver, .. } => cost_of_expr(receiver, cx),
        ExprKind::TupleIndex { receiver, .. } => cost_of_expr(receiver, cx),
        ExprKind::CompField { receiver, index } => {
            let l = cost_of_expr(receiver, cx)?;
            let r = cost_of_expr(index, cx)?;
            Some(add_costs(l, r))
        }
        ExprKind::Index { receiver, index } => {
            let l = cost_of_expr(receiver, cx)?;
            let r = cost_of_expr(index, cx)?;
            Some(add_costs(l, r))
        }
        ExprKind::Cast { expr: inner, .. } => cost_of_expr(inner, cx),
        ExprKind::Range { lo, hi, .. } => {
            let lc = match lo {
                Some(e) => cost_of_expr(e, cx)?,
                None => zero(cx.sort),
            };
            let hc = match hi {
                Some(e) => cost_of_expr(e, cx)?,
                None => zero(cx.sort),
            };
            Some(add_costs(lc, hc))
        }
        ExprKind::Tuple(elems) => sum_costs(elems.iter(), cx),
        ExprKind::Array(elems) => sum_costs(elems.iter(), cx),
        ExprKind::StructLit { fields, .. } => sum_costs(fields.iter().map(|f| &f.value), cx),
        ExprKind::FString(parts) => sum_costs(
            parts.iter().filter_map(|p| match p {
                edda_syntax::ast::FStringPart::Slot(e) => Some(e.as_ref()),
                edda_syntax::ast::FStringPart::Text(_) => None,
            }),
            cx,
        ),
        ExprKind::Try(inner)
        | ExprKind::Await(inner)
        | ExprKind::Raise(inner)
        | ExprKind::Panic(inner)
        | ExprKind::Comptime(inner) => cost_of_expr(inner, cx),
        ExprKind::ComptimeBlock(b) => cost_of_block(b, cx),
        ExprKind::Scope { body, .. } => cost_of_block(body, cx),
        ExprKind::Return(opt) => match opt {
            Some(e) => cost_of_expr(e, cx),
            None => Some(zero(cx.sort)),
        },
        ExprKind::Break { value, .. } => match value {
            Some(e) => cost_of_expr(e, cx),
            None => Some(zero(cx.sort)),
        },
        ExprKind::Continue { .. } => Some(zero(cx.sort)),
        ExprKind::Handle { body, recovery, .. } => {
            let b = cost_of_block(body, cx)?;
            let r = cost_of_expr(recovery, cx)?;
            Some(add_costs(b, r))
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            // Phase B does not see method calls (they re-resolve to free
            // functions during typecheck and reach this pass as Call).
            // Conservative: sum the operand sub-costs.
            let mut acc = cost_of_expr(receiver, cx)?;
            for a in args {
                acc = add_costs(acc,cost_of_expr(&a.expr, cx)?);
            }
            Some(acc)
        }
        // Forms outside the predicate fragment collapse to bail-None so
        // the whole kind's discharge admits silently (matching
        // `discharge_fn_clauses` for unsupported clauses).
        ExprKind::Closure(_)
        | ExprKind::Spawn(_)
        | ExprKind::EffectRow(_)
        | ExprKind::Error => None,
        // Pure value forms have no cost.
        ExprKind::Literal(_) | ExprKind::Path(_) => Some(zero(cx.sort)),
        // Bounded quantifiers don't appear in expression cost positions —
        // they're refinement-only forms. Bail-None matches the other
        // refinement-only / unsupported branches.
        ExprKind::Forall { .. } | ExprKind::Exists { .. } => None,
    }
}

/// Cost contribution of a `for x in <iter> { body }` loop. Combines iter-
/// expression sub-cost, body-per-iteration cost, and the loop-lift `n * c`.
fn cost_of_for(iter: &Expr, body: &Block, cx: &GradedCx<'_>) -> Option<Predicate> {
    let iter_cost = cost_of_expr(iter, cx)?;
    let body_cost = cost_of_block(body, cx)?;
    let count = for_iter_count(iter, cx)?;
    let lifted = mul_predicate(count, body_cost)?;
    Some(add_costs(iter_cost, lifted))
}

/// Max-fold over `match` arms (and any guard sub-costs).
fn cost_of_arms(arms: &[MatchArm], cx: &GradedCx<'_>) -> Option<Predicate> {
    let mut iter = arms.iter();
    let first = iter.next()?;
    let mut acc = cost_of_arm(first, cx)?;
    for arm in iter {
        let next = cost_of_arm(arm, cx)?;
        acc = predicate_max(acc, next);
    }
    Some(acc)
}

/// Cost of one match arm: guard cost (when present) + body cost.
fn cost_of_arm(arm: &MatchArm, cx: &GradedCx<'_>) -> Option<Predicate> {
    let guard_cost = match &arm.guard {
        Some(g) => cost_of_expr(g, cx)?,
        None => zero(cx.sort),
    };
    let body_cost = cost_of_expr(&arm.body, cx)?;
    Some(add_costs(guard_cost, body_cost))
}

/// Sum costs across an iterator of expression references.
fn sum_costs<'a, I: IntoIterator<Item = &'a Expr>>(
    iter: I,
    cx: &GradedCx<'_>,
) -> Option<Predicate> {
    let mut acc = zero(cx.sort);
    for e in iter {
        acc = add_costs(acc,cost_of_expr(e, cx)?);
    }
    Some(acc)
}
