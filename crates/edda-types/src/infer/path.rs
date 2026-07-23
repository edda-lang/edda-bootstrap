//! Path-resolution + literal kernel for the inference dispatcher.
//!
//! [`synth_path`] implements T-Var plus the cross-module fallback through
//! [`edda_resolve::Resolutions`]. The literal helpers ([`synth_literal`],
//! [`check_literal`]) plus the synth-vs-expected bridge
//! ([`check_synth_against_expected`]) are kept here too because
//! [`synth_path`]'s diagnostics share the same shape (single-segment env
//! lookup falling back to a resolver consult).

use edda_span::Span;
use edda_syntax::ast::Literal;

use super::lit::fits_in_primitive;
use super::mode::BindingState;
use super::{InferCx, TyEnv};
use crate::cx::{TypeDeclShape, VariantPayloadInfo};
use crate::prim::Primitive;
use crate::ty::{TyId, TyKind};

/// Record that the [`Span`] of a path-expression names a type-as-value
/// in comptime context. Downstream the comptime evaluator and MIR
/// lowering reach into this map to obtain the concrete [`TyId`] the
/// path refers to (`size_of` / `align_of`'s `Type` argument resolution
/// per `04-specs-comptime.md` §2.9).
fn record_comptime_type_path(ic: &mut InferCx<'_>, span: Span, ty: TyId) {
    ic.comptime_type_paths.insert(span, ty);
}

/// Synthesise a literal's type per the locked rules:
/// integer → `i64`, float → `f64`, bool → `bool`, string forms →
/// `String`, unit → `()`.
pub(super) fn synth_literal(lit: &Literal, ic: &InferCx<'_>) -> TyId {
    match lit {
        Literal::Int { .. } => ic.lower.ty_interner.prim(Primitive::I64),
        Literal::Float(_) => ic.lower.ty_interner.prim(Primitive::F64),
        Literal::Str(_) => ic.lower.ty_interner.prim(Primitive::String),
        Literal::Bool(_) => ic.lower.ty_interner.prim(Primitive::Bool),
        Literal::Unit => ic.lower.ty_interner.prim(Primitive::Unit),
    }
}

/// Check a literal against `expected`. Integer literals admit any
/// integer primitive whose positive range covers the literal's value.
/// Float literals admit either float primitive (`f32` / `f64`);
/// precision loss for `f32` targets follows the IEEE 754 round-to-
/// nearest rule applied by the codegen-side `f64 -> f32` narrowing.
/// Other literals must structurally agree with their synthesised type.
pub(super) fn check_literal(lit: &Literal, expected: TyId, ic: &mut InferCx<'_>, span: Span) -> TyId {
    let expected_kind = ic.lower.ty_interner.kind(expected);
    if let (Literal::Int { value, .. }, TyKind::Primitive(p)) = (lit, expected_kind)
        && p.is_integer()
    {
        if fits_in_primitive(*value, *p) {
            return expected;
        }
        let value = *value;
        let prim_name = p.name();
        ic.emit_typecheck_error(
            span,
            format!("integer literal `{value}` does not fit in `{prim_name}`"),
        );
        return ic.ty_error();
    }
    if let (Literal::Float(_), TyKind::Primitive(p)) = (lit, expected_kind)
        && p.is_float()
    {
        return expected;
    }
    let synth = synth_literal(lit, ic);
    check_synth_against_expected(synth, expected, ic, span)
}

/// T-Synth-Check bridge: a synthesised type must coincide with the
/// expected one. `never` is admissible at any expected type (it is the
/// uninhabited bottom and contributes no run-time value), matching the
/// `never`-absorption rule used by [`super::ctrl::join_types`].
/// Mismatch emits a diagnostic and yields Error.
pub(super) fn check_synth_against_expected(
    synth: TyId,
    expected: TyId,
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    if synth == expected {
        return expected;
    }
    // Error TyId propagates silently — the failing site has already
    // emitted its own diagnostic.
    let error = ic.ty_error();
    if synth == error || expected == error {
        return error;
    }
    // `never` is the bottom of the type lattice: a diverging expression
    // (return / raise / panic / loop / break / continue) can stand in
    // wherever a value is expected without contributing one. Without
    // this admission, `return if c { return X } else { 0 }` and similar
    // forms reach the diagnostic branch below; the join_types rule (see
    // [`super::ctrl::join_types`]) already encodes the same absorption
    // for the synth path.
    let never = ic.lower.ty_interner.prim(Primitive::Never);
    if synth == never {
        return expected;
    }
    let msg = format!(
        "expected `{}`, found `{}`",
        ic.display_ty(expected),
        ic.display_ty(synth),
    );
    ic.emit_typecheck_error(span, msg);
    error
}

