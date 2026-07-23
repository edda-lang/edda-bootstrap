//! CFG-mutation helpers shared by every expression and statement dispatch in
//! the typed-HIR -> MIR lowering pass.
//!
//! These helpers always mutate the active `LoweringContext` and are
//! crate-`pub(super)` so the rest of the lowering pass can use them
//! directly without going through the consume-on-finish [`BlockBuilder`]
//! API. Every helper that seals a terminator clears `ctx.current_bb`
//! afterwards — keeping the "no statements after divergence" invariant in
//! one place rather than scattered through the dispatch.

use edda_span::Span;

use crate::ids::{BlockId, LocalId};
use crate::operand::Operand;
use crate::place::Place;
use crate::rvalue::{Rvalue, RvalueKind};
use crate::statement::{Statement, StatementKind};
use crate::terminator::{Terminator, TerminatorKind};
use crate::ty::MirType;

use super::ctx::LoweringContext;

/// Allocate a fresh block via the body builder, returning its [`BlockId`].
pub(super) fn alloc_block(ctx: &mut LoweringContext<'_>) -> Option<BlockId> {
    let body_builder = ctx.body.as_mut()?;
    let bb = body_builder.block();
    let id = bb.id();
    // BlockBuilder is `#[must_use]`; sealing with `unreachable` keeps clippy
    // happy and leaves the same placeholder a drop would have.
    let _ = bb.unreachable(Span::DUMMY);
    Some(id)
}

/// Allocate three consecutive blocks in one call.
pub(super) fn alloc_three_blocks(
    ctx: &mut LoweringContext<'_>,
) -> Option<(BlockId, BlockId, BlockId)> {
    Some((alloc_block(ctx)?, alloc_block(ctx)?, alloc_block(ctx)?))
}

/// Append `Assign { place: local, rvalue }` to the current block.
pub(super) fn push_assign(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    local: LocalId,
    rvalue: Rvalue,
) {
    push_assign_place(ctx, span, Place::local(local), rvalue);
}

/// Append `Assign { place, rvalue }` to the current block. Variant of
/// [`push_assign`] that accepts a [`Place`] carrying a projection chain;
/// used by `lower_assign` to write through `Field` / `Index` LHS shapes.
pub(super) fn push_assign_place(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    place: Place,
    rvalue: Rvalue,
) {
    let Some(bb) = ctx.current_bb else { return };
    let Some(body_builder) = ctx.body.as_mut() else { return };
    let stmt = Statement {
        span,
        kind: StatementKind::Assign { place, rvalue },
    };
    body_builder.body_mut().blocks[bb].stmts.push(stmt);
}

/// Write `op` into `dest` as an `Rvalue::Use` assignment.
pub(super) fn assign_into(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    dest: LocalId,
    op: Operand,
    ty: MirType,
) {
    let rvalue = Rvalue {
        span,
        kind: RvalueKind::Use(op),
        ty,
    };
    push_assign(ctx, span, dest, rvalue);
}

/// Seal the current block with `Goto(target)` and clear `current_bb`.
pub(super) fn goto(ctx: &mut LoweringContext<'_>, span: Span, target: BlockId) {
    seal_terminator(
        ctx,
        Terminator {
            span,
            kind: TerminatorKind::Goto(target),
        },
    );
}

/// Seal the current block with the supplied terminator and clear
/// `current_bb`.
pub(super) fn seal_terminator(ctx: &mut LoweringContext<'_>, terminator: Terminator) {
    let Some(bb) = ctx.current_bb else { return };
    let Some(body_builder) = ctx.body.as_mut() else { return };
    body_builder.body_mut().blocks[bb].terminator = terminator;
    ctx.current_bb = None;
}

/// Seal the current block with `SwitchBool(cond, on_true, on_false)`.
pub(super) fn seal_switch_bool(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    cond: Operand,
    on_true: BlockId,
    on_false: BlockId,
) {
    seal_terminator(
        ctx,
        Terminator {
            span,
            kind: TerminatorKind::SwitchBool {
                cond,
                true_bb: on_true,
                false_bb: on_false,
            },
        },
    );
}
