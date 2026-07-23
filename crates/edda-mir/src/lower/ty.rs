//! `edda_types::TyId` -> `MirType` bridge.
//!
//! Types are lowered structurally: primitives map 1:1 to [`MirPrim`],
//! tuples and slices recurse. The `TyKind::Nominal(BindingId)`
//! arm is handled by the ADT pre-pass, which populates `ctx.adt_map` with `BindingId -> AdtId`
//! before any body walk, so `lower_ty` looks each nominal reference up and
//! produces `MirTypeKind::Adt(AdtId)`.
//!
//! Sentinel handling: [`TyKind::Error`] (the inference cascade-suppress
//! sentinel) lowers via [`error_ty`] to a `Never`-shaped placeholder so
//! downstream MIR emission can proceed without producing a second
//! diagnostic about a problem already reported by `edda-types`. The `Type`
//! primitive — the comptime meta-type — never has a runtime form and
//! lowers to `Never` silently for the same reason. A `Nominal(binding)`
//! whose binding is absent from `adt_map` also lowers to the placeholder:
//! the type-decl pre-pass should have seen every nominal type the body
//! references, so an unknown binding signals an upstream defect (typically
//! the caller passed an incomplete `type_decls` slice) but does not
//! warrant a second cascade.

use std::collections::HashMap;

use edda_resolve::BindingId;
use edda_types::{EffectEntry, FnPtrSig, Primitive, PureEffect, TyId, TyInterner, TyKind};

use crate::effect::CapabilityKind;
use crate::ids::AdtId;
use crate::layout::LayoutInfo;
use crate::ty::{FnSig, MirPrim, MirType, MirTypeKind, ParamMode};

/// Lower a typed-HIR `TyId` to its MIR counterpart, resolving nominal user
/// types through the program-wide `adt_map`.
pub(super) fn lower_ty(
    ty_interner: &TyInterner,
    adt_map: &HashMap<BindingId, AdtId>,
    id: TyId,
) -> MirType {
    match ty_interner.kind(id) {
        TyKind::Primitive(prim) => lower_primitive(*prim),
        TyKind::Tuple(elements) => {
            let elems: Vec<MirType> = elements
                .iter()
                .map(|elem| lower_ty(ty_interner, adt_map, *elem))
                .collect();
            MirType::new(MirTypeKind::Tuple(elems))
        }
        TyKind::Slice(elem) => {
            let inner = lower_ty(ty_interner, adt_map, *elem);
            MirType::new(MirTypeKind::Slice(Box::new(inner)))
        }
        TyKind::Nominal(binding) => match adt_map.get(binding) {
            Some(adt_id) => MirType::new(MirTypeKind::Adt(*adt_id)),
            None => error_ty(),
        },
        // Capability types DO have a runtime representation — one opaque
        // ptr per handle. Lower to a `Capability(Typed(c))` slot so
        // value-position uses (a `take Stdout` parameter, an
        // `Option_<Cap>.some(value: …)` payload field, a local capability
        // binding) get a real MIR type instead of being silently dropped
        // as `Never`. The effect-row path in [`super::effect::lower_capability`]
        // produces an equivalent slot keyed by parameter name; the two
        // paths converge at the LLVM ABI where every capability passes
        // as one opaque ptr regardless of which kind discriminator it
        // carries.
        TyKind::Capability(cap) => MirType {
            kind: MirTypeKind::Capability(crate::effect::CapabilityKind::Typed(*cap)),
            layout: LayoutInfo::natural(),
        },
        TyKind::FnPtr(sig) => {
            let mir_sig = lower_fn_ptr_sig(ty_interner, adt_map, sig);
            MirType::new(MirTypeKind::FnPtr(Box::new(mir_sig)))
        }
        TyKind::Error => error_ty(),
    }
}

