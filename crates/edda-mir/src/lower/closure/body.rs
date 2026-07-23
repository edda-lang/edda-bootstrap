//! Synthesised closure-body construction for the fat fn-value lowering.
//!
//! Builds the fresh MIR [`crate::Body`] a closure literal lowers to:
//! capability slots, the leading env word, the captured-value unpack, and
//! the body-statement walk with its raising-closure Ok-wrap. Split out of
//! `closure/mod.rs` (the value-construction + result-ADT side) to keep
//! both files under the 500-line cap.

use edda_intern::Symbol;
use edda_types::HirClosure;

use super::super::cfg::{alloc_block, assign_into, push_assign, seal_terminator};
use super::super::ctx::LoweringContext;
use super::super::ty::lower_ty;
use crate::arena::Idx;
use crate::ids::{AdtId, BodyId, LocalId, VariantIdx};
use crate::operand::Operand;
use crate::place::Place;
use crate::rvalue::{Rvalue, RvalueKind};
use crate::terminator::{Terminator, TerminatorKind};
use crate::ty::{MirPrim, MirType, MirTypeKind, ParamMode};

/// Save the enclosing function's per-body state, build the closure's
/// body against fresh state, then restore. Returns the pushed body's
/// [`BodyId`]. `capture` carries the single capture's `(name, type,
/// prim)` so the body can unpack it from the env word.
pub(super) fn synthesise_closure_body(
    ctx: &mut LoweringContext<'_>,
    closure: &HirClosure,
    capture: Option<(Symbol, MirType, MirPrim)>,
    result_adt: Option<AdtId>,
) -> Option<BodyId> {
    let saved_body = ctx.body.take();
    let saved_bindings = std::mem::take(&mut ctx.bindings);
    let saved_capabilities = std::mem::take(&mut ctx.capabilities);
    let saved_loop_stack = std::mem::take(&mut ctx.loop_stack);
    let saved_handler_stack = std::mem::take(&mut ctx.handler_stack);
    let saved_current_bb = ctx.current_bb.take();
    let saved_return_slot = ctx.return_slot.take();
    let saved_scope_stack = std::mem::take(&mut ctx.scope_stack);
    let saved_result_adt = ctx.result_adt.take();

    let body_id = build_closure_body(ctx, closure, capture, result_adt);

    ctx.body = saved_body;
    ctx.bindings = saved_bindings;
    ctx.capabilities = saved_capabilities;
    ctx.loop_stack = saved_loop_stack;
    ctx.handler_stack = saved_handler_stack;
    ctx.current_bb = saved_current_bb;
    ctx.return_slot = saved_return_slot;
    ctx.scope_stack = saved_scope_stack;
    ctx.result_adt = saved_result_adt;

    body_id
}

/// Build the synthesised fat MIR body for `closure` and push it onto
/// the program. Runs against the fresh (saved-out) per-body state set
/// up by [`synthesise_closure_body`]. `result_adt` is `Some` for a
/// raising closure — the wire-level `Result<T, E>` ADT minted by
/// [`closure_result_adt`], used as the body's return type.
fn build_closure_body(
    ctx: &mut LoweringContext<'_>,
    closure: &HirClosure,
    capture: Option<(Symbol, MirType, MirPrim)>,
    result_adt: Option<AdtId>,
) -> Option<BodyId> {
    // Raising closures return the wire-level `Result<T, E>` ADT; the Ok
    // variant is index 0 (matching `synthesize_result_adt`'s layout), so
    // `ctx.result_adt` drives the trailing-value Ok-wrap below and the
    // shared `return` / `?` paths.
    let (return_ty, ok_variant) = match result_adt {
        Some(result_adt_id) => (
            MirType::new(MirTypeKind::Adt(result_adt_id)),
            Some((result_adt_id, VariantIdx::new(0))),
        ),
        None => (lower_ty(ctx.ty_interner, &ctx.adt_map, closure.ret_ty), None),
    };
    let name = ctx.interner.intern("<closure>");
    let builder = crate::builder::BodyBuilder::new(name, closure.span, return_ty.clone());
    ctx.body = Some(builder);
    ctx.result_adt = ok_variant;
    let return_slot = match ctx.body.as_mut() {
        Some(b) => b.return_slot(return_ty, closure.span),
        None => return None,
    };
    ctx.return_slot = Some(return_slot);

    // Capability slots first (before the env word): `lower_effect_row`
    // allocates one leading capability param per `with`-clause capability
    // and seeds `ctx.capabilities` so calls inside the body thread them.
    // A `panic` effect sets the body's `has_panic` bit (no call-site
    // work). The empty param slice signals "no declaring params" — a
    // closure's capabilities are captured (not typed params), so they
    // classify by name, not narrowed type.
    let has_panic = super::super::effect::lower_effect_row(ctx, &closure.effects, &[], closure.span);
    if has_panic && let Some(b) = ctx.body.as_mut() {
        b.set_panic(true);
    }

    // Env word — present so the fat-value ABI is uniform across
    // capturing and captureless closures and named-fn shims. Sits after
    // the capability slots.
    let env_local = match ctx.body.as_mut() {
        Some(b) => b.param(ParamMode::Let, MirType::prim(MirPrim::Usize), closure.span),
        None => return None,
    };

    lower_closure_params(ctx, closure);

    let entry = alloc_block(ctx)?;
    if let Some(builder) = ctx.body.as_mut() {
        builder.set_entry(entry);
    }
    ctx.current_bb = Some(entry);

    // Unpack the single capture from the env word into a body-local
    // bound to the capture's name, so the body resolves it like any
    // other binding.
    if let Some((sym, cap_ty, cap_prim)) = capture {
        seed_capture(ctx, env_local, sym, cap_ty, cap_prim, closure.span);
    }

    lower_closure_body_block(ctx, closure, return_slot);

    // Seal the trailing `Return` when the body did not already diverge.
    if ctx.current_bb.is_some() {
        seal_terminator(
            ctx,
            Terminator {
                span: closure.span,
                kind: TerminatorKind::Return(Operand::Move(Place::local(return_slot))),
            },
        );
    }

    let builder = ctx.body.take()?;
    let body = builder.finish();
    Some(ctx.program.push_body(body))
}

