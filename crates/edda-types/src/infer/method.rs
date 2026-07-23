//! Method-call resolution + typing — stdlib-flavoured dispatch.
//!
//! `receiver.name(args)` desugars to a free-function call where the
//! receiver becomes the leading positional argument: `name(receiver,
//! args...)`. The free function is found by walking
//! [`crate::cx::TyCx::iter_sigs`] for any function whose declared name
//! matches `name` and whose first parameter type equals the receiver's
//! synthesised type.
//!
//! This is the entry point for method-style calls produced by
//! the parser's `bound-head-call-decompose` rule (`out.print_line(...)`
//! lands here). The stdlib (`stdlib/io/stream/src/stream.ea`,
//! `stdlib/text/str/src/str.ea`) declares free functions whose first parameter
//! is the receiver type — `function read_line(stdin: Stdin) -> String
//! with {stdin, err: IoError}` — so the heuristic resolves uniquely.
//!
//! When the resolution succeeds, the binding id is recorded in
//! [`crate::infer::InferCx::method_resolutions`] keyed by the
//! method-call site's span. The MIR-lowering pass picks this up via
//! [`crate::TypedPackage::method_resolutions`] to emit a regular
//! function `Call` with the receiver prepended.

use edda_intern::Symbol;
use edda_resolve::{BindingId, BindingKind, ModulePath};
use edda_span::Span;
use edda_syntax::ast::Ident;

use crate::capability::CapabilityType;
use crate::effect::{EffectEntry, PureEffect};
use crate::hir::{HirCallArg, HirExpr, HirExprKind};
use crate::intrinsic::{CapabilityMethod, IntrinsicKind, resolve_capability_method};
use crate::prim::Primitive;
use crate::sig::Param;
use crate::ty::{TyId, TyKind};

use super::call::{capability_source, capability_source_of_call, check_arg_expr, synth_arg_expr, translate_entry};
use super::path::check_synth_against_expected;
use super::{InferCx, TyEnv, synth_expr};

