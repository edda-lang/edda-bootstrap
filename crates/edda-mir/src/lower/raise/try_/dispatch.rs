//! Dispatcher / propagation block construction for `?` lowering.
//!
//! Fills the per-err `SwitchTag` dispatcher over the call destination's
//! `__Result` sum, builds its arm list (routing each err variant to a
//! matching handler or a fresh propagation block), and extracts the Ok
//! payload from the Result-typed destination on the success path.

use edda_span::Span;
use edda_types::TyId;

use crate::ids::{AdtId, BlockId, FieldIdx, LocalId, VariantIdx};
use crate::operand::Operand;
use crate::place::Place;
use crate::terminator::{Terminator, TerminatorKind};
use crate::ty::{MirType, MirTypeKind};

use crate::lower::cfg::{alloc_block, seal_terminator};
use crate::lower::ctx::LoweringContext;
use crate::lower::pattern::emit_extract_field_variant;
use crate::lower::raise::{
    destination_result_adt, find_err_variant_idx, operand_local, seal_raise,
};
use crate::lower::ty::lower_ty;

use super::resolve::match_handler;

/// Fill the dispatcher block with a per-err `SwitchTag` over the call
/// destination's `__Result` sum. Each err ADT in `err_adts` gets its
/// own switch arm; the otherwise arm reuses the first arm's target
/// because the dispatcher is unreachable when the call's tag is Ok.
pub(in crate::lower) fn fill_dispatcher_block(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    dispatcher_bb: BlockId,
    err_adts: &[AdtId],
    result_op: &Operand,
) {
    let saved = ctx.current_bb;
    ctx.current_bb = Some(dispatcher_bb);

    let Some(dest_local) = operand_local(result_op) else {
        // Resolution failed upstream; emit a divergent Raise to keep
        // the dispatcher block sealed and the CFG well-formed.
        if let Some(adt) = err_adts.first().copied() {
            seal_raise(ctx, span, adt, Operand::Unit);
        }
        ctx.current_bb = saved;
        return;
    };

    let Some(result_adt_id) = destination_result_adt(ctx, dest_local) else {
        // The destination is not a synthesized __Result. Seal with a
        // single Raise against the first err to keep the CFG total.
        if let Some(adt) = err_adts.first().copied() {
            seal_raise(ctx, span, adt, Operand::Unit);
        }
        ctx.current_bb = saved;
        return;
    };

    let arms = build_dispatcher_arms(ctx, span, err_adts, result_adt_id, dest_local);

    // Pick the otherwise target from the first arm (the dispatcher is
    // unreachable when the call's tag is Ok, so the otherwise branch
    // is dead in practice). If no arm could be built — every err ADT
    // failed to resolve — seal with Unreachable so the CFG remains total.
    let Some(otherwise) = arms.first().map(|(_, bb)| *bb) else {
        ctx.current_bb = saved;
        if let Some(body_builder) = ctx.body.as_mut() {
            body_builder.body_mut().blocks[dispatcher_bb].terminator = Terminator {
                span,
                kind: TerminatorKind::Unreachable,
            };
        }
        return;
    };

    seal_terminator(
        ctx,
        Terminator {
            span,
            kind: TerminatorKind::SwitchTag {
                subject: Operand::Copy(Place::local(dest_local)),
                adt: result_adt_id,
                arms,
                otherwise,
            },
        },
    );
    // `seal_terminator` cleared `current_bb`; restore the success
    // continuation that `lower_call` set.
    ctx.current_bb = saved;
}

