//! Per-call-site monomorphisation of outbound comptime type
//! parameters (layer 2).
//!
//! A function declaring `<comptime U: Type>` (codex
//! `04-specs-comptime.md` §2.4 — [`FnDecl::outbound_generics`]) or an
//! inbound `comptime <name>: Type` parameter (codex §4.5,
//! lifted into [`FnDecl::generics`]) is
//! a *template*: its signature and body reference `U` and cannot be
//! typechecked directly. This pass runs inside
//! [`crate::check_package`], after the D-22 comptime expansion of each
//! caller body and before per-function inference, and:
//!
//! 1. recognises every direct call whose callee resolves to a template
//!    binding,
//! 2. binds each type parameter — outbound parameters are inferred
//!    structurally from the matching argument's *declared* type (caller
//!    parameter annotations, annotated `let` bindings, and field chains
//!    through the [`ShapeIndex`] — the same per-field declared-type
//!    ground truth the `comptime if` evaluator uses); inbound
//!    `comptime <name>: Type` parameters are bound from the comptime
//!    type expression at their positional argument slot (a named type
//!    or `field_type_at(T, k)` — codex §4.5's
//!    `decode_field(field_type_at(T, i), ...)` form) and those
//!    arguments are then removed from the rewritten call,
//! 3. clones the template with `U` substituted (the substituted type
//!    reference carries the *bound type's* span, so the existing
//!    span-keyed `Resolutions` map already resolves it), expands its
//!    D-22 comptime forms, and recursively rewrites the specialized
//!    body — memoized by `(template, bound types)` with an explicit
//!    depth limit,
//! 4. rewrites the call site to a single-segment mangled name with an
//!    unresolved span. The typechecker resolves it through
//!    [`MonoFns`] (consulted by `infer::call::synth_call` when the
//!    span lookup misses); MIR lowering resolves it through its
//!    existing `function_symbols` single-segment name fallback, so no
//!    MIR change is needed.
//!
//! Specialized functions get minted [`BindingId`]s in a reserved high
//! index range. Those ids flow only through `TyCx` signature keys,
//! `TypedPackage::fn_bodies`, and MIR's map-keyed registries — never
//! into `ResolvedPackage::binding`, which would panic on them.
//! Signatures live in [`MonoFns::sigs`] while inference runs and are
//! merged into the package `TyCx` at the end of `check_package`, so
//! method-resolution's `iter_sigs → package.binding` walk never sees a
//! minted id.

use ahash::AHashMap;
use edda_diag::{Diagnostics, LintConfig};
use edda_intern::Symbol;
use edda_resolve::{BindingId, ModuleId, Resolved, ResolvedPackage};
use edda_span::Span;
use edda_syntax::ast::{FnBody, FnDecl, GenericKind, GenericParam, ItemKind, TypeKind};

use crate::comptime_expand::ShapeIndex;
use crate::lower::LowerCx;
use crate::prim::Primitive;
use crate::sig::FnSig;

mod infer_arg;
mod rewrite;
mod substitute;
mod type_arg;

pub(crate) use rewrite::rewrite_template_calls;

/// First intra-module binding index used for minted specialization ids.
const MONO_BINDING_BASE: u32 = 0x8000_0000;

/// Maximum specialization recursion depth.
const MONO_DEPTH_LIMIT: usize = 64;

/// Semantic identity of one bound outbound parameter, used to memoize
/// specializations across call sites.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum MonoKey {
    /// Bound to a locked primitive type.
    Prim(Primitive),
    /// Bound to a user type, identified by its resolver binding.
    Nominal(BindingId),
    /// Bound to the unit payload composite `()` — a payload-less sum
    /// variant's `field_type_at` under the native convention.
    Unit,
    /// Bound to a sum-variant payload composite `(A, B, …)`: the
    /// per-element classified keys in declaration order.
    Tuple(Vec<MonoKey>),
    /// Bound to a slice `[E]`: the element type's classified key.
    Slice(Box<MonoKey>),
}

