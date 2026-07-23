//! Spawn-body MIR construction — the lifted [`crate::Body`] a
//! `group.spawn { ... }` block lowers to.
//!
//! Unlike a closure literal's single-word env-packing ABI, a spawn body's
//! params are ordinary by-value arguments — every explicit `take`-arg and
//! implicit read-capture, in [`super::SpawnArg`] order — mirroring how an
//! ordinary named function's parameters lower (`super::super::lower_function`
//! / `super::super::lower_user_params`), not the closure fat-value ABI. The
//! raw `(*const u8, usize) -> *mut u8` `TaskBodyFn` marshaling `edda-rt`
//! expects is a codegen-time concern; this body's own params
//! and return type stay ordinarily typed.

use edda_types::HirSpawn;

use crate::ids::{BodyId, LocalId};
use crate::operand::Operand;
use crate::place::Place;
use crate::terminator::{Terminator, TerminatorKind};
use crate::ty::MirTypeKind;

use super::super::cfg::{alloc_block, assign_into, seal_terminator};
use super::super::ctx::LoweringContext;
use super::super::stmt::register_local_capability;
use super::super::ty::lower_ty;
use super::SpawnArg;

/// Save the enclosing function's per-body state, build the spawn's body
/// against fresh state, then restore. Returns the pushed body's
/// [`BodyId`].
pub(super) fn lift_spawn_body(
    ctx: &mut LoweringContext<'_>,
    spawn: &HirSpawn,
    params: &[SpawnArg],
) -> Option<BodyId> {
    let saved_body = ctx.body.take();
    let saved_bindings = std::mem::take(&mut ctx.bindings);
    let saved_capabilities = std::mem::take(&mut ctx.capabilities);
    let saved_capability_aliases = std::mem::take(&mut ctx.capability_aliases);
    let saved_loop_stack = std::mem::take(&mut ctx.loop_stack);
    let saved_handler_stack = std::mem::take(&mut ctx.handler_stack);
    let saved_current_bb = ctx.current_bb.take();
    let saved_return_slot = ctx.return_slot.take();
    let saved_scope_stack = std::mem::take(&mut ctx.scope_stack);
    let saved_result_adt = ctx.result_adt.take();

    let body_id = build_spawn_body(ctx, spawn, params);

    ctx.body = saved_body;
    ctx.bindings = saved_bindings;
    ctx.capabilities = saved_capabilities;
    ctx.capability_aliases = saved_capability_aliases;
    ctx.loop_stack = saved_loop_stack;
    ctx.handler_stack = saved_handler_stack;
    ctx.current_bb = saved_current_bb;
    ctx.return_slot = saved_return_slot;
    ctx.scope_stack = saved_scope_stack;
    ctx.result_adt = saved_result_adt;

    body_id
}

fn build_spawn_body(
    ctx: &mut LoweringContext<'_>,
    spawn: &HirSpawn,
    params: &[SpawnArg],
) -> Option<BodyId> {
    let return_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, spawn.body.ty);
    let name = ctx.interner.intern("<spawn>");
    let builder = crate::builder::BodyBuilder::new(name, spawn.span, return_ty.clone());
    ctx.body = Some(builder);
    let return_slot = match ctx.body.as_mut() {
        Some(b) => b.return_slot(return_ty, spawn.span),
        None => return None,
    };
    ctx.return_slot = Some(return_slot);

    for arg in params {
        let local = match ctx.body.as_mut() {
            Some(b) => b.param(arg.mode, arg.ty.clone(), spawn.span),
            None => return None,
        };
        ctx.bindings.insert(arg.name, local);
        if let MirTypeKind::Capability(kind) = &arg.ty.kind {
            register_local_capability(ctx, arg.name, local, kind.clone());
        }
    }

    let entry = alloc_block(ctx)?;
    if let Some(builder) = ctx.body.as_mut() {
        builder.set_entry(entry);
    }
    ctx.current_bb = Some(entry);

    lower_spawn_body_block(ctx, spawn, return_slot);

    if ctx.current_bb.is_some() {
        seal_terminator(
            ctx,
            Terminator {
                span: spawn.span,
                kind: TerminatorKind::Return(Operand::Move(Place::local(return_slot))),
            },
        );
    }

    let builder = ctx.body.take()?;
    let body = builder.finish();
    Some(ctx.program.push_body(body))
}

/// Walk the spawn body's statements then its trailing expression into the
/// return slot. Mirrors `super::super::lower_function_body` minus the
/// raising-function Ok-wrap — see `build_spawn_body`'s invariant.
fn lower_spawn_body_block(ctx: &mut LoweringContext<'_>, spawn: &HirSpawn, return_slot: LocalId) {
    super::super::scope::enter_scope(ctx);
    for stmt in spawn.body.stmts.iter() {
        super::super::stmt::lower_stmt(ctx, stmt);
        if ctx.current_bb.is_none() {
            super::super::scope::exit_scope(ctx, spawn.body.span);
            return;
        }
    }
    if let Some(trailing) = spawn.body.trailing.as_deref() {
        let op = super::super::expr::lower_expr_to_operand(ctx, trailing);
        let trailing_is_never = matches!(
            ctx.ty_interner.kind(trailing.ty),
            edda_types::TyKind::Primitive(edda_types::Primitive::Never)
        );
        if !trailing_is_never {
            let return_ty = match ctx.body.as_ref() {
                Some(b) => b.body_ref().return_ty.clone(),
                None => {
                    super::super::scope::exit_scope(ctx, spawn.body.span);
                    return;
                }
            };
            assign_into(ctx, trailing.span, return_slot, op, return_ty);
        }
    }
    super::super::scope::exit_scope(ctx, spawn.body.span);
}
