//! Package-level typecheck driver — composes signature lowering with
//! per-function body inference.
//!
//! [`check_package`] is the entry point the build driver reaches for
//! once `edda-resolve` produces a [`ResolvedPackage`]. It builds the
//! [`crate::TyCx`] (signatures + type-decl layouts) for the whole
//! package, runs bidirectional inference against every function body,
//! and returns a [`TypedPackage`] carrying the [`TyCx`] plus the
//! resulting typed HIR.
//!
//! Downstream consumers split by need:
//!
//! - `edda check` ignores [`TypedPackage::fn_bodies`] and only inspects
//!   `diags`.
//! - `edda build` hands [`TypedPackage::fn_bodies`] +
//!   [`TypedPackage::type_decls`] to `edda-mir`'s lowering pass.
//!
//! The orchestrator lives here rather than in `edda-driver` so the
//! inference primitives (`LowerCx`, `InferCx`, `TyEnv`,
//! `check_fn_body`, `lower_block`, `build_ty_cx`) can stay
//! crate-private to `edda-types`. The driver imports one function and
//! gets the whole pass.
//!
//! # Crate layout
//!
//! - This file ([`check::mod`]) — public [`TypedPackage`] + friends and
//!   the [`check_package`] orchestrator that walks every item in the
//!   resolved package and dispatches by item kind.
//! - [`check::function`] — the per-function driver
//!   ([`function::check_function`]) plus its `CheckFunctionCx` /
//!   `CheckFunctionOut` carriers. Encapsulates the inference + post-
//!   pass discharge sequence that runs once per `FnBody::Block`.

use ahash::AHashMap;
use edda_diag::{Diagnostics, LintConfig};
use edda_intern::{Interner, Symbol};
use edda_resolve::{BindingId, ModuleId, ResolvedPackage};
use edda_span::Span;
use edda_syntax::ast::{FnBody, ItemKind, SpecInvocation};

use crate::attr::{AttrSet, AttrTarget, validate_attributes};
use crate::cx::TyCx;
use crate::hir::HirBlock;
use crate::implicit_spec::ImplicitSpecRequest;
use crate::lower::{LowerCx, build_ty_cx};
use crate::ty::TyInterner;

mod cap_availability;
mod function;
mod target_gate;

pub use cap_availability::check_capability_availability;
use self::function::{CheckFunctionCx, CheckFunctionOut, check_function};

