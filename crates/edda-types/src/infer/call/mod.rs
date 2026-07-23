//! Function-call inference.
//!
//! Implements **T-FunCall** from `inference-rules.md §1a.4`:
//!
//! ```text
//!    Γ ⊢ f ⇒ function(P₁, …, Pₙ) → T uses Rᶠ
//!    (Γ ⊢ aᵢ ⇐ Pᵢ uses Rᵢ)ᵢ₌₁ⁿ
//!    ──────────────────────────────────────────  (T-FunCall)
//!    Γ ⊢ f(a₁, …, aₙ) ⇒ T uses Rᶠ ∪ ⋃ᵢ Rᵢ
//! ```
//!
//! Only direct named-function calls are admitted (no first-class
//! function values per `docs/syntax/types.md`, *Function types*). The
//! callee must therefore be a `HirExprKind::Path` whose resolution
//! points to a `BindingKind::Function`.
//!
//! The row union: after every argument is checked,
//! every entry in the callee's declared `effects` row is pushed into
//! the caller's accumulator, refined for
//! capability entries: per `effect-tracking.md §2`, *capability rows
//! name parameters held, not derived bindings*. A callee entry
//! `Capability(p)` where `p` is the callee's parameter is rewritten
//! at the call site to `Capability(q)`, where `q` is the caller-side
//! parameter (or derivation source) the argument flowed from. Pure
//! entries pass through unchanged — qualified-name match handles
//! them.
//!
//! # Module layout
//!
//! - [`args`] — call-argument typing helpers + the `init`-mode read-bypass.
//! - [`capability`] — call-site capability-row substitution (`§2`).
//! - [`comptime_purity`] — the P-CompTimePure call-site gate.
//! - [`err_propagation`] — the B22 `err: T` propagation gate.
//! - [`indirect`] — fn-pointer (indirect) call inference.
//! - [`variant`] — sum-type variant constructor inference.

mod args;
mod capability;
mod comptime_purity;
mod err_propagation;
mod indirect;
mod variant;

use edda_intern::Symbol;
use edda_resolve::{BindingKind, Resolved};
use edda_span::Span;

use crate::effect::EffectEntry;
use crate::hir::{HirCallArg, HirExpr, HirExprKind};
use crate::sig::Param;
use crate::ty::TyId;

use super::{InferCx, TyEnv};

pub(super) use args::{check_arg_expr, synth_arg_expr};
pub(super) use capability::{capability_source, capability_source_of_call, translate_entry};
pub(super) use comptime_purity::check_comptime_purity;
pub(super) use err_propagation::check_err_propagation;

use args::synth_args_for_diagnostics;
use indirect::synth_indirect_call;
use variant::synth_variant_constructor;

