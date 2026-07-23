//! Pre-passes that populate the lowering context before any function body
//! is walked: extern declarations and source-bodied function-body slot
//! reservation (`BindingId -> reserved BodyId`).

use edda_intern::Symbol;
use edda_types::{EffectEntry, ParamMode as TypesParamMode, PureEffect};

use crate::ids::AdtId;
use crate::ty::{MirType, MirTypeKind, ParamMode};

use super::adt;
use super::ctx::LoweringContext;
use super::effect;
use super::input::{ConstInput, ExternInput, FunctionInput};
use super::ty::lower_ty;

/// Pre-intern every module-level `let` constant into the program and
/// record its `BindingId -> ConstId` mapping in `ctx.module_consts`.
///
/// Path lowering (`super::expr::lower_path`) reads `module_consts` to
/// emit `Operand::Const(id)` whenever a single-segment path resolves
/// to a `BindingKind::Const`. Without this pre-pass references to
/// module-level lets fail with `LoweringError::UnknownBinding`.
pub(super) fn register_consts(ctx: &mut LoweringContext<'_>, consts: &[ConstInput]) {
    for c in consts {
        let mir_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, c.ty);
        let id = ctx.program.push_const(crate::Const {
            ty: mir_ty,
            value: c.value.clone(),
        });
        ctx.module_consts.entry(c.binding).or_insert(id);
    }
}

/// Register every extern declaration into the lowering context.
///
/// Externs are recorded alongside source-bodied functions in
/// `ctx.function_symbols` and `ctx.function_sigs` (so callee-path
/// resolution finds them by name) plus a separate
/// `ctx.function_externs` map that carries the linker symbol plus
/// the pre-lowered MIR-side [`crate::ty::FnSig`] the
/// `FuncRef::Extern` terminator will name.
///
/// For raising externs (those with `err:` entries in their effect row),
/// `mir_sig.ret` is replaced with a synthesized `__Result<T, E>` ADT.
/// This mirrors what [`register_function_bodies`] does for source-bodied
/// raising functions so the LLVM extern declaration returns
/// `{ tag, payload }` — the shape the `on_error` call-site lowering
/// expects when extracting the discriminant at index 0.
pub(super) fn register_externs<'a>(
    ctx: &mut LoweringContext<'a>,
    externs: &'a [ExternInput<'a>],
) {
    for ext in externs {
        let mut mir_sig = lower_extern_sig(ctx, ext.sig);
        if !mir_sig.may_raise.is_empty() {
            let err_adts: Vec<(Symbol, AdtId)> = mir_sig
                .may_raise
                .iter()
                .map(|&adt_id| {
                    let name = ctx
                        .program
                        .program()
                        .adts
                        .get(adt_id)
                        .map(|def| def.name)
                        .unwrap_or_else(|| ctx.interner.intern("__Err"));
                    (name, adt_id)
                })
                .collect();
            let result_adt_id =
                adt::synthesize_result_adt(ctx, mir_sig.ret.clone(), err_adts, ext.span);
            mir_sig.ret = MirType::new(MirTypeKind::Adt(result_adt_id));
        }
        ctx.function_externs
            .insert(ext.binding, (ext.symbol, mir_sig));
        ctx.function_sigs.insert(ext.binding, ext.sig);
        ctx.function_symbols.entry(ext.name).or_insert(ext.binding);
    }
}

/// Lower an `edda_types::FnSig` to its MIR-side counterpart for an
/// extern declaration. The shape mirrors what `BodyBuilder::finish`
/// produces for a source-bodied function so the backend's
/// `declare_extern` ABI lowering can treat both call kinds uniformly.
fn lower_extern_sig(
    ctx: &LoweringContext<'_>,
    edda_sig: &edda_types::FnSig,
) -> crate::ty::FnSig {
    let mut capabilities = Vec::new();
    let mut may_raise = Vec::new();
    let mut may_panic = false;
    // Track which capability NAMES are listed in the effect row so the
    // param iteration below can dedupe a capability-typed parameter whose
    // name appears in the row (it's already accounted for in `capabilities`
    // and would otherwise produce a duplicate ABI slot).
    let mut effect_row_cap_names: std::collections::HashSet<edda_intern::Symbol> =
        std::collections::HashSet::new();
    for entry in edda_sig.effects.entries() {
        match entry {
            EffectEntry::Capability(name) => {
                // Recover the declaring parameter's source-level capability
                // type so a narrowed capability (`fs: ReadOnlyFilesystem`)
                // classifies to its base kind instead of an opaque `Named`
                // slot — same rule as the source-bodied path in
                // `effect::lower_capability`.
                let cap_ty = edda_sig
                    .params
                    .iter()
                    .find(|p| p.name == *name)
                    .and_then(|p| match ctx.ty_interner.kind(p.ty) {
                        edda_types::TyKind::Capability(cap) => Some(*cap),
                        _ => None,
                    });
                capabilities.push(effect::classify_capability(ctx, *name, cap_ty));
                effect_row_cap_names.insert(*name);
            }
            EffectEntry::Pure(PureEffect::Err(err_ty)) => {
                let mir_err = lower_ty(ctx.ty_interner, &ctx.adt_map, *err_ty);
                if let MirTypeKind::Adt(adt_id) = mir_err.kind {
                    may_raise.push(adt_id);
                }
            }
            EffectEntry::Pure(PureEffect::Panic) => may_panic = true,
            EffectEntry::Pure(PureEffect::Yield(_)) => {}
            // Verification-only effect with no runtime presence; MIR
            // lowering emits no code for `divergence`.
            EffectEntry::Pure(PureEffect::Divergence) => {}
            // Verification-only effect with no runtime presence; MIR
            // lowering emits no code for `cancellation`.
            EffectEntry::Pure(PureEffect::Cancellation) => {}
            // Verification-only effect with no runtime presence and no
            // ABI slot; MIR lowering emits no code for `nondet`.
            EffectEntry::Pure(PureEffect::Nondet) => {}
        }
    }

    let mut params = Vec::with_capacity(edda_sig.params.len());
    for param in edda_sig.params.iter() {
        let ty = lower_ty(ctx.ty_interner, &ctx.adt_map, param.ty);
        // Effect-row capability params are routed through `capabilities`
        // above; skip them here so the ABI doesn't grow a duplicate slot.
        if matches!(ty.kind, MirTypeKind::Capability(_))
            && effect_row_cap_names.contains(&param.name)
        {
            continue;
        }
        if matches!(ty.kind, MirTypeKind::Never) {
            continue;
        }
        let mode = map_param_mode(param.mode);
        params.push((mode, ty));
    }

    let ret = lower_ty(ctx.ty_interner, &ctx.adt_map, edda_sig.return_ty);

    crate::ty::FnSig {
        params,
        ret,
        capabilities,
        may_raise,
        may_panic,
    }
}