/// Lower a [`Primitive`] to its MIR counterpart.
fn lower_primitive(prim: Primitive) -> MirType {
    let kind = match prim {
        Primitive::I8 => MirTypeKind::Prim(MirPrim::I8),
        Primitive::I16 => MirTypeKind::Prim(MirPrim::I16),
        Primitive::I32 => MirTypeKind::Prim(MirPrim::I32),
        Primitive::I64 => MirTypeKind::Prim(MirPrim::I64),
        Primitive::I128 => MirTypeKind::Prim(MirPrim::I128),
        Primitive::Isize => MirTypeKind::Prim(MirPrim::Isize),
        Primitive::U8 => MirTypeKind::Prim(MirPrim::U8),
        Primitive::U16 => MirTypeKind::Prim(MirPrim::U16),
        Primitive::U32 => MirTypeKind::Prim(MirPrim::U32),
        Primitive::U64 => MirTypeKind::Prim(MirPrim::U64),
        Primitive::U128 => MirTypeKind::Prim(MirPrim::U128),
        Primitive::Usize => MirTypeKind::Prim(MirPrim::Usize),
        Primitive::F32 => MirTypeKind::Prim(MirPrim::F32),
        Primitive::F64 => MirTypeKind::Prim(MirPrim::F64),
        Primitive::Bool => MirTypeKind::Prim(MirPrim::Bool),
        Primitive::Unit => MirTypeKind::Unit,
        Primitive::Never => MirTypeKind::Never,
        Primitive::String => MirTypeKind::Prim(MirPrim::Str),
        // `Type` is the comptime meta-type — values are themselves types, no
        // runtime representation. The type-check pass should have stripped
        // every `Type`-typed binding before reaching MIR; lower silently to
        // `Never` so any straggler suppresses cascade.
        Primitive::Type => MirTypeKind::Never,
        Primitive::HeapPtr => MirTypeKind::Prim(MirPrim::HeapPtr),
        Primitive::Codepoint => MirTypeKind::Prim(MirPrim::Char),
    };
    MirType {
        kind,
        layout: LayoutInfo::natural(),
    }
}

/// Convert a type-system [`FnPtrSig`] into the MIR-side [`FnSig`] that
/// `MirTypeKind::FnPtr` carries. Pure transcription — no body building,
/// no capability slot allocation, no `&mut program`. Used by [`lower_ty`]
/// when projecting `TyKind::FnPtr` and by indirect-call lowering when
/// synthesising the call's signature payload. For a raising fn-ptr the
/// `ret` it produces is the bare success `T`; the Result-shaped wire
/// return is reconciled by [`super::adt::wire_level_ret`] at the indirect
/// call site and by the value-position construction seams.
fn lower_fn_ptr_sig(
    ty_interner: &TyInterner,
    adt_map: &HashMap<BindingId, AdtId>,
    sig: &FnPtrSig,
) -> FnSig {
    let mut params: Vec<(ParamMode, MirType)> = Vec::with_capacity(sig.params.len());
    for p in sig.params.iter() {
        let lowered = lower_ty(ty_interner, adt_map, p.ty);
        // Capability-typed and Never-typed params are skipped from the
        // fn-ptr ABI's value-params list. Capabilities are accounted for
        // via `capabilities` below from the effect row. Per-fn-ptr-param
        // dedup vs effect-row capability slots can't be done by name
        // because `FnPtrParam` is anonymous — so this conservatively
        // strips every capability-typed slot from `params`. Value-position
        // capability storage via fn-ptr ABI is a separate gap, not yet supported.
        if matches!(lowered.kind, MirTypeKind::Never | MirTypeKind::Capability(_)) {
            continue;
        }
        let mode = match p.mode {
            edda_types::ParamMode::Default => ParamMode::Let,
            edda_types::ParamMode::Mutable => ParamMode::Mutable,
            edda_types::ParamMode::Take => ParamMode::Take,
            edda_types::ParamMode::Init => ParamMode::Init,
        };
        params.push((mode, lowered));
    }
    let ret = lower_ty(ty_interner, adt_map, sig.return_ty);

    let mut capabilities: Vec<CapabilityKind> = Vec::new();
    let mut may_raise: Vec<AdtId> = Vec::new();
    let mut may_panic = false;
    for entry in sig.effects.entries() {
        match entry {
            EffectEntry::Capability(name) => {
                // Use `Named` uniformly: the LLVM ABI is one opaque
                // `ptr` per slot regardless of the kind discriminator,
                // and well-known classification needs an interner this
                // pure transcription does not see.
                capabilities.push(CapabilityKind::Named(*name));
            }
            EffectEntry::Pure(PureEffect::Panic) => may_panic = true,
            EffectEntry::Pure(PureEffect::Err(t)) => {
                if let TyKind::Nominal(binding) = ty_interner.kind(*t)
                    && let Some(&adt_id) = adt_map.get(binding)
                {
                    may_raise.push(adt_id);
                }
            }
            EffectEntry::Pure(PureEffect::Yield(_)) => {
                // Yield effects on fn-ptr types are deferred.
            }
            EffectEntry::Pure(PureEffect::Divergence) => {
                // Divergence is verification-only — no runtime payload
                // on fn-ptr types.
            }
            EffectEntry::Pure(PureEffect::Cancellation) => {
                // Cancellation is verification-only — no runtime
                // payload on fn-ptr types.
            }
            EffectEntry::Pure(PureEffect::Nondet) => {
                // Nondet is verification-only — no runtime payload and
                // no ABI slot on fn-ptr types.
            }
        }
    }

    FnSig {
        params,
        ret,
        capabilities,
        may_raise,
        may_panic,
    }
}