/// One bound comptime type parameter's *reference* form — what the
/// substitution walker splices back into a specialized template
/// (`Named` → the type path; `Unit` → `()`; `Tuple` → a tuple type in
/// type position, a tuple-composite sentinel in expression position).
/// Its semantic identity for memoization is the parallel [`MonoKey`].
#[derive(Clone)]
pub(crate) enum BoundTy {
    /// A plain named type (`i32`, `Point`) referenced by its leaf symbol
    /// and the span whose resolution (or primitive spelling) names it.
    Named(Symbol, Span),
    /// The unit payload composite `()`.
    Unit,
    /// A sum-variant payload composite: the per-element plain-path type
    /// references in declaration order.
    Tuple(Vec<(Symbol, Span)>),
    /// A slice `[E]`: the element type's plain-path reference. Splices a
    /// `[E]` slice type in type position and a one-element array
    /// sentinel in expression position.
    Slice(Symbol, Span),
}

/// Specialized-function lookup surface threaded into inference.
///
/// `infer::call::synth_call` consults `by_name` when a single-segment
/// callee path has no recorded resolution, and
/// `synth_resolved_function_call` falls back to `sigs` when the
/// binding has no `TyCx` signature.
#[derive(Default)]
pub(crate) struct MonoFns {
    /// Mangled leaf name → minted specialization binding.
    pub by_name: AHashMap<Symbol, BindingId>,
    /// Minted specialization binding → lowered signature.
    pub sigs: AHashMap<BindingId, FnSig>,
}

/// One fully-generated specialization awaiting typecheck.
pub(crate) struct MonoSpec {
    /// Minted binding id (module = template's module, index in the
    /// reserved high range).
    pub binding: BindingId,
    /// Template's owning module — the resolution context the
    /// specialized body typechecks against.
    pub module: ModuleId,
    /// Substituted + comptime-expanded + call-rewritten declaration.
    pub decl: FnDecl,
}

/// Mutable monomorphisation state for one `check_package` run.
pub(crate) struct MonoState {
    /// `(template, bound types)` → mangled name of the memoized
    /// specialization.
    memo: AHashMap<(BindingId, Vec<MonoKey>), Symbol>,
    /// Lookup surface threaded into inference.
    pub fns: MonoFns,
    /// Generated specializations in creation order (callees precede
    /// callers is NOT guaranteed and not needed — typecheck happens
    /// after the whole set exists).
    pub specs: Vec<MonoSpec>,
    /// Next minted index offset above [`MONO_BINDING_BASE`].
    next_index: u32,
}

impl MonoState {
    /// Fresh state with no specializations.
    pub fn new() -> Self {
        Self {
            memo: AHashMap::default(),
            fns: MonoFns::default(),
            specs: Vec::new(),
            next_index: 0,
        }
    }
}

/// One template function: its declaration AST and owning module.
pub(crate) struct Template<'p> {
    /// Owning module.
    pub module: ModuleId,
    /// The declaring AST node (outbound_generics non-empty).
    pub decl: &'p FnDecl,
}

/// Read-only inputs shared by every mono step.
pub(crate) struct MonoCx<'a, 'p> {
    /// The resolved package being checked.
    pub package: &'p ResolvedPackage,
    /// Template-function table keyed by the template's binding.
    pub templates: &'a AHashMap<BindingId, Template<'p>>,
    /// Package-wide construction shapes (extended with per-member
    /// declared-type spans for argument-type inference).
    pub shapes: &'a ShapeIndex,
    /// Lowering context (interners + package) for signature lowering.
    pub lower_cx: &'a LowerCx<'a>,
    /// Active lint configuration.
    pub lint_cfg: &'a LintConfig,
    /// Active build target — folds `comptime if target_has("…")` in
    /// specialized template bodies.
    pub target: &'a edda_target::TargetCfg,
}

/// Collect every template function in the package, keyed by its
/// resolver binding.
pub(crate) fn collect_templates(
    package: &ResolvedPackage,
) -> AHashMap<BindingId, Template<'_>> {
    let mut out: AHashMap<BindingId, Template<'_>> = AHashMap::default();
    for module_entry in package.graph().modules() {
        let module_resolved = package.module(module_entry.id);
        for item in &module_entry.ast.items {
            if let ItemKind::Function(fn_decl) = &item.kind
                && is_template(fn_decl)
                && let Some(binding) = module_resolved.items.lookup(fn_decl.name.name)
            {
                out.insert(
                    binding,
                    Template {
                        module: module_entry.id,
                        decl: fn_decl,
                    },
                );
            }
        }
    }
    out
}

/// `true` when `decl` is a template this pass owns (and the regular
/// signature lowering / body check must skip).
pub(crate) fn is_template(decl: &FnDecl) -> bool {
    !decl.outbound_generics.is_empty() || !inbound_type_generics(decl).is_empty()
}

