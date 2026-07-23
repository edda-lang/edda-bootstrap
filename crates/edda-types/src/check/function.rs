//! Per-function typecheck driver — body inference + post-pass
//! discharge sequence.
//!
//! [`check_function`] runs once per `FnBody::Block` reachable from
//! [`super::check_package`]. It owns the four-phase sequence the
//! corpus locks in `02-modes-effects-refinements.md` §4 and
//! `03-verification.md` §§5–7: bidirectional inference of the body
//! (with the §4 function-exit row containment check), stability
//! discharge, refinement / graded / coherence / termination discharge,
//! and finally the per-function output drain back to the package-level
//! accumulators.

use ahash::AHashMap;
use edda_diag::{Diagnostics, LintConfig};
use edda_resolve::BindingId;
use edda_span::Span;
use edda_syntax::ast::FnDecl;

use crate::attr::AttrSet;
use crate::cx::TyCx;
use crate::implicit_spec::ImplicitSpecRequest;
use crate::infer::{BindingState, InferCx, TyEnv, check_fn_body};
use crate::lower::{LowerCx, lower_block};
#[cfg(feature = "refine")]
use crate::refine::{discharge_fn_call_preconditions, discharge_fn_clauses};

use super::TypedFunction;

/// Read-only inputs to [`check_function`] — package-wide state plus
/// the per-function attribute set and resolved module handle.
pub(crate) struct CheckFunctionCx<'a> {
    pub(crate) attrs: &'a AttrSet,
    pub(crate) module_resolved: &'a edda_resolve::ResolvedModule,
    pub(crate) ty_cx: &'a TyCx,
    pub(crate) lower_cx: &'a LowerCx<'a>,
    pub(crate) scc_map: &'a crate::infer::SccMap,
    pub(crate) lint_cfg: &'a LintConfig,
    /// Specialized-function lookup surface from the outbound-generic
    /// mono pass. Threaded into
    /// [`InferCx::mono_fns`] so call inference can resolve rewritten
    /// mangled call sites that the span-keyed `Resolutions` map misses.
    pub(crate) mono_fns: &'a crate::mono::MonoFns,
}

/// Package-level accumulators that [`check_function`] drains its
/// per-function inference outputs into.
pub(crate) struct CheckFunctionOut<'a> {
    pub(crate) method_resolutions: &'a mut AHashMap<Span, BindingId>,
    pub(crate) intrinsic_calls: &'a mut AHashMap<Span, crate::IntrinsicKind>,
    pub(crate) implicit_specs: &'a mut Vec<ImplicitSpecRequest>,
    pub(crate) comptime_type_paths: &'a mut AHashMap<Span, crate::ty::TyId>,
    pub(crate) comptime_builtin_calls: &'a mut AHashMap<Span, crate::ComptimeBuiltin>,
    pub(crate) comptime_fn_calls: &'a mut AHashMap<Span, BindingId>,
    pub(crate) primitive_static_calls: &'a mut AHashMap<Span, crate::PrimitiveStaticMethod>,
    pub(crate) capability_method_calls: &'a mut AHashMap<Span, crate::CapabilityMethod>,
}

/// Type-check one function declaration's body and return the resulting
/// [`TypedFunction`].
///
/// Looks the function up in its module's item table to recover the
/// [`BindingId`] that keys into [`TyCx::sig`]. If the resolver already
/// dropped the binding (duplicate decl etc.) or signature lowering
/// produced nothing for it, returns `None` — the originating
/// diagnostic was emitted upstream.
pub(crate) fn check_function(
    fn_decl: &FnDecl,
    body_block: &edda_syntax::ast::Block,
    cx: &CheckFunctionCx<'_>,
    out: &mut CheckFunctionOut<'_>,
    diags: &mut Diagnostics,
) -> Option<TypedFunction> {
    let binding_id = cx.module_resolved.items.lookup(fn_decl.name.name)?;
    let sig = cx.ty_cx.sig(binding_id)?;
    check_function_with(binding_id, sig, fn_decl, body_block, cx, out, diags)
}

