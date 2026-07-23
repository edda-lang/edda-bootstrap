//! Spec-argument lowering: TyId / Expr / Path / Literal -> codegen Argument.


use edda_codegen::{
    Argument, PrimitiveValue,
};
use edda_intern::Interner;
use edda_resolve::{
    BindingKind, Resolved, ResolvedPackage,
};
use edda_syntax::ast::{
    Expr, ExprKind, Literal, Path as AstPath,
};
use edda_types::{
    CapabilityType, Primitive, TyId, TyInterner, TyKind,
};
use smol_str::SmolStr;


use super::spec_invocation_qualified_name;

/// Convert an interned [`TyId`] into a spec argument.
///
/// Admits `Argument::Type(qname)` — including capability types,
/// whose canonical argument is their bare source spelling
/// (`Stdout`). Non-representable kinds (tuples, slices, function types,
/// the error sentinel) surface as the `Err` case so the caller can emit
/// a precise diagnostic at the spec invocation site.
pub(super) fn ty_id_to_argument(
    ty: TyId,
    ty_interner: &TyInterner,
    resolved: &ResolvedPackage,
    interner: &Interner,
) -> Result<Argument, String> {
    let kind = ty_interner.kind(ty);
    match kind {
        TyKind::Primitive(p) => Ok(Argument::Type(SmolStr::new(p.name()))),
        TyKind::Nominal(binding_id) => {
            let binding = resolved.binding(*binding_id);
            // Same SpecInvocation-vs-TypeDecl split as
            // `path_to_type_argument`: a SpecInvocation binding's
            // canonical qualified name is the spec's parent module
            // plus the mangled leaf, not `<user_module>.<leaf>`.
            let qualified = match binding.kind {
                BindingKind::SpecInvocation => {
                    spec_invocation_qualified_name(binding, resolved, interner, ty_interner)
                        .ok_or_else(|| {
                            "spec-invocation argument's parent spec path did \
                             not resolve at typecheck time"
                                .to_string()
                        })?
                }
                _ => {
                    let module = resolved.module_entry(binding.module);
                    let mut name = module.canonical_path.to_owned_string(interner);
                    name.push('.');
                    name.push_str(interner.resolve(binding.name));
                    name
                }
            };
            Ok(Argument::Type(SmolStr::new(qualified)))
        }
        TyKind::Tuple(_) => Err("tuple type arguments are not yet supported".to_string()),
        TyKind::Slice(_) => Err("slice type arguments are not yet supported".to_string()),
        // Capability types are pointer-width scalars at the ABI
        // (one register). The canonical argument is the capability's bare
        // source spelling (`Stdout`), which re-lowers to
        // `TyKind::Capability` through the type-annotation shortcut in
        // `edda-types lower/ty/path.rs` when the substituted spec body is
        // re-resolved + re-typechecked in pass-2.
        TyKind::Capability(cap) => Ok(Argument::Type(SmolStr::new(cap.name()))),
        TyKind::FnPtr(_) => {
            Err("function-pointer type arguments are not yet supported".to_string())
        }
        TyKind::Error => Err("argument lowered to the error sentinel upstream".to_string()),
    }
}

/// Lower an explicit-invocation argument expression to an
/// [`Argument`]. Handles the surface forms `i32`, `"some.qualified.Type"`,
/// and primitive integer / bool / string literals directly; everything
/// else returns an explanatory error string.
pub(super) fn expr_to_argument(
    expr: &Expr,
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
) -> Result<Argument, String> {
    match &expr.kind {
        ExprKind::Path(p) => path_to_type_argument(p, resolved, interner, ty_interner),
        ExprKind::Literal(lit) => literal_to_argument(*lit, interner),
        ExprKind::Unary { op, expr: inner }
            if matches!(op, edda_syntax::ast::UnOp::Neg)
                && matches!(inner.kind, ExprKind::Literal(Literal::Int { .. })) =>
        {
            // `-N` literal — admit as a signed integer arg whose
            // width the substitution layer will derive from the
            // spec's declared parameter type. The compile-time
            // value is stored as i64 by default; the substitution
            // layer is responsible for any width-narrowing.
            if let ExprKind::Literal(Literal::Int { value, .. }) = inner.kind {
                let signed = (value as i128).wrapping_neg();
                Ok(Argument::Primitive(PrimitiveValue::I64(signed as i64)))
            } else {
                Err("non-literal unary expression".to_string())
            }
        }
        ExprKind::Binary { .. }
        | ExprKind::Call { .. }
        | ExprKind::MethodCall { .. }
        | ExprKind::Field { .. }
        | ExprKind::TupleIndex { .. }
        | ExprKind::Index { .. }
        | ExprKind::If { .. }
        | ExprKind::Match { .. }
        | ExprKind::Block(_) => Err(
            "composite expressions in spec args require the comptime evaluator (follow-up wave)"
                .to_string(),
        ),
        _ => Err("argument shape not yet admitted by codegen wiring".to_string()),
    }
}