/// Inbound-lifted `comptime <name>: Type` generics of `decl`,
/// in declaration order. These occupy
/// positional argument slots at call sites (codex §4.5
/// `decode_field(field_type_at(T, i), ...)`), unlike the `<...>`
/// outbound form which is inferred from value arguments.
pub(crate) fn inbound_type_generics(decl: &FnDecl) -> Vec<&GenericParam> {
    decl.generics
        .iter()
        .filter(|g| {
            g.kind == GenericKind::Comptime
                && matches!(&g.ty, Some(t) if matches!(&t.kind, TypeKind::Meta))
        })
        .collect()
}

/// Get or create the specialization of `template_binding` for the
/// bound outbound-parameter types. Returns the mangled leaf name, or
/// `None` when generation failed (a diagnostic was emitted).
pub(crate) fn get_or_create_specialization(
    template_binding: BindingId,
    keys: Vec<MonoKey>,
    bound: &[BoundTy],
    depth: usize,
    call_span: Span,
    cx: &MonoCx<'_, '_>,
    state: &mut MonoState,
    diags: &mut Diagnostics,
) -> Option<Symbol> {
    if let Some(name) = state.memo.get(&(template_binding, keys.clone())) {
        return Some(*name);
    }
    if depth >= MONO_DEPTH_LIMIT {
        crate::lower::emit_typecheck_error(
            diags,
            cx.lint_cfg,
            call_span,
            "comptime type-parameter specialization exceeds the recursion depth limit",
        );
        return None;
    }
    let template = cx.templates.get(&template_binding)?;

    let mangled = mangle_specialization_name(
        template.decl.name.name,
        template_binding,
        &keys,
        bound,
        cx.lower_cx.interner,
    );
    let mangled_sym = cx.lower_cx.interner.intern(&mangled);
    let binding = BindingId::new(
        template.module,
        MONO_BINDING_BASE + state.next_index,
    );
    state.next_index += 1;

    // Substitute every type generic (outbound first, then inbound
    // `comptime <name>: Type` — the same order `bound` was assembled
    // in by the rewrite walk) with its bound type reference.
    let mut subs: AHashMap<Symbol, BoundTy> = AHashMap::default();
    for (gp, b) in template
        .decl
        .outbound_generics
        .iter()
        .chain(inbound_type_generics(template.decl))
        .zip(bound.iter())
    {
        subs.insert(gp.name.name, b.clone());
    }
    let mut spec_decl = substitute::substitute_fn_decl(template.decl, &subs, mangled_sym);

    // Lower the now-concrete signature and register the lookup surface
    // BEFORE recursing so recursive template chains hit the memo.
    let sig = crate::lower::lower_fn_sig(&spec_decl, cx.lower_cx, diags, cx.lint_cfg);
    state.memo.insert((template_binding, keys), mangled_sym);
    state.fns.by_name.insert(mangled_sym, binding);
    state.fns.sigs.insert(binding, sig);

    // Expand the substituted body's D-22 comptime forms (`comptime for`
    // over `field_count(<bound>)`, `comptime if is_*(<bound>)`, …) and
    // recursively rewrite any template calls the expansion exposed.
    if let FnBody::Block(block) = &mut spec_decl.body {
        if let Some(expanded) =
            crate::comptime_expand::expand_fn_body(block, cx.shapes, cx.lower_cx.interner, cx.target, diags)
        {
            *block = expanded;
        }
        rewrite::rewrite_block_calls(block, &spec_decl.params, depth + 1, cx, state, diags);
    }

    state.specs.push(MonoSpec {
        binding,
        module: template.module,
        decl: spec_decl,
    });
    Some(mangled_sym)
}

/// Compose the deterministic mangled leaf for one specialization.
fn mangle_specialization_name(
    fn_name: Symbol,
    template_binding: BindingId,
    keys: &[MonoKey],
    bound: &[BoundTy],
    interner: &edda_intern::Interner,
) -> String {
    let mut out = String::new();
    out.push_str(interner.resolve(fn_name));
    out.push_str("$m");
    out.push_str(&template_binding.module.as_u32().to_string());
    out.push('_');
    out.push_str(&template_binding.index.to_string());
    for (key, bt) in keys.iter().zip(bound.iter()) {
        out.push('$');
        mangle_bound(&mut out, key, bt, interner);
    }
    out
}