/// Result of [`check_package`]: the typed cross-module context plus the
/// per-function typed HIR and the per-type-decl identity that
/// downstream passes (MIR lowering, codegen) consume.
pub struct TypedPackage {
    /// Cross-module signatures + type-decl layouts.
    pub ty_cx: TyCx,
    /// Typed HIR bodies, one per Function item whose body is a source
    /// block and whose signature lowered cleanly, in package-walk
    /// order. Extern-bodied functions land in [`Self::extern_decls`].
    pub fn_bodies: Vec<TypedFunction>,
    /// Extern-bodied function declarations, one per Function item
    /// whose body is an `extern "symbol"` clause. The signature is
    /// looked up in [`Self::ty_cx`] keyed by `binding`; MIR lowering
    /// uses these to emit `FuncRef::Extern { name, sig }` at every
    /// call site that resolves to one of these bindings.
    pub extern_decls: Vec<TypedExternFunction>,
    /// Type-decl identities, one per TypeDecl item that resolved
    /// cleanly, in package-walk order. The layout is reachable through
    /// [`TyCx::type_decl`] keyed by `binding`.
    pub type_decls: Vec<TypedTypeDecl>,
    /// MethodCall span → resolved free-function `BindingId` map.
    /// Populated by `infer::method::synth_method_call` whenever it
    /// resolves a `receiver.name(args)` call to a free function whose
    /// first parameter type matches the receiver. The MIR-lowering
    /// pass reads this to desugar each `MethodCall` site into a
    /// regular `Call` against the resolved binding (the receiver
    /// becomes argument 0).
    pub method_resolutions: AHashMap<Span, BindingId>,
    /// MethodCall span → compiler intrinsic kind map.
    /// Populated by `infer::method::synth_method_call` when the receiver
    /// type + method name pair matches a compiler intrinsic (`bytes` on
    /// `String`, `len` on slices). The MIR-lowering pass reads this to
    /// emit the appropriate `RvalueKind` instead of a `TerminatorKind::Call`.
    pub intrinsic_calls: AHashMap<Span, crate::IntrinsicKind>,
    /// Implicit-spec invocation requests harvested from per-function
    /// inference (`inference-rules.md §3` — range-literal `Range<T>`
    /// and `none`-pattern `Option<T>` triggers). The codegen pass
    /// reads these to drive spec instantiation; deduped in
    /// first-trigger order across the whole package.
    pub implicit_specs: Vec<ImplicitSpecRequest>,
    /// Explicit user-written top-level spec invocations
    /// (`comptime.md` §312). The codegen pass evaluates each
    /// invocation's comptime args and routes them to
    /// `edda_codegen::instantiate_spec`.
    pub spec_invocations: Vec<TypedSpecInvocation>,
    /// Typed item-level attribute payloads
    /// (`@export` / `@abi` / `@align` / `@repr` / `@layout`),
    /// keyed by the resolver-issued binding id.
    pub attributes: AHashMap<BindingId, AttrSet>,
    /// Comptime path-as-type resolution map (§C10).
    ///
    /// Populated by [`crate::infer`]'s path arm when a path inside a
    /// `comptime <expr>` / `comptime { … }` body names a primitive or
    /// user type. MIR lowering and the codegen-side comptime evaluator
    /// reach into this map to recover the concrete `TyId` the path
    /// refers to when the surface expression's type is the `Type`
    /// meta-primitive.
    pub comptime_type_paths: AHashMap<Span, crate::ty::TyId>,
    /// Comptime built-in call resolution map (§C10).
    ///
    /// Populated by [`crate::infer`]'s call arm when the callee names
    /// `size_of` / `align_of` / `offset_of` / `target_has`. MIR
    /// lowering reads this to recognise the call site without
    /// re-walking the catalogue catalogue.
    pub comptime_builtin_calls: AHashMap<Span, crate::ComptimeBuiltin>,
    /// Direct-call resolution map for the comptime evaluator.
    ///
    /// Populated by [`crate::infer`]'s resolved-function-call arm for
    /// every direct named-function call — package-wide, not only
    /// inside `comptime` bodies, because the evaluator interprets the
    /// bodies of functions a comptime block calls and needs their
    /// call sites resolvable too. The codegen-side comptime evaluator
    /// reads this to resolve a callee to its declaration (signature +
    /// typed-HIR body) when interpreting a call.
    pub comptime_fn_calls: AHashMap<Span, BindingId>,
    /// Primitive-headed static call resolution map. Populated by
    /// [`crate::infer`]'s call arm when it sees a two-segment path
    /// whose `(head, leaf)` pair matches
    /// [`crate::resolve_primitive_static_method`]. MIR lowering reads
    /// this to emit a `Call` with `FuncRef::Extern` targeting the
    /// variant's `__edda_*` runtime symbol — the resolver does not
    /// record these paths because their heads are catalogue items.
    pub primitive_static_calls: AHashMap<Span, crate::PrimitiveStaticMethod>,
    /// Capability-method call resolution map. Populated by
    /// [`crate::infer`]'s method-call arm when the receiver's type is a
    /// [`crate::CapabilityType`] and the `(capability, method-name)`
    /// pair matches [`crate::resolve_capability_method`]. MIR lowering
    /// reads this to emit a `Call` against the variant's
    /// `__edda_*` extern symbol so the existing alloc-family rewrite
    /// (`__edda_alloc_array` → `__edda_alloc_array_raw` with prepended
    /// `size_of(T)` / `align_of(T)`) materialises the runtime ABI.
    pub capability_method_calls: AHashMap<Span, crate::CapabilityMethod>,
}