/// Synthesise a `receiver.name(args)` method-call expression.
///
/// Dispatch heuristic: find the free function in
/// [`crate::cx::TyCx`] whose declared name equals `name` and whose
/// first parameter type equals the receiver's synthesised type. The
/// stdlib follows the convention `<Type>.<method>(receiver, ...)`, so
/// the heuristic resolves uniquely for the canonical use cases
/// (`stdin.read_line()` → `std.io.read_line(stdin)`, etc.).
///
/// On success: records the resolution in
/// [`InferCx::method_resolutions`] keyed by `span`, type-checks each
/// argument expression against the corresponding `sig.params[1..]`
/// type, applies the same row-union + capability-substitution that
/// `synth_call` does, and returns the function's return type.
///
/// On failure: emits a typecheck diagnostic and returns the error
/// sentinel.
pub(super) fn synth_method_call(
    env: &mut TyEnv,
    receiver: &mut HirExpr,
    name: Ident,
    args: &mut [HirCallArg],
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    // The receiver is the implicit leading argument (params[0]) — an
    // enclosing `?` covers only the method call itself, so a fallible
    // call in receiver position must carry its own `?`
    // (same rule as `check_arg_expr`).
    let saved_try_depth = ic.try_position_depth;
    ic.try_position_depth = 0;
    let receiver_ty = synth_expr(env, receiver, ic);
    ic.try_position_depth = saved_try_depth;
    let recv_ty_for_diag = receiver_ty;
    let method_name = name.name;

    // Check compiler intrinsics BEFORE free-function lookup. Intrinsics
    // bypass the TyCx entirely and lower directly to MIR rvalues.
    let method_str = ic.lower.interner.resolve(method_name);
    if let Some(intrinsic) = resolve_intrinsic(method_str, receiver_ty, ic) {
        ic.intrinsic_calls.insert(span, intrinsic);
        // Intrinsic methods take no user-supplied arguments currently.
        for arg in args.iter_mut() {
            synth_arg_expr(env, arg, ic);
        }
        return intrinsic_result_ty(intrinsic, receiver_ty, ic.lower.ty_interner);
    }

    // Capability-typed method with a comptime `Type` argument
    // (`allocator.alloc_array(T, n)`). The receiver's type is a
    // `CapabilityType`; the `(capability, method)` pair selects a
    // catalogue variant whose first argument is type-checked as the
    // `Type` meta-primitive (the same machinery `size_of(T)` uses) and
    // whose remaining arguments check positionally. The free-function
    // search below has no entry for these methods because the stdlib
    // cannot declare a `comptime T: Type` generic signature today;
    // routing here keeps the source-level surface clean while MIR
    // lowering threads the recorded `Type` through `alloc_family.rs`'s
    // rewrite.
    if let TyKind::Capability(cap) = ic.lower.ty_interner.kind(receiver_ty)
        && let Some(method) = resolve_capability_method(*cap, method_str)
    {
        return synth_capability_method_call(env, receiver, method, args, ic, span);
    }

    // Find a free function whose first param type equals the receiver
    // type AND whose declared name matches the method name. Stdlib
    // follows the convention so this resolves uniquely; if ambiguity
    // ever arises the first match wins (deterministic for the
    // stdlib + user-package layout).
    let resolution = resolve_method(ic, method_name, receiver_ty);
    let Some((binding_id, params, return_ty, declared_effects)) = resolution else {
        emit_method_unresolved(ic, name, recv_ty_for_diag, span);
        // Still walk the arguments so embedded diagnostics surface.
        for arg in args.iter_mut() {
            synth_arg_expr(env, arg, ic);
        }
        return ic.ty_error();
    };

    // Record the resolution so MIR-lowering can desugar this site
    // into a regular `Call` against the resolved binding.
    ic.method_resolutions.insert(span, binding_id);

    // Arity: receiver counts as one positional, so user-supplied args
    // fill positions 1..param_count.
    let param_count = params.len();
    let user_arg_count = param_count.saturating_sub(1);
    if args.len() != user_arg_count {
        let name_text = ic.lower.interner.resolve(method_name).to_string();
        ic.emit_typecheck_error(
            span,
            format!(
                "method `{name_text}` takes {user_arg_count} argument(s) but {} were supplied",
                args.len(),
            ),
        );
        for arg in args.iter_mut() {
            synth_arg_expr(env, arg, ic);
        }
        return ic.ty_error();
    }

    // Walk every argument against the declared param type at position i+1.
    let param_tys: Vec<TyId> = params.iter().skip(1).map(|p| p.ty).collect();
    for (arg, param_ty) in args.iter_mut().zip(param_tys.iter()) {
        check_arg_expr(env, arg, *param_ty, ic);
    }

    // A `take self` receiver (`task.await`, `handle.wait()`, …) moves
    // the receiver into the callee — consume a bare `linear` receiver so
    // the scope-exit sweep does not read the hand-off as a leak. The
    // receiver carries no explicit `take` keyword, so the §4 call-mode
    // transition never fires on it.
    if matches!(params.first().map(|p| p.mode), Some(crate::sig::ParamMode::Take)) {
        super::mode::consume_moved_linear(env, receiver, ic);
    }

    // Effect-row contribution: the row union rule from `T-MethodCall`
    // (mirrors `T-FunCall` per `inference-rules.md §1a.4`). Capability
    // entries are rewritten through the unified `call::translate_entry`
    // — passing `Some(receiver)` threads the method-call's receiver as
    // the implicit `params[0]` slot so `box.unbox(mutable allocator)`
    // (callee row `{allocator}`) propagates as `Capability(allocator)`
    // from the caller, not `Capability(box)` from the receiver.
    let mut translated_entries: Vec<EffectEntry> =
        Vec::with_capacity(declared_effects.entries().len());
    for entry in declared_effects.entries() {
        let translated = translate_entry(
            *entry,
            &params,
            Some(receiver),
            args,
            &ic.capability_aliases,
        );
        ic.push_effect_entry(translated);
        translated_entries.push(translated);
    }
    // B22: a statement-position method call whose row carries `err: T`
    // must still be in `?` position or inside a `handle` body — the
    // same rule as direct function calls. See [`super::call::check_err_propagation`].
    super::call::check_err_propagation(&translated_entries, method_name, span, ic);

    return_ty
}