/// Materialise the captured value into a body-local by casting the env
/// word back to the capture's primitive, then bind the capture name to
/// that local in `ctx.bindings`.
fn seed_capture(
    ctx: &mut LoweringContext<'_>,
    env_local: LocalId,
    sym: Symbol,
    cap_ty: MirType,
    cap_prim: MirPrim,
    span: edda_span::Span,
) {
    let cap_local = match ctx.body.as_mut() {
        Some(b) => b.temp(cap_ty.clone(), span),
        None => return,
    };
    push_assign(
        ctx,
        span,
        cap_local,
        Rvalue {
            span,
            kind: RvalueKind::Cast {
                src: Operand::Copy(Place::local(env_local)),
                src_prim: MirPrim::Usize,
                dst_prim: cap_prim,
            },
            ty: cap_ty,
        },
    );
    ctx.bindings.insert(sym, cap_local);
}

/// Lower the closure's value parameters in declaration order (after the
/// leading env word), seeding `ctx.bindings`.
fn lower_closure_params(ctx: &mut LoweringContext<'_>, closure: &HirClosure) {
    for param in closure.params.iter() {
        let ty = lower_ty(ctx.ty_interner, &ctx.adt_map, param.ty);
        if matches!(ty.kind, MirTypeKind::Never) {
            continue;
        }
        let mode = super::super::register::map_param_mode(param.mode);
        let Some(builder) = ctx.body.as_mut() else {
            return;
        };
        let local = builder.param(mode, ty, param.span);
        ctx.bindings.insert(param.name.name, local);
    }
}

/// Walk the closure body's statements then its trailing expression into
/// the return slot. Mirrors `super::lower_function_body` + its
/// trailing-Ok-wrap (`super::flow::wrap_ok_if_raising`).
fn lower_closure_body_block(
    ctx: &mut LoweringContext<'_>,
    closure: &HirClosure,
    return_slot: LocalId,
) {
    super::super::scope::enter_scope(ctx);
    for stmt in closure.body.stmts.iter() {
        super::super::stmt::lower_stmt(ctx, stmt);
        if ctx.current_bb.is_none() {
            super::super::scope::exit_scope(ctx, closure.body.span);
            return;
        }
    }
    if let Some(trailing) = closure.body.trailing.as_deref() {
        let op = super::super::expr::lower_expr_to_operand(ctx, trailing);
        let trailing_is_never = matches!(
            ctx.ty_interner.kind(trailing.ty),
            edda_types::TyKind::Primitive(edda_types::Primitive::Never)
        );
        if !trailing_is_never {
            let return_ty = match ctx.body.as_ref() {
                Some(b) => b.body_ref().return_ty.clone(),
                None => {
                    super::super::scope::exit_scope(ctx, closure.body.span);
                    return;
                }
            };
            let final_op = wrap_ok_if_raising(ctx, trailing.span, op);
            assign_into(ctx, trailing.span, return_slot, final_op, return_ty);
        }
    }
    super::super::scope::exit_scope(ctx, closure.body.span);
}

/// Ok-wrap `op` into the closure body's synthesised `Result<T, E>` ADT
/// when the closure is raising; otherwise return `op` unchanged.
fn wrap_ok_if_raising(ctx: &mut LoweringContext<'_>, span: edda_span::Span, op: Operand) -> Operand {
    let Some((result_adt_id, ok_idx)) = ctx.result_adt else {
        return op;
    };
    let ok_has_payload = ctx
        .program
        .program()
        .adts
        .get(result_adt_id)
        .and_then(|adt| adt.variants.get(ok_idx.as_index()))
        .map(|v| !v.fields.is_empty())
        .unwrap_or(true);
    let fields = if ok_has_payload { vec![op] } else { Vec::new() };
    let ok_ty = MirType::new(MirTypeKind::Adt(result_adt_id));
    let tmp = match ctx.body.as_mut() {
        Some(b) => b.temp(ok_ty.clone(), span),
        None => return Operand::Unit,
    };
    push_assign(
        ctx,
        span,
        tmp,
        Rvalue {
            span,
            kind: RvalueKind::MakeVariant {
                adt: result_adt_id,
                variant: ok_idx,
                fields,
            },
            ty: ok_ty,
        },
    );
    Operand::Move(Place::local(tmp))
}
