//! Effect / capability dispatch for the typed-HIR -> MIR lowering pass.
//!
//! Walks an `edda_types::EffectRow` and produces the body-scoped MIR-side
//! representation: capability slots threaded as synthetic leading parameter
//! locals, error ADTs to add to the body's `may_raise` list, and the
//! `has_panic` bit. The pass also seeds `ctx.capabilities` so subsequent
//! `Call` lowering can resolve capability names to [`EffectId`]s.

use edda_intern::Symbol;
use edda_span::Span;
use edda_types::{
    CapabilityType, EffectEntry, EffectRow, Param, PureEffect, TyId, TyKind,
};

use crate::effect::CapabilityKind;
use crate::error::{LoweringError, MirError};
use crate::layout::LayoutInfo;
use crate::ty::{MirType, MirTypeKind};

use super::ctx::LoweringContext;

/// Lower an `edda_types::EffectRow` into the active body, returning whether
/// the body's `has_panic` bit should be set. Call before any non-capability
/// param is added so capability params land as leading parameters.
///
/// `params` is the declaring function's parameter list; it lets capability
/// classification recover a row entry's source-level [`CapabilityType`] (by
/// matching the entry's `Symbol` against a parameter name) so a
/// `.read_only()`-style narrowed capability threads as its base kind rather
/// than degrading to an opaque `Named` slot.
pub(super) fn lower_effect_row(
    ctx: &mut LoweringContext<'_>,
    row: &EffectRow,
    params: &[Param],
    decl_span: Span,
) -> bool {
    let mut has_panic = false;
    for entry in row.entries() {
        match entry {
            EffectEntry::Capability(name) => {
                let cap_ty = capability_type_of_param(ctx, params, *name);
                lower_capability(ctx, *name, cap_ty, decl_span);
            }
            EffectEntry::Pure(PureEffect::Panic) => {
                has_panic = true;
            }
            EffectEntry::Pure(PureEffect::Err(t)) => {
                lower_err_payload(ctx, *t, decl_span);
            }
            EffectEntry::Pure(PureEffect::Yield(_)) => {
                ctx.errors.push(MirError::from(
                    LoweringError::UnsupportedYieldEffect { span: decl_span },
                ));
            }
            // Divergence is a verification-only effect with no runtime
            // representation — the typechecker uses it to track
            // termination; MIR lowering emits no code for it.
            EffectEntry::Pure(PureEffect::Divergence) => {}
            // Cancellation is a verification-only effect at this
            // generic row-lowering level too — the typechecker uses it
            // to track that `.await` propagates cancellation into its
            // enclosing row; actual cancellation checks are wired
            // through the scope(exec)/Task lowering paths, not through
            // this per-entry dispatch, so MIR lowering emits no code
            // for it here.
            EffectEntry::Pure(PureEffect::Cancellation) => {}
            // Nondet is a verification-only effect with no runtime
            // representation and no ABI slot — it marks observable
            // non-determinism (`group.race` / `group.any`, ambient
            // `Random`) for the row-containment / stability checks. MIR
            // lowering emits no code and threads no capability slot for
            // it (unlike a capability entry, which the arm above lowers
            // to a leading `ptr`).
            EffectEntry::Pure(PureEffect::Nondet) => {}
        }
    }
    has_panic
}

/// Resolve an `err: T` payload type to an [`AdtId`] and append it to the
/// active body's `effect_row.errors` via `BodyBuilder::add_error`. Non-nominal
/// payload types surface `UnsupportedErrTypeInRow`; the `Error` sentinel
/// drops silently because the originating diagnostic was emitted upstream.
fn lower_err_payload(ctx: &mut LoweringContext<'_>, t: TyId, decl_span: Span) {
    match ctx.ty_interner.kind(t) {
        TyKind::Error => {}
        TyKind::Nominal(binding) => {
            if let Some(adt_id) = ctx.adt_map.get(binding).copied()
                && let Some(body) = ctx.body.as_mut()
            {
                body.add_error(adt_id);
            }
        }
        _ => {
            ctx.errors.push(MirError::from(
                LoweringError::UnsupportedErrTypeInRow { span: decl_span },
            ));
        }
    }
}