/// Find the free function whose declared name equals `method_name` and
/// whose first parameter type equals `receiver_ty`. Returns the
/// binding id, the function's parameter count, its return type, and a
/// clone of its declared effect row (snapshotted so the caller can
/// release the borrow on `ic` before mutating through it).
fn resolve_method(
    ic: &InferCx<'_>,
    method_name: Symbol,
    receiver_ty: TyId,
) -> Option<(BindingId, Vec<Param>, TyId, crate::effect::EffectRow)> {
    let package = ic.lower.package?;
    for (binding_id, sig) in ic.ty_cx.iter_sigs() {
        let entry = package.binding(binding_id);
        if entry.name != method_name {
            continue;
        }
        let Some(first_param) = sig.params.first() else {
            continue;
        };
        if first_param.ty != receiver_ty {
            continue;
        }
        return Some((
            binding_id,
            sig.params.iter().cloned().collect(),
            sig.return_ty,
            sig.effects.clone(),
        ));
    }
    None
}

/// Check whether (`method_name`, `receiver_ty`) matches a compiler intrinsic.
///
/// Returns the matching [`IntrinsicKind`] when:
/// - `"bytes"` is called on a `String` receiver, or
/// - `"len"` is called on any slice `[T]` receiver.
fn resolve_intrinsic(
    method_name: &str,
    receiver_ty: TyId,
    ic: &InferCx<'_>,
) -> Option<IntrinsicKind> {
    match method_name {
        "bytes" => {
            if let TyKind::Primitive(Primitive::String) = ic.lower.ty_interner.kind(receiver_ty) {
                return Some(IntrinsicKind::StringBytes);
            }
        }
        "len" => {
            if let TyKind::Slice(_) = ic.lower.ty_interner.kind(receiver_ty) {
                return Some(IntrinsicKind::SliceLen);
            }
        }
        _ => {}
    }
    None
}

/// Return the result type produced by an intrinsic method call.
/// `DiscriminantOf` never reaches here (it is call-shaped, typed by
/// `infer::call::synth_discriminant_of_call`); its arm exists only to
/// keep the match exhaustive.
fn intrinsic_result_ty(
    intrinsic: IntrinsicKind,
    _receiver_ty: TyId,
    interner: &crate::ty::TyInterner,
) -> TyId {
    match intrinsic {
        IntrinsicKind::StringBytes => {
            let u8_id = interner.prim(Primitive::U8);
            interner.slice(u8_id)
        }
        IntrinsicKind::SliceLen | IntrinsicKind::DiscriminantOf => {
            interner.prim(Primitive::Usize)
        }
    }
}

/// Push a typecheck diagnostic when no free function with a
/// matching name + receiver-type-first-param shape was found.
fn emit_method_unresolved(
    ic: &mut InferCx<'_>,
    name: Ident,
    receiver_ty: TyId,
    span: Span,
) {
    let name_text = ic.lower.interner.resolve(name.name).to_string();
    let recv_text = ic.display_ty(receiver_ty);
    ic.emit_typecheck_error(
        span,
        format!(
            "no method `{name_text}` resolves on receiver of type `{recv_text}`; \
             declare a free function `{name_text}(<receiver-type>, ...)` in scope",
        ),
    );
}

