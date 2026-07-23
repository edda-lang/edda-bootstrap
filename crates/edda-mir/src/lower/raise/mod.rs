//! `HirExprKind::Raise` + `HirExprKind::Try` lowering.
//!
//! Lowering for the two error-propagation primitives in the typed
//! HIR.
//!
//! - **Raise(inner)** — `raise <expr>` originates an error. The payload's
//!   [`edda_types::TyId`] resolves to a [`edda_types::TyKind::Nominal`]
//!   whose binding keys [`super::ctx::LoweringContext::adt_map`]; the
//!   resulting [`crate::AdtId`] is the `err_adt` argument of
//!   [`crate::TerminatorKind::Raise`]. The current block is sealed with
//!   `Raise`; the diverging operand `Operand::Unit` is returned and
//!   `current_bb` is left `None` — same shape as `Return` / `Panic`.
//!   Lives in [`explicit`].
//!
//! - **Try(inner)** — `<expr>?` propagates an error to the enclosing
//!   function's caller. Only the shape `<call-expr>?` is currently admitted —
//!   the typed-HIR has no `Result<T, E>` (per `edda-types`'
//!   `synth_try`, `Try(inner).ty == inner.ty == T`), so the err type
//!   has to be discovered from `inner`'s contribution to the row. Lives
//!   in [`try_`].
//!
//! # Architectural note
//!
//! Currently the typed HIR does not yet model `Result<T, E>` — `Try`'s
//! value type is the success type `T` and the err type comes from the
//! row entry, not from the value type itself. The compile-side
//! `lower_call(on_error: Some(_))` walker treats the call destination
//! as a Result-shaped sum at the LLVM level even though the MIR
//! destination's `MirType` is `T`. The propagation block therefore
//! re-raises with `value = Move(destination)` — structurally honest at
//! the MIR layer, but the value passed to `Raise` does not yet have
//! the typed shape the compile-side `find_err_variant` expects.
//! A future `edda-types` rewrite that surfaces `Result<T, E>` as a sum
//! ADT in the typed HIR will close this gap.

mod explicit;
// The submodule is named `try_` because `try` is reserved in Rust.
mod try_;

pub(super) use explicit::lower_raise;
pub(super) use try_::fill_dispatcher_block;
pub(super) use try_::lower_try;

use edda_span::Span;
use edda_types::{TyId, TyKind};

use crate::adt::AdtKind;
use crate::error::{LoweringError, MirError};
use crate::ids::{AdtId, LocalId, VariantIdx};
use crate::operand::Operand;
use crate::terminator::{Terminator, TerminatorKind};
use crate::ty::MirTypeKind;

use super::cfg::seal_terminator;
use super::ctx::LoweringContext;

/// Resolve `ty` (typically a Raise payload's type) to its `AdtId` via
/// `TyKind::Nominal(binding) -> ctx.adt_map[binding]`. Non-nominal payload
/// types and bindings not registered as ADTs surface diagnostics.
pub(super) fn resolve_err_adt(
    ctx: &mut LoweringContext<'_>,
    ty: TyId,
    span: Span,
) -> Option<AdtId> {
    let kind = ctx.ty_interner.kind(ty);
    let TyKind::Nominal(binding) = kind else {
        ctx.errors.push(MirError::from(LoweringError::UnsupportedErrTypeInRow {
            span,
        }));
        return None;
    };
    match ctx.adt_map.get(binding).copied() {
        Some(adt_id) => Some(adt_id),
        None => {
            // The type-decl pre-pass should have seen every nominal type the
            // body references. A missing entry signals an upstream defect
            // (incomplete `type_decls` slice or mis-keyed binding); surface
            // as `UnsupportedErrTypeInRow` so the user has a clear pointer.
            ctx.errors.push(MirError::from(LoweringError::UnsupportedErrTypeInRow {
                span,
            }));
            None
        }
    }
}

/// Seal the current block with `Raise { err_adt, value }`.
pub(super) fn seal_raise(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    err_adt: AdtId,
    value: Operand,
) {
    seal_terminator(
        ctx,
        Terminator {
            span,
            kind: TerminatorKind::Raise { err_adt, value },
        },
    );
}

/// Extract the local id from a non-projected place operand. Used by the
/// Try-lowering helpers to recover the call destination from
/// `lower_call`'s return value.
pub(super) fn operand_local(op: &Operand) -> Option<LocalId> {
    let place = match op {
        Operand::Move(p) | Operand::Copy(p) => p,
        _ => return None,
    };
    if !place.projection.is_empty() {
        return None;
    }
    Some(place.local)
}

/// Inspect a local's MIR type and return its ADT id if it is a sum ADT —
/// the shape the call lowering produces for a raising callee's
/// destination.
pub(super) fn destination_result_adt(
    ctx: &LoweringContext<'_>,
    local: LocalId,
) -> Option<AdtId> {
    let body = ctx.body.as_ref()?;
    let decl = body.body_ref().locals.get(local)?;
    let MirTypeKind::Adt(adt_id) = decl.ty.kind else {
        return None;
    };
    let adt = ctx.program.program().adts.get(adt_id)?;
    if matches!(adt.kind, AdtKind::Sum) {
        Some(adt_id)
    } else {
        None
    }
}

/// Find the variant index in a synthesized `__Result` ADT whose single
/// payload field references the given err ADT. Mirrors the search in
/// the compile-side `find_err_variant` so the err extracted here lands
/// in the right slot when the caller's `Raise` lowering repackages it.
pub(super) fn find_err_variant_idx(
    ctx: &LoweringContext<'_>,
    result_adt_id: AdtId,
    err_adt_id: AdtId,
) -> Option<VariantIdx> {
    let result_adt = ctx.program.program().adts.get(result_adt_id)?;
    for (i, variant) in result_adt.variants.iter().enumerate() {
        if variant.fields.len() != 1 {
            continue;
        }
        if let MirTypeKind::Adt(payload_id) = &variant.fields[0].ty.kind {
            if *payload_id == err_adt_id {
                return Some(VariantIdx::from_raw(i as u32));
            }
        }
    }
    None
}