/// Reserve every source function's body slot and pre-fill
/// `ctx.function_map`, `ctx.function_symbols`, and `ctx.function_sigs`
/// with each function's `BindingId -> BodyId`, `Symbol -> BindingId`,
/// and `BindingId -> &FnSig` mappings. The `Call` terminator
/// lowering reads these to resolve a callee's single-segment path into a
/// [`crate::FuncRef`] plus look up the callee's parameter modes and
/// effect-row capability declarations.
///
/// Slots are RESERVED (not predicted) so the reserved `BodyId` is fixed
/// at reserve time and unaffected by the mid-walk `push_body` calls that
/// the fn-value shim and closure-body lowering perform; `lower_function`
/// fills the reserved slot in place. For each raising function (effect
/// row contains `err: E`), synthesizes a `Result<T, E>` sum ADT and
/// stores it in `ctx.function_result_adts`.
pub(super) fn register_function_bodies<'a>(
    ctx: &mut LoweringContext<'a>,
    functions: &'a [FunctionInput<'a>],
) {
    for func in functions.iter() {
        // Synthesize a Result<T, E> sum ADT for raising functions. The
        // synthesized result type (when present) becomes both the reserved
        // slot's placeholder return type and the value stored in
        // `function_result_adts`, keyed by this function's reserved BodyId.
        let result_adt_id = synthesize_function_result_adt(ctx, func);
        let return_ty = match result_adt_id {
            Some(id) => MirType::new(MirTypeKind::Adt(id)),
            None => lower_ty(ctx.ty_interner, &ctx.adt_map, func.sig.return_ty),
        };
        let body_id = ctx.program.reserve_body(func.name, func.span, return_ty);

        ctx.function_map.insert(func.binding, body_id);
        ctx.function_sigs.insert(func.binding, func.sig);
        // First registration wins on name collision — the resolver is
        // expected to surface a duplicate-decl diagnostic upstream so we
        // do not double-report it from MIR lowering.
        ctx.function_symbols.entry(func.name).or_insert(func.binding);

        if let Some(result_adt_id) = result_adt_id {
            ctx.function_result_adts.insert(body_id, result_adt_id);
        }
    }
}

/// Synthesize the `Result<T, E>` sum ADT for a raising function, or
/// `None` for a non-raising function (or one whose err payloads do not
/// all resolve to ADTs). Split out of `register_function_bodies` to keep
/// that function under the line cap.
fn synthesize_function_result_adt(
    ctx: &mut LoweringContext<'_>,
    func: &FunctionInput<'_>,
) -> Option<AdtId> {
    let err_tys: Vec<_> = func
        .sig
        .effects
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            EffectEntry::Pure(PureEffect::Err(t)) => Some(*t),
            _ => None,
        })
        .collect();
    if err_tys.is_empty() {
        return None;
    }
    let success_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, func.sig.return_ty);
    let mut err_adts: Vec<(Symbol, AdtId)> = Vec::with_capacity(err_tys.len());
    for err_ty in &err_tys {
        match lower_ty(ctx.ty_interner, &ctx.adt_map, *err_ty).kind {
            MirTypeKind::Adt(err_adt_id) => {
                // Use the err_adt's name if available, else synthesize one.
                let err_name = match ctx.program.program().adts.get(err_adt_id) {
                    Some(def) => def.name,
                    None => ctx.interner.intern("__Err"),
                };
                err_adts.push((err_name, err_adt_id));
            }
            // Non-ADT err type: skip synthesis for this function.
            _ => return None,
        }
    }
    if err_adts.is_empty() {
        return None;
    }
    Some(adt::synthesize_result_adt(ctx, success_ty, err_adts, func.span))
}

/// `edda_types::ParamMode -> mir::ParamMode`. `Default` collapses to `Let`.
pub(super) fn map_param_mode(mode: TypesParamMode) -> ParamMode {
    match mode {
        TypesParamMode::Default => ParamMode::Let,
        TypesParamMode::Mutable => ParamMode::Mutable,
        TypesParamMode::Take => ParamMode::Take,
        TypesParamMode::Init => ParamMode::Init,
    }
}