/// Synthesise a `callee(args)` function-call expression.
///
/// Two callable shapes are admitted per the locked spec:
///
/// 1. **Function call** — `f(arg, arg, ...)`. Arguments are positional
///    (`expressions.md` §521); a `CallArg.name` is rejected here.
/// 2. **Variant constructor** — `Type.variant(field: expr, ...)` for
///    struct-payload variants or `Type.variant(expr, ...)` for
///    tuple-payload variants (`declarations.md` §252 + §221).
///
/// The function-call side (rule 1) is implemented. The variant-
/// constructor side (rules 2-3) is deferred: it requires recognising
/// the `Type.variant` callee shape against a sum-type declaration in
/// [`crate::TyCx`], looking up the declared payload fields, and
/// reordering / validating named arguments against the declaration.
/// Until that lands, variant-constructor call sites surface as the
/// existing "cannot call type" / "no recorded resolution" diagnostics
/// from the resolver and from this function's path-binding lookup.
///
/// `[TODO]` — variant-constructor call validation per the
/// rules above.
pub(super) fn synth_call(
    env: &mut TyEnv,
    callee: &mut HirExpr,
    args: &mut [HirCallArg],
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    // Three call shapes:
    //   1. Direct named-function call: callee is a `Path` resolving to a
    //      `BindingKind::Function`. Existing rule, handled below.
    //   2. Sum-type variant constructor: callee is a multi-segment
    //      `Path` resolving to a `BindingKind::TypeDecl` with a sum
    //      shape. Handled below.
    //   3. Indirect fn-ptr call: any other callee whose synthesised
    //      type is `TyKind::FnPtr`. Routes to `synth_indirect_call`.
    //
    // Cases 1 + 2 require a Path; everything else either falls through
    // to case 3 (when its type is FnPtr) or surfaces a precise
    // diagnostic.
    let path = match &callee.kind {
        HirExprKind::Path(p) => p.clone(),
        _ => return synth_indirect_call(env, callee, args, ic, span),
    };
    let path_span = path.span;
    callee.ty = ic.ty_error();

    // Comptime built-in call (§C10 — `04-specs-comptime.md` *Comptime
    // built-ins* + `06-tooling.md` §9 `target_has`). The resolver does
    // not record catalogue-head paths in its `resolutions` map; without
    // this short-circuit the package lookup below would emit
    // `callee path \`size_of\` has no recorded resolution`. Inside a
    // `comptime` body the call's surface type is `usize` (size_of /
    // align_of) or `bool` (target_has). `offset_of` is locked but its
    // dispatch is still wave-deferred; we accept the call-site so the
    // user gets the "offset_of not yet supported" diagnostic from the
    // evaluator rather than the resolver's bare "no resolution".
    if ic.in_comptime_context
        && path.segments.len() == 1
        && let Some(builtin) = crate::comptime_builtin::comptime_builtin_for_name(
            ic.lower.interner.resolve(path.segments[0].name),
        )
    {
        return super::comptime_call::synth_comptime_builtin_call(
            env, &path, builtin, args, ic, span,
        );
    }

    // Dotted `target.supports(Cap)` comptime built-in —
    // the lone two-segment built-in.
    // The resolver stays silent on the `target.` namespace (see
    // `edda-resolve` paths.rs), so route the call here before the
    // package lookup, mirroring the single-segment branch above. Typed
    // `bool`; the capability type-arg is checked in
    // `synth_target_supports_call`.
    if ic.in_comptime_context
        && path.segments.len() == 2
        && ic.lower.interner.resolve(path.segments[0].name) == "target"
        && ic.lower.interner.resolve(path.segments[1].name) == "supports"
    {
        return super::comptime_call::synth_target_supports_call(env, &path, args, ic, span);
    }

    // `discriminant_of(v)` runtime intrinsic — the D-22 sum-type READ
    // surface. The resolver stays silent on the name (it is a
    // catalogue head), so route the call here before the package
    // lookup, mirroring the comptime-builtin branch above — but NOT
    // gated on `in_comptime_context`: the discriminant is a runtime
    // read. Typed `usize`; records [`crate::IntrinsicKind::DiscriminantOf`]
    // keyed by the call span so MIR lowering emits `ExtractTag` + a
    // widening `Cast` instead of a `Call` terminator.
    if path.segments.len() == 1
        && ic.lower.interner.resolve(path.segments[0].name) == "discriminant_of"
    {
        return synth_discriminant_of_call(env, args, ic, span);
    }

    // Primitive-headed static-method call. The resolver returns
    // `Resolved::Error` for catalogue-headed paths and stays silent —
    // without this short-circuit the package lookup below would emit a
    // spurious `callee path ... has no recorded resolution` for a call
    // the catalogue is meant to own. The locked catalogue (currently
    // empty) lives in
    // [`crate::intrinsic::resolve_primitive_static_method`]; MIR
    // lowering reads `primitive_static_calls` to emit a `Call` against
    // the variant's `__edda_*` runtime extern.
    if path.segments.len() == 2
        && let Some(method) = crate::resolve_primitive_static_method(
            ic.lower.interner.resolve(path.segments[0].name),
            ic.lower.interner.resolve(path.segments[1].name),
        )
    {
        return super::primitive_static_call::synth_primitive_static_method_call(
            env, &path, method, args, ic, span,
        );
    }

    // Resolve the callee via the package's `Resolutions` map.
    let package = match ic.lower.package {
        Some(p) => p,
        None => {
            synth_args_for_diagnostics(env, args, ic);
            ic.emit_typecheck_error(
                span,
                "function calls require a resolved-package context",
            );
            return ic.ty_error();
        }
    };
    let (binding_id, entry_name, entry_kind) =
        match package.resolutions().lookup_path(path_span) {
            Some(Resolved::Binding(id)) => {
                let entry = package.binding(id);
                (id, entry.name, entry.kind)
            }
            Some(Resolved::Module(_)) => {
                synth_args_for_diagnostics(env, args, ic);
                ic.emit_typecheck_error(span, "cannot call a module");
                return ic.ty_error();
            }
            Some(Resolved::Error) => {
                synth_args_for_diagnostics(env, args, ic);
                return ic.ty_error();
            }
            None => {
                // Mono-specialization fallback:
                // the outbound-generic pass rewrites template call
                // sites to single-segment mangled names whose path
                // span is deliberately unresolved. The mangled names
                // contain `$`, which user identifiers cannot, so the
                // table hit is unambiguous.
                if path.segments.len() == 1
                    && let Some(mf) = ic.mono_fns
                    && let Some(&binding) = mf.by_name.get(&path.segments[0].name)
                {
                    return synth_resolved_function_call(
                        env,
                        binding,
                        path.segments[0].name,
                        args,
                        ic,
                        span,
                    );
                }
                let path_text = path
                    .segments
                    .iter()
                    .map(|s| ic.lower.interner.try_resolve(s.name).unwrap_or("<missing>"))
                    .collect::<Vec<_>>()
                    .join(".");
                synth_args_for_diagnostics(env, args, ic);
                ic.emit_typecheck_error(
                    span,
                    format!("callee path `{path_text}` has no recorded resolution"),
                );
                return ic.ty_error();
            }
        };

    // Sum-type variant constructor call: `<Type>.<variant>(args)`.
    // The callee path resolves to a `BindingKind::TypeDecl` (user-
    // declared sum) or `BindingKind::SpecInvocation` (spec-instantiated
    // sum, e.g. `Option_i32`) whose layout is a sum
    // (`declarations.md` §252 + §221). The variant name is the last
    // path segment. Tuple-payload variants take positional args;
    // struct-payload variants take named args.
    if matches!(entry_kind, BindingKind::TypeDecl | BindingKind::SpecInvocation)
        && path.segments.len() >= 2
        && let Some(info) = ic.ty_cx.type_decl(binding_id)
        && let crate::cx::TypeDeclShape::Sum { .. } = &info.kind
    {
        let variant_sym = path.segments.last().unwrap().name;
        let variant_info = info.variant(variant_sym).cloned();
        return synth_variant_constructor(
            env,
            binding_id,
            entry_name,
            variant_sym,
            variant_info,
            args,
            ic,
            span,
        );
    }

    // Param / Local bindings may hold a fn-pointer value (`let h = f;
    // h(42)`). Route them through the indirect-call path — re-synth
    // the callee so its `ty` is populated, then dispatch on the
    // resulting type. This is the typing equivalent of the MIR-side
    // `lower_function_ref_by_path` fallback: a single-segment path
    // that names a Function reads as a fn-pointer value here, and the
    // same arm handles a path bound to a local.
    if matches!(entry_kind, BindingKind::Param | BindingKind::Local) {
        return synth_indirect_call(env, callee, args, ic, span);
    }

    if !matches!(entry_kind, BindingKind::Function) {
        let name = ic.lower.interner.resolve(entry_name).to_string();
        let kind_label = match entry_kind {
            BindingKind::TypeDecl => "type",
            BindingKind::Spec => "spec",
            BindingKind::SpecInvocation => "spec invocation",
            BindingKind::Const => "constant",
            BindingKind::ImportLeaf => "module",
            BindingKind::Param | BindingKind::Local => unreachable!(),
            BindingKind::Function => unreachable!(),
        };
        synth_args_for_diagnostics(env, args, ic);
        ic.emit_typecheck_error(span, format!("cannot call {kind_label} `{name}`"));
        return ic.ty_error();
    }

    synth_resolved_function_call(env, binding_id, entry_name, args, ic, span)
}

