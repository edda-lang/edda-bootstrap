//! Path-expression lowering for the typed-HIR -> MIR pass.
//!
//! A path in value position is one of: a single-segment local binding;
//! a module-level function used as a value (single- or multi-segment,
//! lowered to the fat `{ code, env }` fn-value via a synthesised
//! forwarding shim); a module-qualified `let` constant; or a
//! unit-payload variant constructor. Split out of `expr/mod.rs` to keep
//! both files under the 500-line cap.

use edda_span::Span;

use crate::builder::BodyBuilder;
use crate::error::{LoweringError, MirError};
use crate::ids::{BodyId, EffectId, LocalId};
use crate::operand::Operand;
use crate::place::Place;
use crate::rvalue::{Rvalue, RvalueKind};
use crate::terminator::{CallArg, CallMode, FuncRef};
use crate::ty::{FnSig, MirPrim, MirType, MirTypeKind, ParamMode};

use super::super::cfg::push_assign;
use super::super::ctx::LoweringContext;
use edda_types::HirExpr;

/// Lower a path expression — single-segment local / fn-value / const,
/// or multi-segment module-qualified fn-value / const / unit-variant
/// constructor.
pub(super) fn lower_path(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    path: &edda_types::HirPath,
) -> Operand {
    if path.segments.len() == 1 {
        let name = path.segments[0].name;
        if let Some(local) = ctx.bindings.get(&name) {
            return Operand::Copy(Place::local(*local));
        }
        // Single-segment path that isn't a local binding may name a
        // module-level function used as a value (`let h = f`). The
        // resolver records `BindingKind::Function`; we look the
        // declared binding up in `function_map` to retrieve its
        // `BodyId`, allocate a fresh temp typed as the body's fn-ptr,
        // and emit `Assign { temp = FunctionRef(body) }`.
        if let Some(fn_ptr_operand) = lower_function_ref_by_path(ctx, expr, path) {
            return fn_ptr_operand;
        }
        // Or it may name a module-level `let` constant. The driver's
        // `register_consts` pre-pass interned each `let` initialiser
        // into `program.consts` and recorded the `BindingId -> ConstId`
        // in `ctx.module_consts`. Resolve the path's binding through
        // `resolutions` and emit `Operand::Const(id)` when present.
        if let Some(operand) = lower_const_ref_by_path(ctx, path) {
            return operand;
        }
        ctx.errors.push(MirError::from(LoweringError::UnknownBinding {
            name,
            span: expr.span,
        }));
        return Operand::Unit;
    }
    // Multi-segment value-position Path. A module-qualified function
    // name used as a value — `<leaf>.<fn>` (e.g. `expr_mod.parse_expr`
    // passed as an argument) — lowers to the SAME fat fn-value as the
    // single-segment form. The resolver's
    // `resolve_cross_module_item` already recorded the function
    // `BindingId` for the full path span, and the typechecker's
    // `synth_path` already typed the expression `FnPtr` regardless of
    // segment count; `lower_function_ref_by_path` keys off the same span
    // and is itself segment-agnostic, so the named-fn-value path is
    // reused verbatim. It returns `None` for a non-function binding
    // (a variant-constructor TypeDecl is absent from `function_map`),
    // so the variant-constructor path below still runs for those.
    if let Some(fn_ptr_operand) = lower_function_ref_by_path(ctx, expr, path) {
        return fn_ptr_operand;
    }
    // A module-qualified `let` constant (`<leaf>.<CONST>`) lowers to the
    // pre-interned `Operand::Const` the same way the single-segment form
    // does; `lower_const_ref_by_path` is likewise segment-agnostic and
    // returns `None` for any non-`Const` binding.
    if let Some(operand) = lower_const_ref_by_path(ctx, path) {
        return operand;
    }
    // Otherwise the only remaining currently-admitted shape that produces a value
    // here is a unit-payload variant constructor — `<Type>.<variant>`
    // (and the cross-module form `<leaf>.<Type>.<variant>`). The resolver
    // records the TypeDecl binding for the full path span; the
    // typechecker has already proven the variant exists and is
    // unit-payload (payload variants are routed through `synth_call`). We
    // mirror that proof here: resolve to TypeDecl, confirm sum + variant
    // + Unit payload, and emit `MakeVariant` with an empty field list.
    super::super::variant_ctor::lower_unit_variant_constructor(ctx, expr, path)
}