/// Build the per-err arm list for the dispatcher's `SwitchTag`.
fn build_dispatcher_arms(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    err_adts: &[AdtId],
    result_adt_id: AdtId,
    dest_local: LocalId,
) -> Vec<(VariantIdx, BlockId)> {
    let mut arms: Vec<(VariantIdx, BlockId)> = Vec::with_capacity(err_adts.len());
    for err_adt in err_adts {
        let Some(variant_idx) = find_err_variant_idx(ctx, result_adt_id, *err_adt) else {
            // The callee's err row contains an ADT that does not appear
            // in the destination __Result. The codegen pre-pass is
            // expected to keep these in sync; reaching this branch
            // signals an upstream defect. Skip the arm.
            continue;
        };
        let target = match match_handler(ctx, *err_adt) {
            Some((recovery_bb, None)) => recovery_bb,
            Some((recovery_bb, Some(binder_local))) => {
                let Some(binder_fill_bb) = alloc_block(ctx) else {
                    continue;
                };
                fill_binder_extract_block(
                    ctx,
                    span,
                    binder_fill_bb,
                    variant_idx,
                    *err_adt,
                    dest_local,
                    binder_local,
                    recovery_bb,
                );
                binder_fill_bb
            }
            None => {
                let Some(propagation_bb) = alloc_block(ctx) else {
                    continue;
                };
                fill_single_propagation_block(
                    ctx,
                    span,
                    propagation_bb,
                    variant_idx,
                    *err_adt,
                    dest_local,
                );
                propagation_bb
            }
        };
        arms.push((variant_idx, target));
    }
    arms
}

/// Fill a per-err binder-fill block: extract the err payload from the
/// known variant of the call destination into the handler's binder
/// local, then jump to the handler's recovery block.
fn fill_binder_extract_block(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    binder_fill_bb: BlockId,
    err_variant_idx: VariantIdx,
    err_adt: AdtId,
    dest_local: LocalId,
    binder_local: LocalId,
    recovery_bb: BlockId,
) {
    let saved = ctx.current_bb;
    ctx.current_bb = Some(binder_fill_bb);

    let err_ty = MirType::new(MirTypeKind::Adt(err_adt));
    emit_extract_field_variant(
        ctx,
        span,
        dest_local,
        Some(err_variant_idx),
        FieldIdx::from_raw(0),
        binder_local,
        err_ty,
    );
    crate::lower::cfg::goto(ctx, span, recovery_bb);
    ctx.current_bb = saved;
}

/// Fill a per-err propagation block: extract the err payload from the
/// known variant of the call destination and raise into the caller's
/// frame.
fn fill_single_propagation_block(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    propagation_bb: BlockId,
    err_variant_idx: VariantIdx,
    err_adt: AdtId,
    dest_local: LocalId,
) {
    let saved = ctx.current_bb;
    ctx.current_bb = Some(propagation_bb);

    let err_ty = MirType::new(MirTypeKind::Adt(err_adt));
    let err_local = match ctx.body.as_mut() {
        Some(body) => body.temp(err_ty.clone(), span),
        None => {
            ctx.current_bb = saved;
            return;
        }
    };
    emit_extract_field_variant(
        ctx,
        span,
        dest_local,
        Some(err_variant_idx),
        FieldIdx::from_raw(0),
        err_local,
        err_ty,
    );
    seal_raise(ctx, span, err_adt, Operand::Move(Place::local(err_local)));
    // `seal_terminator` cleared `current_bb` after sealing the
    // propagation block; restore the success continuation.
    ctx.current_bb = saved;
}

/// Extract the Ok payload from a `__Result<T, E>`-typed call destination
/// into a fresh local typed as the source-level success type `T`.
pub(super) fn extract_ok_payload(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    expr_ty: TyId,
    result_op: &Operand,
) -> Operand {
    let Some(dest_local) = operand_local(result_op) else {
        return result_op.clone();
    };
    let Some(result_adt_id) = destination_result_adt(ctx, dest_local) else {
        // Destination is not a synthesized Result — non-raising callee path.
        return result_op.clone();
    };
    // Ok variant has no payload field when the success type was Unit.
    let ok_has_payload = ctx
        .program
        .program()
        .adts
        .get(result_adt_id)
        .and_then(|adt| adt.variants.first())
        .map(|v| !v.fields.is_empty())
        .unwrap_or(true);
    if !ok_has_payload {
        return Operand::Unit;
    }
    let value_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, expr_ty);
    let value_local = match ctx.body.as_mut() {
        Some(body) => body.temp(value_ty.clone(), span),
        None => return result_op.clone(),
    };
    // Ok is always variant 0 of the synthesized Result per
    // `register_function_bodies` / `register_externs`.
    emit_extract_field_variant(
        ctx,
        span,
        dest_local,
        Some(VariantIdx::from_raw(0)),
        FieldIdx::from_raw(0),
        value_local,
        value_ty,
    );
    Operand::Move(Place::local(value_local))
}