/// Type-check a `discriminant_of(v)` intrinsic call — the active
/// variant's declaration-order index of a sum-typed value, typed
/// `usize` (D-22 READ surface).
/// The single argument is an ordinary runtime value (not a comptime
/// `Type`); it must synthesise to a nominal whose declaration is a
/// [`crate::TypeDeclShape::Sum`]. On success the resolution is recorded
/// on `ic.intrinsic_calls` so MIR lowering emits the tag-read rvalue
/// instead of a `Call` terminator.
fn synth_discriminant_of_call(
    env: &mut TyEnv,
    args: &mut [HirCallArg],
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    if let Some(arg) = args.iter().find(|a| a.name.is_some()) {
        ic.emit_typecheck_error(
            arg.span,
            "named argument is not admitted on intrinsic `discriminant_of`".to_string(),
        );
    }
    if let Some(arg) = args.iter().find(|a| a.mode.is_some()) {
        ic.emit_typecheck_error(
            arg.span,
            "call-site mode keyword is not admitted on intrinsic `discriminant_of`".to_string(),
        );
    }
    if args.len() != 1 {
        synth_args_for_diagnostics(env, args, ic);
        ic.emit_typecheck_error(
            span,
            format!(
                "intrinsic `discriminant_of` takes 1 argument(s) but {found} were supplied",
                found = args.len()
            ),
        );
        return ic.ty_error();
    }
    synth_arg_expr(env, &mut args[0], ic);
    let arg_ty = args[0].expr.ty;
    let error = ic.ty_error();
    if arg_ty == error {
        return error;
    }
    let is_sum_value = matches!(
        ic.lower.ty_interner.kind(arg_ty),
        crate::ty::TyKind::Nominal(b)
            if matches!(
                ic.ty_cx.type_decl(*b).map(|info| &info.kind),
                Some(crate::cx::TypeDeclShape::Sum { .. })
            )
    );
    if !is_sum_value {
        let display = ic.display_ty(arg_ty).to_string();
        ic.emit_typecheck_error(
            span,
            format!(
                "intrinsic `discriminant_of` expects a sum-typed value, found `{display}`",
            ),
        );
        return error;
    }
    ic.intrinsic_calls.insert(span, crate::IntrinsicKind::DiscriminantOf);
    ic.lower.ty_interner.prim(crate::Primitive::Usize)
}

