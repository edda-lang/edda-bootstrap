//! Err-row resolution for `?`-propagation lowering.
//!
//! Discovers the err `AdtId`s a `?`-wrapped call may propagate, by
//! inspecting the callee's effect row (binding-resolved or fn-pointer
//! `may_raise`), and the small predicates that gate admissibility
//! (`body_has_error_scope`) and handler matching (`match_handler`).

use edda_span::Span;
use edda_types::{EffectEntry, FnSig, HirExpr, HirExprKind, PureEffect, TyId};

use crate::error::{LoweringError, MirError};
use crate::ids::{AdtId, BlockId, LocalId};
use crate::ty::MirTypeKind;

use crate::lower::ctx::LoweringContext;
use crate::lower::raise::resolve_err_adt;
use crate::lower::ty::lower_ty;

/// Find an enclosing handler frame whose discharged err ADT matches
/// `err_adt`. Returns `Some((recovery_bb, binder_local))` when a frame
/// matches: `recovery_bb` is the block `?` jumps to instead of
/// propagating, and `binder_local` is the user-binder local the
/// dispatcher must populate (when the source wrote `handle ... as
/// <binder>`) before jumping.
pub(super) fn match_handler(
    ctx: &LoweringContext<'_>,
    err_adt: AdtId,
) -> Option<(BlockId, Option<LocalId>)> {
    ctx.handler_stack
        .iter()
        .rev()
        .find(|f| f.handled_adt == err_adt)
        .map(|f| (f.recovery_bb, f.binder_local))
}

/// Whether the body being lowered declared any `err: E` in its effect
/// row. `?` propagation outside an error scope is rejected per
/// `effect-tracking.md §3`.
pub(super) fn body_has_error_scope(ctx: &LoweringContext<'_>) -> bool {
    ctx.body
        .as_ref()
        .is_some_and(|b| !b.body_ref().effect_row.errors.is_empty())
}

/// Pull every err `AdtId` to potentially propagate out of a `?` inner
/// expression. `inner` is either a `Call` or a `MethodCall`; for the
/// former the callee path is the resolution surface, for the latter the
/// MethodCall span carries the resolution key. Returns `None` if the
/// signature lookup failed; returns `Some(vec)` even when the callee
/// declares no err entries (caller distinguishes empty from absent).
pub(super) fn callee_err_adts_of_inner(
    ctx: &mut LoweringContext<'_>,
    inner: &HirExpr,
    site_span: Span,
) -> Option<Vec<AdtId>> {
    // Capability-method calls (`allocator.alloc_array(T, n)?`) are recorded
    // in `ctx.capability_method_calls`, not `method_resolutions`, so the
    // generic `callee_fn_sig` lookup below cannot see their err row. Resolve
    // their err ADTs directly so `?` takes the propagation path (err dispatch
    // + Ok-payload extraction) instead of silently degrading to a raw move of
    // the 24-byte `__Result` into the 16-byte success local — the
    // null-slice-ptr segfault.
    if let HirExprKind::MethodCall { .. } = &inner.kind {
        if let Some(adts) =
            crate::lower::capability_method_call::capability_method_err_adts(ctx, inner.span)
        {
            return Some(adts);
        }
    }
    let sig_source: &HirExpr = match &inner.kind {
        HirExprKind::Call { callee, .. } => callee,
        HirExprKind::MethodCall { .. } => inner,
        _ => return None,
    };
    // Binding-based resolution first: direct calls and method calls whose
    // callee is a registered function (entry in `ctx.function_sigs`).
    if let Some(adts) = callee_err_adts(ctx, sig_source, site_span) {
        return Some(adts);
    }
    // Indirect (fn-value) callee fallback: a fn-pointer local / param has no
    // `function_sigs` entry, so the binding-based lookup above returns `None`.
    // Recover the err ADTs from the callee's `FnPtr` type's `may_raise` — the
    // identical signature `super::super::call::lower_indirect_call` projects the
    // call's `__Result`-typed destination from. Without this, `?` on a raising
    // fn-value takes the no-propagation path (no Ok-payload extraction) while the
    // call still writes a `__Result`-shaped value into the destination, so the
    // success value is read off the Result base instead of the Ok payload field —
    // an 8-byte shift of the whole returned aggregate that lands the kind tag in
    // the box-pointer slot.
    indirect_callee_err_adts(ctx, inner)
}

/// Recover a raising indirect (fn-pointer) callee's err ADTs from its
/// `FnPtr` type so `?` takes the propagation + Ok-payload-extraction
/// path. The binding-based [`callee_err_adts`] cannot see a fn-value's
/// err row because a fn-pointer local / param has no `function_sigs`
/// entry.
fn indirect_callee_err_adts(
    ctx: &LoweringContext<'_>,
    inner: &HirExpr,
) -> Option<Vec<AdtId>> {
    let HirExprKind::Call { callee, .. } = &inner.kind else {
        return None;
    };
    let callee_mir_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, callee.ty);
    let MirTypeKind::FnPtr(mir_sig) = callee_mir_ty.kind else {
        return None;
    };
    if mir_sig.may_raise.is_empty() {
        return None;
    }
    Some(mir_sig.may_raise.clone())
}

/// Resolve every err entry in the callee's effect row to its
/// corresponding `AdtId`. The returned vec preserves the canonical
/// row order (so dispatcher arms are deterministic across runs).
fn callee_err_adts(
    ctx: &mut LoweringContext<'_>,
    callee: &HirExpr,
    site_span: Span,
) -> Option<Vec<AdtId>> {
    let sig = callee_fn_sig(ctx, callee)?;
    let err_tys: Vec<TyId> = sig
        .effects
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            EffectEntry::Pure(PureEffect::Err(t)) => Some(*t),
            _ => None,
        })
        .collect();
    if err_tys.is_empty() {
        ctx.errors.push(MirError::from(LoweringError::UnsupportedHirVariant {
            variant: "Try (callee declares no err effect)",
            span: site_span,
        }));
        return None;
    }
    let mut adts: Vec<AdtId> = Vec::with_capacity(err_tys.len());
    for err_ty in err_tys {
        if let Some(adt) = resolve_err_adt(ctx, err_ty, site_span) {
            adts.push(adt);
        }
    }
    Some(adts)
}

/// Look up the callee's signature for `Try`'s err-row inspection.
fn callee_fn_sig<'a>(
    ctx: &mut LoweringContext<'a>,
    callee: &HirExpr,
) -> Option<&'a FnSig> {
    use edda_resolve::Resolved;
    // Method-call inner: `receiver.method(args)?`.
    if let HirExprKind::MethodCall { .. } = &callee.kind {
        let binding = ctx.method_resolutions.get(&callee.span).copied()?;
        return ctx.function_sigs.get(&binding).copied();
    }
    let HirExprKind::Path(path) = &callee.kind else {
        ctx.errors.push(MirError::from(LoweringError::UnsupportedHirVariant {
            variant: "Try (callee is not a Path)",
            span: callee.span,
        }));
        return None;
    };
    let binding = match ctx.resolutions.lookup_path(path.span) {
        Some(Resolved::Binding(id)) => id,
        _ if path.segments.len() == 1 => {
            let name = path.segments[0].name;
            ctx.function_symbols.get(&name).copied()?
        }
        _ => {
            ctx.errors.push(MirError::from(LoweringError::MultiSegmentPath {
                span: callee.span,
            }));
            return None;
        }
    };
    ctx.function_sigs.get(&binding).copied()
}