/// Lower a path that names a module-level body function used as a value.
/// Segment-agnostic: handles both the single-segment form (`let h = f`)
/// and the module-qualified multi-segment form (`let h = mod.f`,
/// `pass(mod.f)`), because the resolver
/// records the function `BindingId` under the full `path.span` for both.
/// Looks the path up through the package's resolutions → `BindingId` →
/// `function_map` → `BodyId`, synthesises a forwarding shim whose leading
/// parameter is the env word, and emits a `FunctionRef(shim)` rvalue
/// typed as the fat fn-value.
///
/// Returns `None` when the path resolves to anything else (a TypeDecl,
/// a Module, etc.), so the caller can fall through to its existing error
/// path. An extern-bodied function reports `ExternFnValueUnsupported`
/// directly rather than falling through. A
/// capability-bearing or panicking named function materialises (the
/// shim declares the same capability slots and forwards them; panic is
/// a call-site no-op). A raising (`err:`) named function also
/// materialises: the fn-ptr ret is projected to the callee's wire-level
/// `Result<T, E>` ADT (see [`overwrite_raising_fn_ptr_ret`]) so the
/// `?`-propagating indirect call reads the Result-shaped value the shim
/// tail-forwards.
fn lower_function_ref_by_path(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    path: &edda_types::HirPath,
) -> Option<Operand> {
    use edda_resolve::Resolved;
    let resolved = ctx.resolutions.lookup_path(path.span)?;
    let binding_id = match resolved {
        Resolved::Binding(id) => id,
        Resolved::Module(_) | Resolved::Error => return None,
    };
    let Some(body_id) = ctx.function_map.get(&binding_id).copied() else {
        // The typechecker's `synth_path` (edda_types::infer::path) does not
        // distinguish source-bodied from extern-bodied functions when
        // synthesising a `FnPtr` type for a function name in value
        // position — both share `BindingKind::Function`. An extern
        // binding therefore reaches here as a genuine `FnPtr`-typed
        // expression rather than failing earlier. Report the precise gap
        // instead of falling through to
        // the generic `UnknownBinding` a caller would otherwise see for a
        // perfectly well-resolved name.
        if ctx.function_externs.contains_key(&binding_id) {
            ctx.errors.push(MirError::from(LoweringError::ExternFnValueUnsupported {
                name: path.segments.last().expect("HirPath has >= 1 segment").name,
                span: expr.span,
            }));
            return Some(Operand::Unit);
        }
        return None;
    };

    // Build the fn-ptr MirType from the *typechecker's* projection of
    // this path expression. The expression's HIR type was set to
    // `TyKind::FnPtr(sig)` by the typechecker's path-in-value-position
    // rule (`edda_types::infer::path`), and `lower_ty` reads that
    // through to `MirTypeKind::FnPtr(sig)`.
    let fn_ptr_ty = crate::lower::ty::lower_ty(ctx.ty_interner, &ctx.adt_map, expr.ty);
    let MirTypeKind::FnPtr(mir_sig) = &fn_ptr_ty.kind else {
        debug_assert!(
            false,
            "lower_function_ref_by_path called with non-FnPtr HIR type {:?}",
            expr.ty,
        );
        return None;
    };
    let mut mir_sig: FnSig = (**mir_sig).clone();

    // Raising named-fn value: the fn-ptr
    // type projection (`super::ty::lower_fn_ptr_sig`) leaves `ret` as the
    // bare success type `T` with the error payload in `may_raise`, but the
    // real callee body returns the synthesised wire-level `Result<T, E>`
    // ADT (`register_function_bodies` populated
    // `ctx.function_result_adts[body_id]`). Overwrite the projected `ret`
    // with that Result ADT *here* — the seam where `ctx` (and so the
    // result-ADT registry + program builder) is reachable; `lower_fn_ptr_sig`
    // is a pure transcription with no `&mut program` and cannot do it. The
    // fat value's `FnPtr` ret, the shim's ret, and the shim's call
    // destination all then carry the identical Result wire shape, so the
    // `?`-propagating indirect call (`edda-compile` `lower_indirect_call`,
    // which reads `sig.ret` directly) needs
    // no change. Capability-bearing / panicking values already materialised
    // before this slice (their `ret` was already correct).
    let fn_ptr_ty = if mir_sig.may_raise.is_empty() {
        fn_ptr_ty
    } else {
        overwrite_raising_fn_ptr_ret(ctx, body_id, &mut mir_sig, fn_ptr_ty)
    };

    let shim_id = synthesise_fn_value_shim(ctx, body_id, &mir_sig, expr.span)?;

    let body_builder = ctx.body.as_mut()?;
    let dest_local = body_builder.temp(fn_ptr_ty.clone(), expr.span);
    push_assign(
        ctx,
        expr.span,
        dest_local,
        Rvalue {
            span: expr.span,
            kind: RvalueKind::FunctionRef(shim_id),
            ty: fn_ptr_ty,
        },
    );
    Some(Operand::Move(Place::local(dest_local)))
}