/// One explicit spec invocation surfaced from a module's AST.
///
/// The codegen pass receives a slice of these and, per invocation,
/// looks up the resolved `Spec` declaration AST, evaluates the
/// comptime args, and drives `edda_codegen::instantiate_spec`.
#[derive(Clone, Debug)]
pub struct TypedSpecInvocation {
    /// Resolver-issued module id this invocation was declared in.
    pub module: ModuleId,
    /// Cloned AST node, including the path and argument expressions.
    pub invocation: SpecInvocation,
}

/// One function presented by the typecheck pass: its resolver-side
/// binding id, name, defining span, and the inference-mutated typed
/// HIR body.
pub struct TypedFunction {
    /// Resolver-issued binding id for this function.
    pub binding: BindingId,
    /// Source-declared function name.
    pub name: Symbol,
    /// Defining span of the full function declaration.
    pub span: Span,
    /// Typed HIR body (inference has populated every `ty` carrier).
    pub body: HirBlock,
}

/// One extern-bodied function presented by the typecheck pass.
///
/// Extern declarations have no Edda-side body: the resolver did not
/// walk one, the typechecker did not lower or check one. The
/// signature (parameters, return type, effect row) is recorded in
/// [`TyCx`] the same way it is for source-bodied functions; the
/// `symbol` carries the linker-visible name to emit at the call
/// site.
pub struct TypedExternFunction {
    /// Resolver-issued binding id for this function declaration.
    pub binding: BindingId,
    /// Source-declared function name (the in-language identifier
    /// callers use to reference the extern).
    pub name: Symbol,
    /// Defining span of the full function declaration.
    pub span: Span,
    /// Linker-visible extern symbol from the `extern "..."` clause.
    pub symbol: Symbol,
    /// Importing-DLL name from the optional `from "dll"` clause.
    /// `Some` means the driver's link
    /// stage resolves `symbol` through a PE `.idata` import from this
    /// DLL; `None` keeps the static resolution.
    pub dll: Option<Symbol>,
}

/// One user-declared `type` presented by the typecheck pass.
///
/// SpecInvocation bindings also appear here when
/// `lower_spec_invocation` registered a substituted [`TypeDeclInfo`] in
/// [`TyCx`] — MIR lowering reads the same `TypeDeclInput` slot for
/// both kinds so paths like `Option_u8.none` lower against the
/// substituted layout even when codegen has not materialised the
/// invocation as a separate module.
pub struct TypedTypeDecl {
    /// Resolver-issued binding id for this type declaration.
    pub binding: BindingId,
    /// Source-declared type name (for TypeDecl bindings) or the
    /// CA1-mangled short name (for SpecInvocation bindings).
    pub name: Symbol,
}