/// Add one capability slot to the active body and seed `ctx.capabilities`.
/// `cap_ty` is the row entry's source-level [`CapabilityType`] when the
/// declaring parameter's type was a capability — used to classify narrowed
/// capabilities (`fs: ReadOnlyFilesystem`) by their base kind.
fn lower_capability(
    ctx: &mut LoweringContext<'_>,
    name: Symbol,
    cap_ty: Option<CapabilityType>,
    span: Span,
) {
    let kind = classify_capability(ctx, name, cap_ty);
    let slot_ty = MirType {
        kind: MirTypeKind::Capability(kind.clone()),
        layout: LayoutInfo::natural(),
    };
    let effect_id = match ctx.body.as_mut() {
        Some(body) => body.add_capability(kind, slot_ty, span),
        None => return,
    };
    ctx.capabilities.insert(name, effect_id);
}

/// Map a capability row entry to its [`CapabilityKind`]. The well-known
/// names match `docs/types/effect-tracking.md`; capability names
/// outside that set are classified through the parameter's source-level
/// [`CapabilityType`] so narrowed capabilities reach a concrete kind.
pub(super) fn classify_capability(
    ctx: &LoweringContext<'_>,
    name: Symbol,
    cap_ty: Option<CapabilityType>,
) -> CapabilityKind {
    match ctx.interner.resolve(name) {
        "allocator" => CapabilityKind::Allocator,
        "clock" => CapabilityKind::Clock,
        "fs" => CapabilityKind::Fs,
        "net" => CapabilityKind::Net,
        "env" => CapabilityKind::Env,
        "args" => CapabilityKind::Args,
        "random" => CapabilityKind::Random,
        _ => match cap_ty {
            Some(cap) => cap_kind_for_type(cap),
            None => CapabilityKind::Named(name),
        },
    }
}

/// Map a source-level [`CapabilityType`] to the effect-row [`CapabilityKind`]
/// that threads it. Bare primitives with a keyword kind (`Allocator`/`Clock`/
/// `Filesystem`/`Network`/`Random`) and every narrowing of one of those
/// collapse onto that base kind; capabilities with no primitive keyword
/// counterpart carry their type via `Typed` so renderers can still name them.
fn cap_kind_for_type(cap: CapabilityType) -> CapabilityKind {
    match cap {
        CapabilityType::Allocator | CapabilityType::BoundedAllocator => {
            CapabilityKind::Allocator
        }
        CapabilityType::Filesystem
        | CapabilityType::ReadOnlyFilesystem
        | CapabilityType::SandboxedFilesystem => CapabilityKind::Fs,
        CapabilityType::Network
        | CapabilityType::LocalhostNetwork
        | CapabilityType::RestrictedNetwork => CapabilityKind::Net,
        CapabilityType::Random | CapabilityType::DeterministicRandom => {
            CapabilityKind::Random
        }
        CapabilityType::Clock => CapabilityKind::Clock,
        // No bare-keyword primitive counterpart in the effect-row kind set:
        // keep the source type so the ABI slot still renders/debug-names
        // correctly. The LLVM ABI is one opaque ptr regardless.
        CapabilityType::MonotonicClock
        | CapabilityType::Stdout
        | CapabilityType::Stderr
        | CapabilityType::Stdin
        | CapabilityType::Executor
        | CapabilityType::Subprocess
        | CapabilityType::Debugger => CapabilityKind::Typed(cap),
    }
}

/// Resolve a capability row entry's source-level [`CapabilityType`] by
/// matching its `Symbol` against the declaring function's parameter list.
fn capability_type_of_param(
    ctx: &LoweringContext<'_>,
    params: &[Param],
    name: Symbol,
) -> Option<CapabilityType> {
    let param = params.iter().find(|p| p.name == name)?;
    match ctx.ty_interner.kind(param.ty) {
        TyKind::Capability(cap) => Some(*cap),
        _ => None,
    }
}