/// Reconcile a raising named-fn value's `FnSig::ret` (and the matching
/// `FnPtr` MirType) with the wire-level `Result<T, E>` ADT the real callee
/// body returns. Looks `body_id` up in the already-populated
/// `ctx.function_result_adts` registry — never re-synthesises the ADT.
fn overwrite_raising_fn_ptr_ret(
    ctx: &LoweringContext<'_>,
    body_id: BodyId,
    sig: &mut FnSig,
    fn_ptr_ty: MirType,
) -> MirType {
    let Some(&result_adt_id) = ctx.function_result_adts.get(&body_id) else {
        return fn_ptr_ty;
    };
    sig.ret = MirType::new(MirTypeKind::Adt(result_adt_id));
    MirType::new(MirTypeKind::FnPtr(Box::new(sig.clone())))
}

/// Synthesise a forwarding shim
/// `shim(caps…, env, args…) { return real(caps…, args…) }` for a named
/// function used as a value, push it onto the program, and return its
/// [`BodyId`]. The shim gives the named function the leading-env-param
/// shape the unified fat fn-value ABI requires (capabilities precede the
/// env word, matching the indirect-call argument layout), without
/// changing the function's own calling convention for direct calls.
fn synthesise_fn_value_shim(
    ctx: &mut LoweringContext<'_>,
    real_body_id: BodyId,
    sig: &FnSig,
    span: Span,
) -> Option<BodyId> {
    let name = ctx.interner.intern("<fnval-shim>");
    let mut builder = BodyBuilder::new(name, span, sig.ret.clone());
    let _ret_slot = builder.return_slot(sig.ret.clone(), span);

    // Capability slots come first (before the env word), satisfying the
    // `BodyBuilder` capability-first invariant and the indirect-call arg
    // layout `[capabilities…, env, params…]`. Each slot's `EffectId` is
    // forwarded into the inner call below so the callee receives the same
    // capability pointers the indirect call site threaded in.
    let mut cap_effect_ids: Vec<EffectId> = Vec::with_capacity(sig.capabilities.len());
    for kind in &sig.capabilities {
        let cap_ty = MirType::new(MirTypeKind::Capability(kind.clone()));
        cap_effect_ids.push(builder.add_capability(kind.clone(), cap_ty, span));
    }
    // `may_panic` is recorded on the shim's own effect row for fidelity;
    // it needs no call-site work (a panicking callee aborts via its own
    // `Panic` terminator).
    if sig.may_panic {
        builder.set_panic(true);
    }

    // Leading env word — ignored by the shim, present so the fat-value
    // ABI is uniform with closures. Sits after the capability slots.
    let _env = builder.param(ParamMode::Let, MirType::prim(MirPrim::Usize), span);
    let mut arg_locals: Vec<(LocalId, CallMode)> = Vec::with_capacity(sig.params.len());
    for (mode, ty) in &sig.params {
        let local = builder.param(*mode, ty.clone(), span);
        arg_locals.push((local, CallMode::from_param_mode(*mode)));
    }
    let dest = builder.temp(sig.ret.clone(), span);

    // Return block (reserved + sealed first; it does not depend on the
    // call block).
    let ret_block = builder.block();
    let ret_block_id = ret_block.id();
    ret_block.return_(span, Operand::Move(Place::local(dest)));

    // Entry block: forward every user param to the real body (threading
    // the shim's capability slots), then go to the return block.
    let call_block = builder.block();
    let call_block_id = call_block.id();
    let call_args: Vec<CallArg> = arg_locals
        .iter()
        .map(|(local, mode)| CallArg {
            mode: *mode,
            operand: Operand::Move(Place::local(*local)),
        })
        .collect();
    call_block.call(
        span,
        FuncRef::Body(real_body_id),
        call_args,
        cap_effect_ids,
        Place::local(dest),
        ret_block_id,
        None,
    );
    builder.set_entry(call_block_id);

    Some(ctx.program.push_body(builder.finish()))
}

/// Lower a single-segment path that resolves to a module-level
/// `let` constant. Looks the path up through the package's
/// resolutions → `BindingId` → `module_consts` → `ConstId`. The
/// pre-pass `super::register::register_consts` is responsible for
/// interning each constant's value; references that arrive here only
/// need to emit an `Operand::Const` naming the pre-interned slot.
fn lower_const_ref_by_path(
    ctx: &LoweringContext<'_>,
    path: &edda_types::HirPath,
) -> Option<Operand> {
    use edda_resolve::Resolved;
    let resolved = ctx.resolutions.lookup_path(path.span)?;
    let binding_id = match resolved {
        Resolved::Binding(id) => id,
        Resolved::Module(_) | Resolved::Error => return None,
    };
    let const_id = ctx.module_consts.get(&binding_id).copied()?;
    Some(Operand::Const(const_id))
}