/// Drive the typecheck pass for an entire resolved package.
///
/// Builds a fresh [`TyCx`] holding signature + layout information for
/// every Function / TypeDecl in `package`, then lowers each function
/// body to HIR and runs bidirectional inference against it. The §4
/// function-exit rule is enforced per body via
/// [`crate::infer::check_fn_body`].
///
/// Diagnostics from signature lowering, body lowering, and per-body
/// inference flow into `diags` with their effective severity gated by
/// `lint_cfg`. The driver uses [`edda_diag::Diagnostics::has_errors`]
/// to decide the build's exit code.
///
/// Returns a [`TypedPackage`] bundling the [`TyCx`], the typed HIR
/// bodies, and the type-decl identities that downstream passes
/// consume. `edda check` discards `fn_bodies` / `type_decls`; `edda
/// build` threads them into `edda-mir`'s lowering pass.
pub fn check_package(
    package: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
    lint_cfg: &LintConfig,
    target_cfg: &edda_target::TargetCfg,
    diags: &mut Diagnostics,
) -> TypedPackage {
    let lower_cx = LowerCx {
        interner,
        ty_interner,
        package: Some(package),
    };
    let mut ty_cx = build_ty_cx(package, &lower_cx, diags, lint_cfg);
    // D-22 comptime expansion: unroll `comptime for … field_count(T)`
    // and fold `out.(i)` → `out.<field_name>` before any body is lowered
    // to HIR. The index is structural (product-type field names) and is
    // built once for the whole package; per-body expansion only clones
    // bodies that actually contain a comptime-for / CompField.
    let field_index = crate::comptime_expand::build_shape_index(package);
    // Layer 2: outbound-generic templates and the mono state that
    // accumulates their per-call-site specializations. Templates are
    // excluded from sig lowering (see `build_ty_cx`) and from the body
    // walk below; specializations are generated while rewriting caller
    // bodies and typechecked after the package walk.
    let mono_templates = crate::mono::collect_templates(package);
    let mut mono_state = crate::mono::MonoState::new();
    // SCC + divergence-injection pre-pass: walks every
    // function's resolved AST body, builds the call graph, partitions
    // into SCCs, and decides per-SCC whether the row-
    // containment check should see a [`Pure(Divergence)`] entry at
    // function entry. The map is consulted per function in
    // [`check_function`] below.
    let scc_map = crate::infer::build_scc_map(package);
    // Functions gated by `@target_requires(T)` for a `T`
    // this build target does not support do not exist on this target —
    // collected ahead of the item walk below so it can skip lowering
    // their bodies (and extern declarations) into `TypedPackage`.
    let target_gated = target_gate::collect_target_gated(package, interner, target_cfg.triple());
    let mut fn_bodies = Vec::new();
    let mut extern_decls = Vec::new();
    let mut type_decls = Vec::new();
    let mut method_resolutions: AHashMap<Span, BindingId> = AHashMap::default();
    let mut intrinsic_calls: AHashMap<Span, crate::IntrinsicKind> = AHashMap::default();
    let mut implicit_specs: Vec<ImplicitSpecRequest> = Vec::new();
    let mut comptime_type_paths: AHashMap<Span, crate::ty::TyId> = AHashMap::default();
    let mut comptime_builtin_calls: AHashMap<Span, crate::ComptimeBuiltin> = AHashMap::default();
    let mut comptime_fn_calls: AHashMap<Span, BindingId> = AHashMap::default();
    let mut primitive_static_calls: AHashMap<Span, crate::PrimitiveStaticMethod> =
        AHashMap::default();
    let mut capability_method_calls: AHashMap<Span, crate::CapabilityMethod> =
        AHashMap::default();
    let mut spec_invocations: Vec<TypedSpecInvocation> = Vec::new();
    let mut attributes: AHashMap<BindingId, AttrSet> = AHashMap::default();

    // A bodyless `@abi` function has no body to walk, so its §7 stability
    // discharge needs no method-resolution
    // map; this empty map is passed for every `FnBody::Extern` check.
    let no_method_res: AHashMap<Span, BindingId> = AHashMap::default();

    for module_entry in package.graph().modules() {
        let module_resolved = package.module(module_entry.id);
        for item in &module_entry.ast.items {
            // Dispatch attribute validation by item kind. The validator
            // emits a diagnostic for any attribute attached to an
            // `AttrTarget::Other` item (Import / Module / Spec /
            // SpecInvocation / module-level Let), so users see a
            // precise rejection rather than silent drops.
            let target = match &item.kind {
                ItemKind::Function(fn_decl) => match &fn_decl.body {
                    // A function whose body slot is `extern "sym"`
                    // (FFI binding) admits `@abi("symbol-name")` as a
                    // linker-symbol payload, not a calling convention. The
                    // validator routes that via the dedicated variant.
                    edda_syntax::ast::FnBody::Extern { .. } => AttrTarget::ExternFunction,
                    edda_syntax::ast::FnBody::Block(_) => AttrTarget::Function,
                },
                ItemKind::TypeDecl(_) => AttrTarget::TypeDecl,
                _ => AttrTarget::Other,
            };
            let attr_set =
                validate_attributes(&item.attributes, target, interner, lint_cfg, diags);

            match &item.kind {
                ItemKind::Function(fn_decl) => {
                    let fn_binding = module_resolved.items.lookup(fn_decl.name.name);
                    if !attr_set.is_empty() {
                        if let Some(binding) = fn_binding {
                            attributes.insert(binding, attr_set);
                        }
                    }
                    // This item's `@target_requires(T)`
                    // capability is unsupported on `target_cfg` — the
                    // function does not exist on this target, so its
                    // body (source-bodied or extern) is never lowered.
                    // Call sites that still reference it are diagnosed
                    // by `check_target_requires_gate` below; the
                    // signature stays registered in `ty_cx` (via
                    // `build_ty_cx` above) so those call sites still
                    // resolve.
                    if fn_binding.is_some_and(|b| target_gated.contains_key(&b)) {
                        continue;
                    }
                    // Templates are typechecked per specialization
                    // by the mono pass, never directly — their bodies
                    // reference outbound generics no inference rule can
                    // type.
                    if crate::mono::is_template(fn_decl) {
                        continue;
                    }
                    match &fn_decl.body {
                        FnBody::Block(block) => {
                            // Expand D-22 comptime forms (returns `None`
                            // when the body has none, so non-comptime
                            // bodies are checked without a clone).
                            let expanded = crate::comptime_expand::expand_fn_body(
                                block,
                                &field_index,
                                interner,
                                target_cfg,
                                diags,
                            );
                            let block = expanded.as_ref().unwrap_or(block);
                            // Rewrite calls to outbound-generic
                            // templates into mangled specialization
                            // calls (returns `None` when the body calls
                            // no template).
                            let mono_cx = crate::mono::MonoCx {
                                package,
                                templates: &mono_templates,
                                shapes: &field_index,
                                lower_cx: &lower_cx,
                                lint_cfg,
                                target: target_cfg,
                            };
                            let mono_rewritten = crate::mono::rewrite_template_calls(
                                block,
                                fn_decl,
                                &mono_cx,
                                &mut mono_state,
                                diags,
                            );
                            let block = mono_rewritten.as_ref().unwrap_or(block);
                            let cx = CheckFunctionCx {
                                attrs: &attr_set,
                                module_resolved,
                                ty_cx: &ty_cx,
                                lower_cx: &lower_cx,
                                scc_map: &scc_map,
                                lint_cfg,
                                mono_fns: &mono_state.fns,
                            };
                            let mut out = CheckFunctionOut {
                                method_resolutions: &mut method_resolutions,
                                intrinsic_calls: &mut intrinsic_calls,
                                implicit_specs: &mut implicit_specs,
                                comptime_type_paths: &mut comptime_type_paths,
                                comptime_builtin_calls: &mut comptime_builtin_calls,
                                comptime_fn_calls: &mut comptime_fn_calls,
                                primitive_static_calls: &mut primitive_static_calls,
                                capability_method_calls: &mut capability_method_calls,
                            };
                            if let Some(typed) =
                                check_function(fn_decl, block, &cx, &mut out, diags)
                            {
                                fn_bodies.push(typed);
                            }
                        }
                        FnBody::Extern { name: symbol, dll, .. } => {
                            // Signature lowered into `ty_cx` already; there is no
                            // body to walk, but the §7 stable-row check still
                            // applies — a bodyless `@abi` `stable function` carries
                            // the same effect-row obligation as a bodied one
                            // (D-20 admits
                            // `DeterministicRandom`). `discharge_fn_stability`
                            // runs the row + `@unverified` rules and skips its
                            // (vacuous) body walker for `FnBody::Extern`. Emit the
                            // typed-extern record so the MIR pass can route call
                            // sites to `FuncRef::Extern`.
                            if let Some(binding_id) = fn_binding {
                                if let Some(sig) = ty_cx.sig(binding_id) {
                                    crate::stability::discharge_fn_stability(
                                        fn_decl,
                                        sig,
                                        &attr_set,
                                        &ty_cx,
                                        &lower_cx,
                                        &no_method_res,
                                        lint_cfg,
                                        diags,
                                    );
                                }
                                extern_decls.push(TypedExternFunction {
                                    binding: binding_id,
                                    name: fn_decl.name.name,
                                    span: fn_decl.span,
                                    symbol: *symbol,
                                    dll: *dll,
                                });
                            }
                        }
                    }
                }
                ItemKind::TypeDecl(decl) => {
                    if let Some(binding) = module_resolved.items.lookup(decl.name.name) {
                        if !attr_set.is_empty() {
                            attributes.insert(binding, attr_set);
                        }
                        type_decls.push(TypedTypeDecl {
                            binding,
                            name: decl.name.name,
                        });
                    }
                }
                ItemKind::SpecInvocation(si) => {
                    spec_invocations.push(TypedSpecInvocation {
                        module: module_entry.id,
                        invocation: (**si).clone(),
                    });
                    // Register the substituted TypeDecl (when one was
                    // produced by `lower_spec_invocation`) as a
                    // `TypedTypeDecl` so MIR lowering's `adt_map`
                    // includes it under the SpecInvocation's
                    // BindingId. Without this entry,
                    // `<SpecInv>.<member>` paths in expression
                    // position (e.g. `Option_u8.none` inside a
                    // codegen-produced module whose nested spec
                    // invocation has not been independently
                    // materialised) fail at MIR lowering with
                    // `MultiSegmentPath`.
                    if let Some(short_name) =
                        edda_resolve::mangle_spec_invocation_name(si, interner)
                        && let Some(binding) = module_resolved.items.lookup(short_name)
                        && ty_cx.type_decl(binding).is_some()
                    {
                        type_decls.push(TypedTypeDecl {
                            binding,
                            name: short_name,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    // Typecheck every generated specialization. Each is a fully
    // concrete function (outbound generics substituted, comptime forms
    // expanded, nested template calls rewritten); its signature lives
    // in the mono side table and its resolution context is the
    // template's module. Specializations land in `fn_bodies` like any
    // source function, so the driver's MIR input builder and the MIR
    // `function_symbols` name fallback pick them up with no changes.
    let mono_specs = std::mem::take(&mut mono_state.specs);
    let no_attrs = AttrSet::default();
    for spec in &mono_specs {
        let FnBody::Block(block) = &spec.decl.body else {
            continue;
        };
        let module_resolved = package.module(spec.module);
        let cx = CheckFunctionCx {
            attrs: &no_attrs,
            module_resolved,
            ty_cx: &ty_cx,
            lower_cx: &lower_cx,
            scc_map: &scc_map,
            lint_cfg,
            mono_fns: &mono_state.fns,
        };
        let mut out = CheckFunctionOut {
            method_resolutions: &mut method_resolutions,
            intrinsic_calls: &mut intrinsic_calls,
            implicit_specs: &mut implicit_specs,
            comptime_type_paths: &mut comptime_type_paths,
            comptime_builtin_calls: &mut comptime_builtin_calls,
            comptime_fn_calls: &mut comptime_fn_calls,
            primitive_static_calls: &mut primitive_static_calls,
            capability_method_calls: &mut capability_method_calls,
        };
        let Some(sig) = mono_state.fns.sigs.get(&spec.binding) else {
            continue;
        };
        if let Some(typed) = self::function::check_function_with(
            spec.binding,
            sig,
            &spec.decl,
            block,
            &cx,
            &mut out,
            diags,
        ) {
            fn_bodies.push(typed);
        }
    }
    // Merge specialization signatures into the package `TyCx` only now
    // — during inference they must stay out of `TyCx` so the method-
    // resolution `iter_sigs → package.binding(id)` walk never sees a
    // minted id (which `ResolvedPackage::binding` would panic on).
    for (binding, sig) in std::mem::take(&mut mono_state.fns.sigs) {
        ty_cx.insert_sig(binding, sig);
    }

    // Diagnose every surviving call site that still
    // references a `@target_requires`-gated-absent function.
    target_gate::check_target_requires_gate(package, interner, &target_gated, lint_cfg, diags);

    TypedPackage {
        ty_cx,
        fn_bodies,
        extern_decls,
        type_decls,
        method_resolutions,
        intrinsic_calls,
        implicit_specs,
        spec_invocations,
        attributes,
        comptime_type_paths,
        comptime_builtin_calls,
        comptime_fn_calls,
        primitive_static_calls,
        capability_method_calls,
    }
}
