//! Lexical-scope tracking helpers for the typed-HIR -> MIR lowering pass.
//!
//! Every block entry (function body, `HirBlock` expression body) wraps its
//! statement walk in a matching `enter_scope` / `exit_scope` pair so
//! user-let bindings emit `StorageLive` / `StorageDead` over the right
//! range. These helpers are crate-`pub(super)` to mirror the rest of the
//! lowering pass.

use edda_span::Span;

use crate::ids::LocalId;
use crate::statement::{Statement, StatementKind};

use super::ctx::LoweringContext;

/// Push a fresh lexical-scope frame onto `ctx.scope_stack`. Every block
/// entry (function body, `HirBlock` expression body) wraps its statement
/// walk in a matching `enter_scope` / `exit_scope` pair so user-let
/// bindings emit `StorageLive` / `StorageDead` over the right range.
pub(super) fn enter_scope(ctx: &mut LoweringContext<'_>) {
    ctx.scope_stack.push(Vec::new());
}

/// Pop the top scope frame and emit `StorageDead` for every local it
/// recorded, in reverse declaration order. If the current block is `None`
/// (the scope ended in a diverging terminator), the locals are dropped
/// without emitting — the analyses that consume `StorageDead` accept
/// either a matching pair or no `StorageDead` at all after divergence.
pub(super) fn exit_scope(ctx: &mut LoweringContext<'_>, span: Span) {
    let Some(frame) = ctx.scope_stack.pop() else {
        return;
    };
    if ctx.current_bb.is_none() {
        return;
    }
    for local in frame.into_iter().rev() {
        push_storage_dead(ctx, span, local);
    }
}

/// Append `StorageLive(local)` to the current block and register the local
/// in the top scope frame so its matching `StorageDead` is emitted on
/// scope exit. Called by `lower_let` for every user-let binding.
pub(super) fn push_storage_live(ctx: &mut LoweringContext<'_>, span: Span, local: LocalId) {
    push_storage_stmt(ctx, span, local, true);
    if let Some(frame) = ctx.scope_stack.last_mut() {
        frame.push(local);
    }
}

/// Append `StorageDead(local)` to the current block. Used by `exit_scope`;
/// kept separate so the storage-live helper can share the underlying
/// builder access.
fn push_storage_dead(ctx: &mut LoweringContext<'_>, span: Span, local: LocalId) {
    push_storage_stmt(ctx, span, local, false);
}

/// Append a `StorageLive` (when `live`) or `StorageDead` (when `!live`)
/// statement to the current block. Silently drops the statement when there
/// is no active block — matches the existing `push_assign` failure mode.
fn push_storage_stmt(ctx: &mut LoweringContext<'_>, span: Span, local: LocalId, live: bool) {
    let Some(bb) = ctx.current_bb else { return };
    let Some(body_builder) = ctx.body.as_mut() else { return };
    let kind = if live {
        StatementKind::StorageLive(local)
    } else {
        StatementKind::StorageDead(local)
    };
    body_builder.body_mut().blocks[bb]
        .stmts
        .push(Statement { span, kind });
}