/// Append one bound parameter's deterministic mangle token. A named
/// reference contributes its leaf spelling (plus the resolver
/// coordinates for a nominal, so two same-named types never collide); a
/// unit composite contributes `unit`; a payload tuple contributes
/// `t<arity>` followed by each element's named token; a slice `[E]`
/// contributes `s_` followed by its element's named token.
fn mangle_bound(out: &mut String, key: &MonoKey, bt: &BoundTy, interner: &edda_intern::Interner) {
    match (key, bt) {
        (MonoKey::Prim(_) | MonoKey::Nominal(_), BoundTy::Named(leaf, _)) => {
            mangle_named(out, key, *leaf, interner);
        }
        (MonoKey::Unit, BoundTy::Unit) => out.push_str("unit"),
        (MonoKey::Tuple(elem_keys), BoundTy::Tuple(elem_refs)) => {
            out.push('t');
            out.push_str(&elem_keys.len().to_string());
            for (ek, (leaf, _)) in elem_keys.iter().zip(elem_refs.iter()) {
                out.push('_');
                mangle_named(out, ek, *leaf, interner);
            }
        }
        (MonoKey::Slice(elem_key), BoundTy::Slice(leaf, _)) => {
            out.push_str("s_");
            mangle_named(out, elem_key, *leaf, interner);
        }
        _ => out.push_str("__mono_key_mismatch"),
    }
}

/// Mangle a single named (primitive or nominal) element: its leaf
/// spelling, plus the `_<module>_<index>` resolver coordinates when
/// nominal so two identically-named types stay distinct.
fn mangle_named(out: &mut String, key: &MonoKey, leaf: Symbol, interner: &edda_intern::Interner) {
    out.push_str(interner.resolve(leaf));
    if let MonoKey::Nominal(id) = key {
        out.push('_');
        out.push_str(&id.module.as_u32().to_string());
        out.push('_');
        out.push_str(&id.index.to_string());
    }
}

/// Classify one bound [`BoundTy`] reference into its semantic
/// [`MonoKey`]. Returns `None` when a named reference neither resolves
/// to a TypeDecl binding nor names a locked primitive, or when a payload
/// composite carries such an element.
pub(crate) fn classify_bound_type(bt: &BoundTy, cx: &MonoCx<'_, '_>) -> Option<MonoKey> {
    match bt {
        BoundTy::Named(leaf, span) => classify_named(*leaf, *span, cx),
        BoundTy::Unit => Some(MonoKey::Unit),
        BoundTy::Tuple(elems) => {
            let mut keys = Vec::with_capacity(elems.len());
            for (leaf, span) in elems {
                keys.push(classify_named(*leaf, *span, cx)?);
            }
            Some(MonoKey::Tuple(keys))
        }
        BoundTy::Slice(leaf, span) => {
            Some(MonoKey::Slice(Box::new(classify_named(*leaf, *span, cx)?)))
        }
    }
}

/// Classify a single named `(leaf, span)` reference: a resolved TypeDecl
/// binding (`Nominal`) or a locked primitive (`Prim`).
fn classify_named(leaf: Symbol, span: Span, cx: &MonoCx<'_, '_>) -> Option<MonoKey> {
    if let Some(Resolved::Binding(id)) = cx.package.resolutions().lookup_path(span) {
        return Some(MonoKey::Nominal(id));
    }
    Primitive::from_name(cx.lower_cx.interner.resolve(leaf)).map(MonoKey::Prim)
}

/// Convert one [`MemberTy`](crate::comptime_expand::MemberTy) — a
/// `field_type_at(T, k)` ground-truth entry — into its [`BoundTy`]
/// reference form. `None` when the member type is not expressible as a
/// bound type: an `Opaque` product-field type, or a payload composite
/// with an unclassifiable (non-plain-path) element.
pub(crate) fn member_bound_ty(m: &crate::comptime_expand::MemberTy) -> Option<BoundTy> {
    use crate::comptime_expand::MemberTy;
    match m {
        MemberTy::Named(leaf, span) => Some(BoundTy::Named(*leaf, *span)),
        MemberTy::Unit => Some(BoundTy::Unit),
        MemberTy::Tuple(elems) => {
            let mut refs = Vec::with_capacity(elems.len());
            for el in elems {
                refs.push((*el)?);
            }
            Some(BoundTy::Tuple(refs))
        }
        MemberTy::Slice(Some((leaf, span))) => Some(BoundTy::Slice(*leaf, *span)),
        MemberTy::Slice(None) => None,
        MemberTy::Opaque => None,
    }
}