/// Resolve a Path expression into a spec argument — `Argument::Type` for
/// a primitive / type-decl / spec-invocation path, or `Argument::Function`
/// for a path naming a top-level function.
pub(super) fn path_to_type_argument(
    path: &AstPath,
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
) -> Result<Argument, String> {
    // Single-segment primitive / capability name fast path
    // (`i32`, `bool`, `Stdout`, …).
    if path.segments.len() == 1 {
        let text = interner.resolve(path.segments[0].name);
        if let Some(prim) = Primitive::from_name(text) {
            // Round-trip through the interner so the produced name is
            // identical to the spelling the canonical encoder emits.
            let _ = ty_interner.prim(prim);
            return Ok(Argument::Type(SmolStr::new(prim.name())));
        }
        // Capability names are recognised lexically in
        // type-annotation position only (`edda-types lower/ty/path.rs`),
        // so they are NOT resolver bindings and never resolve through the
        // path-lookup branch below. Admit them here, mirroring the
        // primitive fast-path: the canonical argument is the capability's
        // bare spelling, which re-lowers as a capability in the
        // substituted spec body.
        if let Some(cap) = CapabilityType::from_name(text) {
            return Ok(Argument::Type(SmolStr::new(cap.name())));
        }
    }

    // Resolver-mediated lookup. Single-segment paths that resolve to a
    // top-level type binding emit the binding's fully qualified name.
    // SpecInvocation bindings are admitted here for the same reason
    // edda-types/lower/ty.rs admits them as nominal types: the spec
    // invocation's mangled module name (`Box_Payload` for
    // `spec std.mem.alloc.Box(Payload)`) is a usable type name once
    // the sibling spec has materialised, so nesting a spec-generated
    // name inside another spec arg (`spec StringMap(Box_Payload)`)
    // must resolve through the same path as a hand-written type decl.
    // For SpecInvocation bindings the canonical qualified name is the
    // generated module's path — `std.mem.alloc.Box_Payload`, not
    // `<user_module>.Box_Payload` — because the substituted spec body
    // must reach the generated sibling, not the placeholder binding in
    // the user's module (which is module-internal and would surface as
    // an `import_resolution_error` on cross-module access).
    match resolved.resolutions().lookup_path(path.span) {
        Some(Resolved::Binding(id)) => {
            let binding = resolved.binding(id);
            match binding.kind {
                BindingKind::TypeDecl => {
                    let module = resolved.module_entry(binding.module);
                    let mut name = module.canonical_path.to_owned_string(interner);
                    name.push('.');
                    name.push_str(interner.resolve(binding.name));
                    Ok(Argument::Type(SmolStr::new(name)))
                }
                BindingKind::SpecInvocation => {
                    spec_invocation_qualified_name(binding, resolved, interner, ty_interner)
                        .map(|qname| Argument::Type(SmolStr::new(qname)))
                        .ok_or_else(|| {
                            "spec-invocation argument's parent spec path did \
                             not resolve at typecheck time"
                                .to_string()
                        })
                }
                // A path resolving to a top-level function is a
                // function-reference argument bound to a
                // `comptime f: function(...)` spec parameter. Its qname is
                // the declaring module's canonical path + leaf — identical
                // to the TypeDecl form, and to the resolver-side reciprocal
                // `arg_type_qname_for_hash`, so the disambig hash agrees.
                BindingKind::Function => {
                    let module = resolved.module_entry(binding.module);
                    let mut name = module.canonical_path.to_owned_string(interner);
                    name.push('.');
                    name.push_str(interner.resolve(binding.name));
                    Ok(Argument::Function(SmolStr::new(name)))
                }
                other => Err(format!(
                    "argument path resolves to a {other:?} — codegen admits type and function arguments"
                )),
            }
        }
        Some(Resolved::Module(_)) => {
            Err("path resolves to a module, not a type or value".to_string())
        }
        Some(Resolved::Error) | None => {
            Err("path did not resolve at typecheck time".to_string())
        }
    }
}

/// Lower a primitive literal to an `Argument::Primitive`.
///
/// Integer literals default to `i64` width; the substitution layer is
/// responsible for any narrowing required by the spec parameter.
pub(super) fn literal_to_argument(lit: Literal, interner: &Interner) -> Result<Argument, String> {
    match lit {
        Literal::Int { value, .. } => Ok(Argument::Primitive(PrimitiveValue::I64(value as i64))),
        Literal::Bool(b) => Ok(Argument::Primitive(PrimitiveValue::Bool(b))),
        Literal::Str(sym) => Ok(Argument::Primitive(PrimitiveValue::String(SmolStr::new(
            interner.resolve(sym),
        )))),
        Literal::Float(_) => Err("float literal arguments are not yet supported".to_string()),
        Literal::Unit => Err("unit literal is not a spec argument".to_string()),
    }
}