/// Type-check a resolved direct function call: extract the signature,
/// enforce arity, reject named arguments, check each argument against
/// its declared parameter type, run the comptime-purity gate, and
/// thread the callee's row (with §2 capability substitution) into the
/// caller's accumulator.
fn synth_resolved_function_call(
    env: &mut TyEnv,
    binding_id: edda_resolve::BindingId,
    entry_name: Symbol,
    args: &mut [HirCallArg],
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    // Extract the signature's parameter types, return type, full
    // parameter list (for capability substitution), and declared
    // effects so the `ic.ty_cx` borrow is released before threading
    // `ic` into `check_expr`.
    let sig_data = ic
        .ty_cx
        .sig(binding_id)
        .or_else(|| {
            // Minted specialization bindings carry their
            // signature in the mono side table until `check_package`
            // merges it into `TyCx` at the end of the pass.
            ic.mono_fns.and_then(|mf| mf.sigs.get(&binding_id))
        })
        .map(|s| {
            let param_tys: Vec<TyId> = s.params.iter().map(|p| p.ty).collect();
            let params: Vec<Param> = s.params.iter().cloned().collect();
            (param_tys, params, s.return_ty, s.effects.clone())
        });
    let (param_tys, params, return_ty, callee_effects) = match sig_data {
        Some(data) => data,
        None => {
            let name = ic.lower.interner.resolve(entry_name).to_string();
            synth_args_for_diagnostics(env, args, ic);
            ic.emit_typecheck_error(
                span,
                format!("no signature recorded for function `{name}`"),
            );
            return ic.ty_error();
        }
    };

    if args.len() != param_tys.len() {
        let name = ic.lower.interner.resolve(entry_name).to_string();
        let arity = param_tys.len();
        let supplied = args.len();
        synth_args_for_diagnostics(env, args, ic);
        ic.emit_typecheck_error(
            span,
            format!(
                "function `{name}` takes {arity} argument(s) but {supplied} were supplied",
            ),
        );
        return ic.ty_error();
    }

    // Function calls are positional (`expressions.md` §521).
    // Named-payload arguments are only admitted at variant constructor
    // sites (`declarations.md` §252); rejecting them here surfaces the
    // misuse with a precise diagnostic instead of silently dropping the
    // user's intent.
    for arg in args.iter() {
        if let Some(name) = &arg.name {
            let fn_name = ic.lower.interner.resolve(entry_name).to_string();
            let field_name = ic.lower.interner.resolve(name.name).to_string();
            ic.emit_typecheck_error(
                arg.span,
                format!(
                    "named argument `{field_name}:` is not admitted at \
                     function call site (function `{fn_name}` takes \
                     positional arguments)",
                ),
            );
        }
    }

    for (arg, param_ty) in args.iter_mut().zip(param_tys.iter()) {
        check_arg_expr(env, arg, *param_ty, ic);
        super::mode::apply_call_mode_transition(env, arg, ic);
    }

    // P-CompTimePure gate (`inference-rules.md §1a.6`): when this call
    // is inside a `comptime <expr>` or `comptime { … }` body, the
    // callee's row must be a subset of `{panic, yield: T}` — anything
    // else (capabilities, `err: T`) breaks comptime purity. The check
    // fires BEFORE row union so a non-pure call surfaces at the call
    // site even when the caller's declared row would otherwise admit
    // the entry.
    if ic.in_comptime_context {
        check_comptime_purity(&callee_effects, entry_name, span, ic);
    }

    // Record the callee resolution for the comptime evaluator,
    // keyed by the call expression's span, mirroring
    // `comptime_builtin_calls`. Recorded for EVERY resolved direct
    // call — not only lexically-comptime ones — because the evaluator
    // interprets the bodies of functions a comptime block calls, and
    // a call site inside such a body was type-checked as ordinary
    // code (a wrapper shape: `comptime { wrapper() }` where
    // `wrapper`'s body calls `theme.contrast_ok(...)`). Purity of
    // transitive callees is gated at evaluation time.
    ic.comptime_fn_calls.insert(span, binding_id);

    // T-FunCall row union: every entry of the callee's declared row
    // contributes to the caller's accumulator (`inference-rules.md
    // §1a.4`, `R = Rᶠ ∪ ⋃ᵢ Rᵢ` — each `Rᵢ` was already pushed by the
    // per-argument `check_expr` above). Pure entries pass through;
    // capability entries are rewritten by `effect-tracking.md §2`'s
    // "rows name parameters held, not derived bindings" rule —
    // `Capability(callee_param)` becomes `Capability(caller_source)`
    // where `caller_source` is the originating caller-side parameter
    // (or fallback when the source cannot be traced statically).
    let mut translated_entries: Vec<EffectEntry> =
        Vec::with_capacity(callee_effects.entries().len());
    for entry in callee_effects.entries() {
        let translated = translate_entry(*entry, &params, None, args, &ic.capability_aliases);
        ic.push_effect_entry(translated);
        translated_entries.push(translated);
    }
    check_err_propagation(&translated_entries, entry_name, span, ic);

    return_ty
}

#[cfg(test)]
#[path = "../call_tests.rs"]
mod tests;