/// Synthesise a path expression.
///
/// Resolution order:
/// 1. Single-segment path that's a name in the ambient `env` — return
///    its type (the `T-Var` rule for params / locals).
/// 2. Otherwise, if a [`crate::lower::LowerCx::package`] is available,
///    consult [`edda_resolve::Resolutions`] keyed by `path.span`:
///    - **Function** — synthesise [`crate::TyKind::FnPtr`] from the
///      recorded [`crate::FnSig`] via [`crate::FnSig::to_fn_ptr_sig`].
///      The direct-call form `f(x)` is special-cased in
///      [`super::call::synth_call`]; this arm fires only when a
///      function name appears as a value.
///    - TypeDecl variant constructor — synthesise the nominal type for
///      unit payloads; sum-variant payloads still require a call site.
///    - TypeDecl / Spec / ImportLeaf / Const — diagnose with the
///      binding's kind in the message.
///    - [`edda_resolve::Resolved::Module`] — diagnose ("module name in
///      value position").
///    - [`edda_resolve::Resolved::Error`] — cascade silently.
/// 3. No env / no resolution match — diagnose "cannot find binding".
pub(super) fn synth_path(path: &crate::hir::HirPath, env: &TyEnv, ic: &mut InferCx<'_>, span: Span) -> TyId {
    use edda_resolve::{BindingKind, Resolved};
    // Single-segment env lookup — the T-Var rule (`inference-rules.md
    // §1a.4`). Per §4, reading the binding requires its state to be
    // `Valid`; an `Uninit` or `Consumed` binding emits a diagnostic
    // and yields `Error` so the caller does not propagate a misused
    // type.
    if path.segments.len() == 1
        && let Some(t) = env.lookup(path.segments[0].name)
    {
        let sym = path.segments[0].name;
        match env.lookup_state(sym) {
            Some(BindingState::Valid) | None => return t,
            Some(state) => {
                let name = ic.lower.interner.resolve(sym).to_string();
                ic.emit_typecheck_error(
                    span,
                    format!("binding `{name}` is {} at this point", state.describe()),
                );
                return ic.ty_error();
            }
        }
    }

    // Comptime type-as-value (§C10 — `04-specs-comptime.md` *Comptime
    // values* and *Comptime built-ins*). Inside a `comptime` body the
    // path may name a primitive (`u32`, `i64`, `bool`, ...) or a user
    // type — it produces a `Value::Type` at evaluation. The expression's
    // surface type is the `Type` meta-primitive. We try the primitive
    // catalogue first because primitive names never resolve in the
    // resolver (they are catalogue heads, returned as `Resolved::Error`).
    if ic.in_comptime_context
        && path.segments.len() == 1
        && let Some(p) = crate::prim::Primitive::from_name(
            ic.lower.interner.resolve(path.segments[0].name),
        )
    {
        // Track which primitive this resolved to so the comptime
        // evaluator + MIR lowering can convert this path into a
        // `Value::Type(<concrete TyId>)`. Both single-arm record paths
        // dispatch through `record_comptime_type_path`.
        record_comptime_type_path(ic, path.span, ic.lower.ty_interner.prim(p));
        return ic.lower.ty_interner.prim(crate::prim::Primitive::Type);
    }

    // Comptime capability-as-value. A
    // bare capability name (`Subprocess`, `Filesystem`, ...) is a
    // catalogue head — the resolver stays silent — so it never reaches
    // the Resolutions branch below. Inside a `comptime` body it is the
    // type-arg of `target.supports(Cap)`: record its
    // `TyKind::Capability` handle so the evaluator recovers it as a
    // `Value::Type`, and surface the path as the `Type` meta-primitive.
    if ic.in_comptime_context
        && path.segments.len() == 1
        && let Some(c) = crate::capability::CapabilityType::from_name(
            ic.lower.interner.resolve(path.segments[0].name),
        )
    {
        record_comptime_type_path(ic, path.span, ic.lower.ty_interner.capability(c));
        return ic.lower.ty_interner.prim(crate::prim::Primitive::Type);
    }

    // Consult Resolutions when available.
    if let Some(package) = ic.lower.package
        && let Some(resolved) = package.resolutions().lookup_path(path.span)
    {
        return match resolved {
            Resolved::Binding(id) => {
                let entry = package.binding(id);
                let name = ic.lower.interner.resolve(entry.name).to_string();
                // Comptime type-as-value for any TypeDecl / SpecInvocation
                // path — single-segment (`T` bare or `MyType`) and multi-
                // segment (`module.MyType` after spec substitution rewrites
                // `T` to its bound qualified name) alike. Lives BEFORE the
                // sum-variant branch so `alloc_array(diagnostics.Diagnostic, n)`
                // in a monomorphised artifact does not mis-fire the
                // "no variant `Diagnostic` on sum type" path (which would
                // happen for sums) or fall through to the
                // "type X cannot be used as a value" rejection (which
                // happens for products) per B-017.
                if ic.in_comptime_context
                    && matches!(entry.kind, BindingKind::TypeDecl | BindingKind::SpecInvocation)
                {
                    let nominal = ic
                        .lower
                        .ty_interner
                        .intern_kind(TyKind::Nominal(id));
                    record_comptime_type_path(ic, path.span, nominal);
                    return ic.lower.ty_interner.prim(Primitive::Type);
                }
                // Sum-type variant constructor in value position:
                // `<Type>.<variant>` resolves to a `BindingKind::TypeDecl`
                // (user-declared sum) or a `BindingKind::SpecInvocation`
                // (spec-instantiated sum, e.g. `Option_i32`) whose
                // layout in [`TyCx`] is a sum. The variant name is the
                // last path segment. Unit-payload variants synthesise
                // the nominal type directly; tuple/struct-payload
                // variants require a call site
                // (`<Type>.<variant>(args)`) and are routed through
                // `synth_call`.
                if matches!(entry.kind, BindingKind::TypeDecl | BindingKind::SpecInvocation)
                    && path.segments.len() >= 2
                    && let Some(info) = ic.ty_cx.type_decl(id)
                    && let TypeDeclShape::Sum { .. } = &info.kind
                {
                    let variant_sym = path.segments.last().unwrap().name;
                    if let Some(variant) = info.variant(variant_sym) {
                        return match &variant.payload {
                            VariantPayloadInfo::Unit => ic
                                .lower
                                .ty_interner
                                .intern_kind(TyKind::Nominal(id)),
                            VariantPayloadInfo::Tuple { .. }
                            | VariantPayloadInfo::Struct { .. } => {
                                let variant_text =
                                    ic.lower.interner.resolve(variant_sym).to_string();
                                ic.emit_typecheck_error(
                                    span,
                                    format!(
                                        "variant `{name}.{variant_text}` has a payload — \
                                         construct it with `{name}.{variant_text}(...)` \
                                         instead of using the bare path",
                                    ),
                                );
                                ic.ty_error()
                            }
                        };
                    } else {
                        let variant_text =
                            ic.lower.interner.resolve(variant_sym).to_string();
                        ic.emit_typecheck_error(
                            span,
                            format!("no variant `{variant_text}` on sum type `{name}`"),
                        );
                        return ic.ty_error();
                    }
                }
                // Function binding in value position: synthesise a
                // `function(...)` fn-ptr type from the recorded
                // signature. `synth_call` still special-cases the
                // direct-call form, so this path only matters when a
                // function name appears as a value (`let h = f`,
                // `pass(f)`, etc.).
                if entry.kind == BindingKind::Function
                    && let Some(sig) = ic.ty_cx.sig(id)
                {
                    let fn_ptr_sig = sig.to_fn_ptr_sig();
                    return ic.lower.ty_interner.fn_ptr(fn_ptr_sig);
                }
                // Module-level `let` constant in value position:
                // return the declared annotation type recorded by
                // `build_ty_cx`. The initialiser's comptime-pure
                // evaluation is a separate wave's concern; for
                // type-checking purposes the binding behaves like a
                // value of its annotated type.
                if entry.kind == BindingKind::Const
                    && let Some(ty) = ic.ty_cx.const_ty(id)
                {
                    return ty;
                }
                let kind_label = match entry.kind {
                    BindingKind::Function => "function",
                    BindingKind::TypeDecl => "type",
                    BindingKind::Spec => "spec",
                    BindingKind::SpecInvocation => "spec invocation",
                    BindingKind::ImportLeaf => "module",
                    BindingKind::Const => "constant",
                    // Params / Locals would have hit the env lookup
                    // above; reaching this branch means the env is out
                    // of sync (a test-only scenario).
                    BindingKind::Param | BindingKind::Local => {
                        ic.emit_typecheck_error(
                            span,
                            format!("binding `{name}` is not in the current type environment"),
                        );
                        return ic.ty_error();
                    }
                };
                ic.emit_typecheck_error(
                    span,
                    format!(
                        "{kind_label} `{name}` cannot be used as a value (no first-class {kind_label}s)",
                    ),
                );
                ic.ty_error()
            }
            Resolved::Module(_) => {
                ic.emit_typecheck_error(
                    span,
                    "module name used in value position; expected an expression",
                );
                ic.ty_error()
            }
            Resolved::Error => ic.ty_error(),
        };
    }

    // Fallback: report by segment shape.
    if path.segments.len() == 1 {
        let resolved_name = ic
            .lower
            .interner
            .resolve(path.segments[0].name)
            .to_string();
        ic.emit_typecheck_error(span, format!("cannot find binding `{resolved_name}` in scope"));
    } else {
        ic.emit_typecheck_error(
            span,
            "qualified-path expressions are not yet inferred",
        );
    }
    ic.ty_error()
}