/// Type-check a `<capability>.alloc_array(T, n)`-style method call.
///
/// Dispatched by [`synth_method_call`] when the receiver's type is a
/// [`crate::CapabilityType`] and the `(capability, method-name)` pair
/// matches the locked catalogue
/// (see [`crate::resolve_capability_method`]). The first argument
/// type-checks as the `Type` meta-primitive (the `synth_path` comptime
/// branch records the resolved `TyId` on `ic.comptime_type_paths`);
/// remaining arguments check positionally against the synthesised
/// signature; the call's row gains the receiver's capability source plus
/// the method's err entry (when the package imports `std.mem.alloc`).
fn synth_capability_method_call(
    env: &mut TyEnv,
    receiver: &mut HirExpr,
    method: CapabilityMethod,
    args: &mut [HirCallArg],
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    let method_name = method.method_name();
    let arity = capability_method_arity(method);

    if args.len() != arity {
        for arg in args.iter_mut() {
            synth_arg_expr(env, arg, ic);
        }
        ic.emit_typecheck_error(
            span,
            format!(
                "method `{method_name}` takes {arity} argument(s) but {found} were supplied",
                found = args.len()
            ),
        );
        return ic.ty_error();
    }

    // Reject named-payload / mode-decorated arguments — capability-
    // method calls follow the regular positional-call shape.
    for arg in args.iter() {
        if let Some(field) = &arg.name {
            let field_text = ic.lower.interner.resolve(field.name).to_string();
            ic.emit_typecheck_error(
                arg.span,
                format!(
                    "named argument `{field_text}:` is not admitted at capability-method \
                     call site `{method_name}`"
                ),
            );
        }
        if arg.mode.is_some() {
            ic.emit_typecheck_error(
                arg.span,
                format!(
                    "call-site mode keyword is not admitted at capability-method \
                     call site `{method_name}`"
                ),
            );
        }
    }

    match method {
        CapabilityMethod::AllocatorAllocArray => synth_alloc_array(env, receiver, args, ic, span),
        CapabilityMethod::AllocatorFork => synth_allocator_fork(receiver, ic, span),
        CapabilityMethod::RandomDeterministic => {
            synth_random_deterministic(env, receiver, args, ic, span)
        }
    }
}

/// Number of user-supplied positional arguments expected by a
/// capability-method variant. Receiver does not count.
fn capability_method_arity(method: CapabilityMethod) -> usize {
    match method {
        CapabilityMethod::AllocatorAllocArray => 2,
        CapabilityMethod::AllocatorFork => 0,
        CapabilityMethod::RandomDeterministic => 1,
    }
}

/// Type-check `<allocator>.fork() -> Allocator`.
fn synth_allocator_fork(receiver: &mut HirExpr, ic: &mut InferCx<'_>, span: Span) -> TyId {
    ic.capability_method_calls
        .insert(span, CapabilityMethod::AllocatorFork);

    let receiver_src = capability_source(receiver).or_else(|| capability_source_of_call(receiver));
    if let Some(src) = receiver_src {
        let resolved = ic.capability_aliases.get(&src).copied().unwrap_or(src);
        ic.push_effect_entry(EffectEntry::Capability(resolved));
    }

    ic.lower
        .ty_interner
        .capability(crate::capability::CapabilityType::Allocator)
}

/// Type-check `<random>.deterministic(seed: u64) -> DeterministicRandom`.
fn synth_random_deterministic(
    env: &mut TyEnv,
    receiver: &mut HirExpr,
    args: &mut [HirCallArg],
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    let u64_ty = ic.lower.ty_interner.prim(Primitive::U64);
    check_arg_expr(env, &mut args[0], u64_ty, ic);

    ic.capability_method_calls
        .insert(span, CapabilityMethod::RandomDeterministic);

    let receiver_src = capability_source(receiver).or_else(|| capability_source_of_call(receiver));
    if let Some(src) = receiver_src {
        let resolved = ic.capability_aliases.get(&src).copied().unwrap_or(src);
        ic.push_effect_entry(EffectEntry::Capability(resolved));
    }

    ic.lower
        .ty_interner
        .capability(crate::capability::CapabilityType::DeterministicRandom)
}