/// [`check_function`]'s core with an explicit binding + signature.
/// The outbound-generic mono pass calls this directly for specialized
/// functions whose mangled names are absent from the module item
/// table and whose signatures live in the mono side table.
pub(crate) fn check_function_with(
    binding_id: BindingId,
    sig: &crate::sig::FnSig,
    fn_decl: &FnDecl,
    body_block: &edda_syntax::ast::Block,
    cx: &CheckFunctionCx<'_>,
    out: &mut CheckFunctionOut<'_>,
    diags: &mut Diagnostics,
) -> Option<TypedFunction> {
    let attrs = cx.attrs;
    let ty_cx = cx.ty_cx;
    let lower_cx = cx.lower_cx;
    let scc_map = cx.scc_map;
    let lint_cfg = cx.lint_cfg;
    let method_resolutions = &mut *out.method_resolutions;
    let intrinsic_calls = &mut *out.intrinsic_calls;
    let implicit_specs = &mut *out.implicit_specs;
    let comptime_type_paths = &mut *out.comptime_type_paths;
    let comptime_builtin_calls = &mut *out.comptime_builtin_calls;
    let comptime_fn_calls = &mut *out.comptime_fn_calls;
    let primitive_static_calls = &mut *out.primitive_static_calls;
    let capability_method_calls = &mut *out.capability_method_calls;

    let mut body = lower_block(body_block, lower_cx, diags, lint_cfg);

    let mut env = TyEnv::new();
    env.enter_scope();
    let mut take_param_syms: Vec<edda_intern::Symbol> = Vec::new();
    for (param_decl, param_sig) in fn_decl.params.iter().zip(sig.params.iter()) {
        // A `Default` (`let`) param is an immutable byval copy / shared
        // borrow — it cannot be `mutable`/`init`-borrowed, since the
        // backend lowers that as a copy and the write is silently lost.
        // `mutable`/`init`/`take` params
        // own or alias the caller's slot and may be re-borrowed mutably.
        let param_mutable = param_sig.mode != crate::sig::ParamMode::Default;
        // An `init`-mode param is the callee's obligation to
        // initialise, not a caller-supplied value — seed it
        // `Uninit` so the function body's own initialising write
        // (a plain assignment or `init`-arg delegation to another
        // init-taking callee) is what promotes it to `Valid`, and a
        // read before that write is `read_before_init`.
        let seed_state = if param_sig.mode == crate::sig::ParamMode::Init {
            BindingState::Uninit
        } else {
            BindingState::Valid
        };
        env.bind_with_state_mut(
            param_decl.name.name,
            param_sig.ty,
            seed_state,
            param_mutable,
        );
        if param_sig.mode == crate::sig::ParamMode::Take {
            take_param_syms.push(param_decl.name.name);
        }
    }

    // Inject `Pure(Divergence)` at function entry when this
    // function's call-graph SCC is recursive AND no member carries a
    // `decreases` clause. The function-exit row-containment check
    // surfaces the mismatch as `effect_row_mismatch` when the user has
    // not added `divergence` to their declared row.
    let inject_divergence = scc_map.is_divergent(binding_id);

    let mut ic = InferCx::new(lower_cx, ty_cx, lint_cfg, diags);
    ic.mono_fns = Some(cx.mono_fns);
    let exit_span = body_block.span;
    check_fn_body(
        &mut env,
        &mut body,
        &mut ic,
        &sig.effects,
        sig.return_ty,
        exit_span,
        inject_divergence,
        &take_param_syms,
    );

    // Move the per-function inference outputs out of `ic` so the
    // mutable borrow of `diags` held by `InferCx` can be released
    // before stability discharge re-borrows `diags`. The owned local
    // collections feed both stability (which only reads
    // `method_res_local`) and the subsequent package-level drains.
    let method_res_local = std::mem::take(&mut ic.method_resolutions);
    let intrinsic_calls_local = std::mem::take(&mut ic.intrinsic_calls);
    let implicit_specs_local = std::mem::take(&mut ic.implicit_specs);
    let comptime_type_paths_local = std::mem::take(&mut ic.comptime_type_paths);
    let comptime_builtin_calls_local = std::mem::take(&mut ic.comptime_builtin_calls);
    let comptime_fn_calls_local = std::mem::take(&mut ic.comptime_fn_calls);
    let primitive_static_calls_local = std::mem::take(&mut ic.primitive_static_calls);
    let capability_method_calls_local = std::mem::take(&mut ic.capability_method_calls);
    drop(ic);

    // Phase B stability structural check per `03-verification.md` §7.
    // Short-circuits when the function is not declared
    // `stable function`. Otherwise checks the effect-row whitelist,
    // walks the body for non-stable callees / scope(exec) / hash
    // iteration (both path- and method-form), the `@unverified`
    // rejection rule, and emits per-rule diagnostics.
    crate::stability::discharge_fn_stability(
        fn_decl,
        sig,
        attrs,
        ty_cx,
        lower_cx,
        &method_res_local,
        lint_cfg,
        diags,
    );

    // Drain the per-function method-resolution map into the
    // package-level accumulator so the MIR-lowering pass can read it.
    for (span, binding) in method_res_local {
        method_resolutions.insert(span, binding);
    }
    // Drain per-function intrinsic-call map into the package-level accumulator.
    for (span, kind) in intrinsic_calls_local {
        intrinsic_calls.insert(span, kind);
    }
    // Drain per-function implicit-spec requests into the package-level
    // accumulator. `InferCx::register_implicit_spec` already deduped
    // within this function; cross-function dedupe happens at the
    // append site below.
    for req in implicit_specs_local {
        if !implicit_specs
            .iter()
            .any(|existing| existing.kind == req.kind && existing.type_arg == req.type_arg)
        {
            implicit_specs.push(req);
        }
    }
    // Drain per-function comptime resolution maps (§C10). Spans are
    // source-location unique, so no per-key dedup is needed; the
    // accumulator just absorbs both maps wholesale.
    for (span, ty) in comptime_type_paths_local {
        comptime_type_paths.insert(span, ty);
    }
    for (span, builtin) in comptime_builtin_calls_local {
        comptime_builtin_calls.insert(span, builtin);
    }
    for (span, binding) in comptime_fn_calls_local {
        comptime_fn_calls.insert(span, binding);
    }
    for (span, method) in primitive_static_calls_local {
        primitive_static_calls.insert(span, method);
    }

    env.exit_scope();

    // `@unverified(reason = "...")` admits every obligation inside the
    // function without SMT discharge per refinement-decidability.md §9.
    // The attribute set is threaded into the clause-discharge pass so
    // the ensures-clause loop can route through the certificate path
    // instead of the SMT solver (bug C14 extension).
    #[cfg(feature = "refine")]
    discharge_fn_clauses(fn_decl, sig, attrs, ty_cx, lower_cx, diags);

    // Call-site precondition discharge:
    // for every call in this function's body whose callee declares
    // `requires` clauses, substitutes the callee's parameters with the
    // actual call-site arguments and discharges the result under
    // `ObligationKind::RequiresAtCall`. Distinct from the pass above,
    // which only ever treats a function's own `requires` as an
    // assumption for its own `ensures` — never as a goal checked
    // against a caller's arguments. Reads the per-function
    // capability-method map to recognise `alloc_array` result bindings,
    // so that map's drain is deferred below.
    #[cfg(feature = "refine")]
    discharge_fn_call_preconditions(
        fn_decl,
        sig,
        attrs,
        ty_cx,
        lower_cx,
        &capability_method_calls_local,
        diags,
    );

    // Drain the per-function capability-method map into the
    // package-level accumulator — after the call-site precondition
    // pass, which is the only consumer of the per-function map.
    for (span, method) in capability_method_calls_local {
        capability_method_calls.insert(span, method);
    }

    // Graded-effect missing-kind structural check per
    // `02-modes-effects-refinements.md` §5 — emits
    // `effect_graded_bound_exceeded` when a callee declares a graded
    // kind the caller's row has no entry for. Bound-vs-bound discharge
    // (sum / branch-max / loop-lift) lives behind the `refine` feature
    // gate in `graded_refine` and routes through the Z3 LIA backend.
    crate::graded::discharge_fn_graded_calls(
        fn_decl, sig, ty_cx, lower_cx, lint_cfg, diags,
    );

    // Phase B graded-effect bound discharge: §5.4 accumulator
    // (sum/branch-max/loop-lift) plus parameter-referencing-bound
    // LIA lift through `edda_refine::lift_predicate`. Feature-gated so
    // crates that omit the Z3 dependency keep compiling.
    #[cfg(feature = "refine")]
    crate::graded_refine::discharge_fn_graded_bounds(
        fn_decl, sig, ty_cx, lower_cx, diags,
    );

    // Return-position borrow-mode region check: when the signature
    // declares `-> let T` / `-> mutable T`, verify the function has a
    // tie-able receiver parameter and that every returned place is
    // rooted at one. Behaviour-neutral
    // for by-value returns.
    crate::return_mode::discharge_fn_return_mode(
        fn_decl, sig, lower_cx, lint_cfg, diags,
    );

    // Phase C coherence-region re-validation per
    // `05-concurrency-coherence.md` §3. For each `scope(coherence)`
    // region in the body, conservatively emit
    // `coherence_mutable_refinement_invalidated` when a refined
    // `mutable` parameter is referenced inside the region.
    crate::coherence::discharge_fn_coherence(
        fn_decl, sig, ty_cx, lower_cx, lint_cfg, diags,
    );

    // C5: termination obligations on the function's `decreases` clause
    // at every in-SCC recursive call site. Non-recursive functions and
    // functions without a `decreases` clause are short-circuited inside
    // the discharge function. Feature-gated behind `refine` so crates
    // that do not build Z3 stay clean.
    //
    // The attribute set is threaded through so the discharge layer can
    // honour a function-level `@unverified(reason = "...")` annotation
    // across every termination sub-obligation per
    // refinement-decidability.md §9 (bug C14).
    #[cfg(feature = "refine")]
    crate::refine::discharge_fn_termination(
        fn_decl,
        sig,
        attrs,
        ty_cx,
        lower_cx,
        scc_map,
        binding_id,
        diags,
    );

    Some(TypedFunction {
        binding: binding_id,
        name: fn_decl.name.name,
        span: fn_decl.span,
        body,
    })
}