/// The `Never`-shaped sentinel returned when a type carries `edda-types`'
/// `Error` sentinel. Lowering uses this so a single upstream diagnostic does
/// not cascade into a wall of MIR-side complaints.
pub(super) fn error_ty() -> MirType {
    MirType {
        kind: MirTypeKind::Never,
        layout: LayoutInfo::natural(),
    }
}

/// Extract the [`MirPrim`] discriminant from a typed-HIR `TyId`, when the type
/// is a primitive scalar.
pub(super) fn ty_to_prim(ty_interner: &TyInterner, id: TyId) -> Option<MirPrim> {
    if let TyKind::Primitive(prim) = ty_interner.kind(id) {
        primitive_to_prim(*prim)
    } else {
        None
    }
}

/// Map a [`Primitive`] to its `MirPrim` counterpart, returning `None` for
/// kinds that have no runtime primitive representation (`Unit`, `Never`,
/// `Type`).
fn primitive_to_prim(prim: Primitive) -> Option<MirPrim> {
    let p = match prim {
        Primitive::I8 => MirPrim::I8,
        Primitive::I16 => MirPrim::I16,
        Primitive::I32 => MirPrim::I32,
        Primitive::I64 => MirPrim::I64,
        Primitive::I128 => MirPrim::I128,
        Primitive::Isize => MirPrim::Isize,
        Primitive::U8 => MirPrim::U8,
        Primitive::U16 => MirPrim::U16,
        Primitive::U32 => MirPrim::U32,
        Primitive::U64 => MirPrim::U64,
        Primitive::U128 => MirPrim::U128,
        Primitive::Usize => MirPrim::Usize,
        Primitive::F32 => MirPrim::F32,
        Primitive::F64 => MirPrim::F64,
        Primitive::Bool => MirPrim::Bool,
        Primitive::String => MirPrim::Str,
        Primitive::HeapPtr => MirPrim::HeapPtr,
        Primitive::Codepoint => MirPrim::Char,
        Primitive::Unit | Primitive::Never | Primitive::Type => return None,
    };
    Some(p)
}

/// Return the canonical short tag for a primitive — used by
/// [`crate::error::LoweringError::UnsupportedCast`] diagnostic rendering.
pub(super) fn prim_tag(prim: MirPrim) -> &'static str {
    prim.as_str()
}