/// Type-check `<allocator>.alloc_array(T, n) -> [T]`.
fn synth_alloc_array(
    env: &mut TyEnv,
    receiver: &mut HirExpr,
    args: &mut [HirCallArg],
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    // First arg: `Type` meta — walks through synth so `synth_path`'s
    // comptime branch records the resolved `TyId` on
    // `ic.comptime_type_paths`. Toggle `in_comptime_context` for the
    // walk so a single-segment primitive name is recognised as a
    // type-as-value rather than a missing binding.
    let arg0_span = args[0].expr.span;
    let prev_comptime = ic.in_comptime_context;
    ic.in_comptime_context = true;
    let arg0_synth = synth_expr(env, &mut args[0].expr, ic);
    ic.in_comptime_context = prev_comptime;
    let type_meta = ic.lower.ty_interner.prim(Primitive::Type);
    check_synth_against_expected(arg0_synth, type_meta, ic, arg0_span);

    // Recover the user-supplied `TyId` from the comptime-type-paths
    // map. `synth_path` records the concrete handle keyed by the path
    // span. When the arg is not a bare path (or the comptime branch
    // failed), fall back to the error sentinel; the result type
    // becomes `[error]` and downstream sites cascade silently.
    let element_ty = match &args[0].expr.kind {
        HirExprKind::Path(path) => ic
            .comptime_type_paths
            .get(&path.span)
            .copied()
            .unwrap_or_else(|| ic.ty_error()),
        _ => {
            ic.emit_typecheck_error(
                arg0_span,
                "first argument to `alloc_array` must be a bare type name (e.g. `u32`, `MyType`)",
            );
            ic.ty_error()
        }
    };

    // Second arg: `usize` count.
    let usize_ty = ic.lower.ty_interner.prim(Primitive::Usize);
    check_arg_expr(env, &mut args[1], usize_ty, ic);

    // Record the dispatch for MIR lowering.
    ic.capability_method_calls
        .insert(span, CapabilityMethod::AllocatorAllocArray);

    // Effect row: the receiver's capability source (so callers see
    // `Capability(allocator)` against their own parameter) and the
    // `err: alloc.AllocError` propagation entry. The capability
    // source rule mirrors `call::translate_entry`'s
    // capability-substitution logic.
    let receiver_src = capability_source(receiver).or_else(|| capability_source_of_call(receiver));
    if let Some(src) = receiver_src {
        let resolved = ic.capability_aliases.get(&src).copied().unwrap_or(src);
        ic.push_effect_entry(EffectEntry::Capability(resolved));
    }

    let mut row_entries: Vec<EffectEntry> = Vec::new();
    if let Some(err_ty) = lookup_alloc_error_ty(ic) {
        let entry = EffectEntry::Pure(PureEffect::Err(err_ty));
        ic.push_effect_entry(entry);
        row_entries.push(entry);
    }
    // B22: a statement-position call carrying `err: T` must still be in
    // `?` propagator position or inside a handler — same rule as direct
    // function calls.
    super::call::check_err_propagation(&row_entries, ic.lower.interner.intern("alloc_array"), span, ic);

    // Result: `[T]`.
    ic.lower.ty_interner.slice(element_ty)
}

/// Look up `std.mem.alloc.AllocError` through the active resolved
/// package and return its `TyId` as a nominal handle. Returns `None`
/// when the package has no `std.mem.alloc` module (the user hasn't
/// imported it) or the module has no `AllocError` binding.
fn lookup_alloc_error_ty(ic: &InferCx<'_>) -> Option<TyId> {
    let package = ic.lower.package?;
    let std_sym = ic.lower.interner.intern("std");
    let mem_sym = ic.lower.interner.intern("mem");
    let alloc_sym = ic.lower.interner.intern("alloc");
    let alloc_error_sym = ic.lower.interner.intern("AllocError");
    let canonical_path = ModulePath::new(vec![std_sym, mem_sym, alloc_sym]);
    let module_id = package.graph().lookup_by_path(&canonical_path)?;
    let resolved_module = package.module(module_id);
    let binding_id = resolved_module.items.lookup(alloc_error_sym)?;
    let binding_entry = package.binding(binding_id);
    if binding_entry.kind != BindingKind::TypeDecl {
        return None;
    }
    Some(ic.lower.ty_interner.intern_kind(TyKind::Nominal(binding_id)))
}
