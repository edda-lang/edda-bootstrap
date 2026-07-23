//! Diverging control-flow exits — `return` / `break` / `continue` /
//! `raise` / `panic`.
//!
//! All synthesise `never`. `raise` / `panic` are *originators* that
//! additionally contribute an entry to the effect-row accumulator.

use crate::effect::{EffectEntry, PureEffect};
use crate::hir::{HirBlock, HirExpr, HirExprKind, HirStmtKind};
use crate::prim::Primitive;
use crate::ty::TyId;

use super::super::{InferCx, TyEnv, synth_expr};

/// Whether `expr` always exits the enclosing function — every control
/// path through it ends in `return`, `raise`, or `panic`.
///
/// Branch joins use this to decide whether an arm can reach the join
/// at all: an arm that always exits the function contributes no
/// binding states to the join, so folding its (pre-arm) states in by
/// GLB would spuriously poison the surviving arm's initialisations
/// (`uninit lir` initialised in a
/// `handle` body whose recovery arm is `raise …`).
///
/// `break` / `continue` deliberately return `false`: they do leave the
/// *branch*, but control re-enters the function at an enclosing loop
/// boundary, and their arm's binding states must still reach that loop
/// join through the ordinary GLB merge. Divergence-typed forms the
/// walk does not recognise (an infinite `loop`, a call to a
/// `-> never` function) also return `false` — the caller then stays on
/// the conservative merge path, which can reject valid code but never
/// accepts invalid code.
pub(crate) fn expr_always_exits(expr: &HirExpr) -> bool {
    match &expr.kind {
        HirExprKind::Raise(_) | HirExprKind::Panic(_) | HirExprKind::Return(_) => true,
        HirExprKind::Try(inner) => expr_always_exits(inner),
        HirExprKind::Block(block) => block_always_exits(block),
        HirExprKind::If {
            cond,
            then_block,
            else_branch,
        } => {
            expr_always_exits(cond)
                || else_branch.as_ref().is_some_and(|e| {
                    block_always_exits(then_block) && expr_always_exits(e)
                })
        }
        HirExprKind::Match { scrutinee, arms } => {
            expr_always_exits(scrutinee)
                || (!arms.is_empty() && arms.iter().all(|arm| expr_always_exits(&arm.body)))
        }
        _ => false,
    }
}

/// Block form of [`expr_always_exits`] — `true` when any statement
/// always exits the function (everything after it is unreachable) or
/// the trailing expression always exits.
pub(crate) fn block_always_exits(block: &HirBlock) -> bool {
    let stmt_exits = block.stmts.iter().any(|stmt| match &stmt.kind {
        HirStmtKind::Expr(e) => expr_always_exits(e),
        HirStmtKind::Let { init, .. } => init.as_ref().is_some_and(expr_always_exits),
        HirStmtKind::Assign { rhs, .. } => expr_always_exits(rhs),
    });
    stmt_exits || block.trailing.as_deref().is_some_and(expr_always_exits)
}

/// Synthesise a diverging expression — `return`, `break`, `continue`.
/// The optional payload is synthesised for its side-effects (so any
/// internal type errors surface) but the result is always `never`.
///
/// `raise` and `panic` are *originators* that contribute to the effect
/// row; they have their own dispatchers ([`synth_raise`], [`synth_panic`])
/// and do not route through here.
pub(crate) fn synth_divergent(
    env: &mut TyEnv,
    payload: Option<&mut HirExpr>,
    ic: &mut InferCx<'_>,
) -> TyId {
    if let Some(e) = payload {
        let _ = synth_expr(env, e, ic);
    }
    ic.lower.ty_interner.prim(Primitive::Never)
}

/// Synthesise `return <expr>` — the payload is type-checked against
/// the enclosing function's return type when known, so integer
/// literals narrow from their default `i64` to the declared width.
///
/// Without this narrowing, MIR lowering produces a `Const` whose type
/// is `i64`, the LLVM emitter issues `store i64 0, ptr <i32-alloca>`,
/// and the resulting 8-byte store overruns the slot. On Linux x86-64
/// the SysV red zone absorbs the overrun harmlessly; on Windows
/// x86-64 (no red zone) it clobbers the return address sitting at
/// `[rsp+8]`, and the program access-violates on `ret`.
///
/// The result is always [`Primitive::Never`] — a `return` does not
/// produce a value to its surrounding expression.
pub(crate) fn synth_return(
    env: &mut TyEnv,
    payload: Option<&mut HirExpr>,
    ic: &mut InferCx<'_>,
) -> TyId {
    if let Some(e) = payload {
        let expected = ic.fn_return_ty;
        if expected == ic.lower.ty_interner.error() {
            // No function context (test-mode InferCx, or inference
            // ran outside `check_fn_body`). Fall back to synthesis so
            // the payload's sub-expressions still get their `ty`
            // fields populated for diagnostics.
            let _ = synth_expr(env, e, ic);
        } else {
            super::super::check_expr(env, e, expected, ic);
        }
        // `return e` moves `e` to the caller — consume a `linear`
        // binding handed out this way so the scope-exit sweep does not
        // read it as a leak.
        super::super::mode::consume_moved_linear(env, e, ic);
    }
    ic.lower.ty_interner.prim(Primitive::Never)
}

/// Synthesise `raise <expr>`. Synthesises the payload (so sub-expression
/// type errors surface), pushes an `Err(payload.ty)` entry into the
/// accumulator per `effect-tracking.md §4`, and returns `never`.
///
/// Whether the resulting `err: T` entry is *admissible* against the
/// enclosing function's declared row is enforced at function-body
/// exit by [`super::super::check_fn_body`] — this site simply records the
/// contribution.
pub(crate) fn synth_raise(
    env: &mut TyEnv,
    payload: &mut HirExpr,
    ic: &mut InferCx<'_>,
) -> TyId {
    let payload_ty = synth_expr(env, payload, ic);
    if payload_ty != ic.ty_error() {
        ic.push_effect_entry(EffectEntry::Pure(PureEffect::Err(payload_ty)));
    }
    ic.lower.ty_interner.prim(Primitive::Never)
}

/// Synthesise `panic <expr>`. Synthesises the payload for its
/// side-effects, pushes the bare `Panic` entry, and returns `never`.
/// Per `effect-tracking.md §4`, the message expression is operational
/// metadata routed to the runtime — not a typed payload — so the
/// payload's type does not appear in the row entry.
pub(crate) fn synth_panic(
    env: &mut TyEnv,
    payload: &mut HirExpr,
    ic: &mut InferCx<'_>,
) -> TyId {
    let _ = synth_expr(env, payload, ic);
    ic.push_effect_entry(EffectEntry::Pure(PureEffect::Panic));
    ic.lower.ty_interner.prim(Primitive::Never)
}
