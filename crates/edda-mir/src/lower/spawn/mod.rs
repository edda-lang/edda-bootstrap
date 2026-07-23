//! `scope(exec)` / `group.spawn { ... }` / `.await` lowering.
//!
//! Three cooperating pieces:
//!
//! - This file — `scope(exec)` entry/exit (opens/joins the task group via
//!   ordinary `Call` terminators against the `edda-rt` extern surface),
//!   `group.spawn { ... }` (→
//!   [`crate::TerminatorKind::Spawn`]), and `<task>.await` (→
//!   [`crate::TerminatorKind::Await`]).
//! - [`body`] — lifts a spawn body into its own [`crate::Body`], mirroring
//!   `super::lower_function`'s ordinary by-value param lowering rather than
//!   the closure module's single-word env-packing ABI (a spawn body is
//!   never held as a first-class value, only ever called once by the
//!   codegen-synthesised `TaskBodyFn` shim, so it needs no fat-value
//!   convention).
//! - [`free_vars`] — the free-variable scan that finds a spawn body's
//!   implicit read-only captures (CLAUDE.md: "the body admits implicit
//!   read-only capture of enclosing bindings — only the explicit
//!   `take`-mode argument list transfers ownership").

mod body;
mod free_vars;

use std::collections::HashSet;

use edda_intern::Symbol;
use edda_span::Span;
use edda_types::{HirExpr, HirSpawn};

use crate::ids::LocalId;
use crate::operand::Operand;
use crate::place::Place;
use crate::terminator::{CallArg, CallMode, FuncRef, Terminator, TerminatorKind};
use crate::ty::{FnSig, MirPrim, MirType, ParamMode};

use super::cfg::seal_terminator;
use super::ctx::LoweringContext;
use super::expr::{lower_expr_to_operand, unsupported_and_unit};
use super::ty::lower_ty;

/// One argument threaded from a `group.spawn` call site into its lifted
/// child body's params, in the fixed order: explicit `take`-args first (in
/// source order), then implicit read-captures (in first-reference order).
struct SpawnArg {
    name: Symbol,
    ty: MirType,
    mode: ParamMode,
}

/// Lower `<group>.spawn(take a = init, ...) { body }` to a lifted
/// [`crate::Body`] plus a [`crate::TerminatorKind::Spawn`].
pub(super) fn lower_spawn(ctx: &mut LoweringContext<'_>, expr: &HirExpr, spawn: &HirSpawn) -> Operand {
    let Some(&group_local) = ctx.bindings.get(&spawn.scope_name.name) else {
        return unsupported_and_unit(ctx, expr.span, "Spawn (scope binder not in scope)");
    };

    let mut explicit_names: HashSet<Symbol> = HashSet::new();
    let mut params: Vec<SpawnArg> = Vec::with_capacity(spawn.args.len());
    let mut operands: Vec<Operand> = Vec::with_capacity(spawn.args.len());

    for a in spawn.args.iter() {
        explicit_names.insert(a.name.name);
        let op = lower_expr_to_operand(ctx, &a.init);
        if ctx.current_bb.is_none() {
            return Operand::Unit;
        }
        let ty = lower_ty(ctx.ty_interner, &ctx.adt_map, a.init.ty);
        operands.push(op);
        params.push(SpawnArg { name: a.name.name, ty, mode: ParamMode::Take });
    }

    let free_names = free_vars::collect_implicit_reads(&spawn.body, &explicit_names, &ctx.bindings);
    for name in free_names {
        let Some(&local) = ctx.bindings.get(&name) else {
            continue;
        };
        let Some(ty) = ctx
            .body
            .as_ref()
            .and_then(|b| b.body_ref().locals.get(local))
            .map(|decl| decl.ty.clone())
        else {
            continue;
        };
        operands.push(Operand::Copy(Place::local(local)));
        params.push(SpawnArg { name, ty, mode: ParamMode::Let });
    }

    let Some(child) = body::lift_spawn_body(ctx, spawn, &params) else {
        return Operand::Unit;
    };

    let Some((dest, target)) =
        super::call::allocate_dest_and_target(ctx, expr.span, MirType::prim(MirPrim::HeapPtr))
    else {
        return Operand::Unit;
    };

    seal_terminator(
        ctx,
        Terminator {
            span: expr.span,
            kind: TerminatorKind::Spawn {
                child,
                args: operands,
                group_local,
                dest,
                target,
            },
        },
    );
    ctx.current_bb = Some(target);
    Operand::Move(Place::local(dest))
}

/// Lower `<task>.await` to a [`crate::TerminatorKind::Await`].
pub(super) fn lower_await(ctx: &mut LoweringContext<'_>, expr: &HirExpr, inner: &HirExpr) -> Operand {
    let task_op = lower_expr_to_operand(ctx, inner);
    if ctx.current_bb.is_none() {
        return Operand::Unit;
    }
    let result_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, expr.ty);
    let Some((dest, target)) = super::call::allocate_dest_and_target(ctx, expr.span, result_ty)
    else {
        return Operand::Unit;
    };
    seal_terminator(
        ctx,
        Terminator {
            span: expr.span,
            kind: TerminatorKind::Await { task: task_op, dest, target },
        },
    );
    ctx.current_bb = Some(target);
    Operand::Move(Place::local(dest))
}

/// Emit a `Call` to `__edda_task_group_open() -> HeapPtr`, opening the task
/// group a `scope(exec)` region's spawns register against.
pub(super) fn open_task_group(ctx: &mut LoweringContext<'_>, span: Span) -> Option<LocalId> {
    let sig = Box::new(FnSig {
        params: Vec::new(),
        ret: MirType::prim(MirPrim::HeapPtr),
        capabilities: Vec::new(),
        may_raise: Vec::new(),
        may_panic: false,
    });
    let func_ref = FuncRef::Extern {
        name: ctx.interner.intern("__edda_task_group_open"),
        sig,
    };
    let (dest, target) =
        super::call::allocate_dest_and_target(ctx, span, MirType::prim(MirPrim::HeapPtr))?;
    seal_terminator(
        ctx,
        Terminator {
            span,
            kind: TerminatorKind::Call {
                func: func_ref,
                args: Vec::new(),
                capabilities: Vec::new(),
                destination: Place::local(dest),
                target,
                on_error: None,
            },
        },
    );
    ctx.current_bb = Some(target);
    Some(dest)
}

/// Emit a `Call` to `__edda_task_group_join(group)`, consuming
/// `group_local` at a `scope(exec)` region's exit.
pub(super) fn close_task_group(ctx: &mut LoweringContext<'_>, span: Span, group_local: LocalId) {
    let sig = Box::new(FnSig {
        params: vec![(ParamMode::Take, MirType::prim(MirPrim::HeapPtr))],
        ret: MirType::unit(),
        capabilities: Vec::new(),
        may_raise: Vec::new(),
        may_panic: false,
    });
    let func_ref = FuncRef::Extern {
        name: ctx.interner.intern("__edda_task_group_join"),
        sig,
    };
    let Some((dest, target)) = super::call::allocate_dest_and_target(ctx, span, MirType::unit())
    else {
        return;
    };
    seal_terminator(
        ctx,
        Terminator {
            span,
            kind: TerminatorKind::Call {
                func: func_ref,
                args: vec![CallArg {
                    mode: CallMode::Take,
                    operand: Operand::Move(Place::local(group_local)),
                }],
                capabilities: Vec::new(),
                destination: Place::local(dest),
                target,
                on_error: None,
            },
        },
    );
    ctx.current_bb = Some(target);
}
