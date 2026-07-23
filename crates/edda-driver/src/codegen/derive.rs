//! derive desugaring: expand `derive ... for T` into codegen RootInvocations
//! and seed the eq-cascade roots.


use edda_codegen::{
    Argument, ArgumentTuple,
};
use edda_diag::{Diagnostics, LintConfig};
use edda_intern::Interner;
use edda_resolve::{
    BindingId, BindingKind, Resolved, ResolvedPackage,
    derive_spec_target,
};
use edda_span::Span;
use edda_syntax::ast::{
    Derive, Path as AstPath,
    Visibility,
};
use edda_types::{
    ImplicitSpec, TyCx, TyId, TyInterner,
    TypedSpecInvocation,
};
use smol_str::SmolStr;


use super::arguments::{expr_to_argument, ty_id_to_argument};
use super::eq_comparator::{CollectionKind, collection_of, for_each_eq_cascade_type};
use super::{
    RootInvocation, SpecLookup, binding_qualified_name,
    emit_typecheck, find_spec_decl, resolve_path_to_qualified, spec_invocation_qualified_name,
};

/// Expand one `derive <items> for <Type>` declaration into one
/// [`RootInvocation`] per admitted item.
///
/// Unknown derive items have already been diagnosed by the resolver's
/// [`crate::cascade`]-time `walk_derive`; this pass silently skips them
/// to avoid double-reporting. Items whose stdlib spec module did not
/// land in [`ResolvedPackage`] (e.g. because the stdlib catalogue is
/// out-of-date) surface a precise codegen diagnostic at the item span.
pub(super) fn collect_derive(
    derive: &Derive,
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
    ty_cx: &TyCx,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
    out: &mut Vec<RootInvocation>,
    eq_cascade_visited: &mut std::collections::HashSet<BindingId>,
    debug_cascade_visited: &mut std::collections::HashSet<BindingId>,
) {
    // Resolve the target type's path once for all items. The resolver
    // recorded this via `walk_derive`; absent resolutions surface as
    // `Resolved::Error` and we emit the matching codegen diagnostic.
    let target_arg = match derive_target_argument(&derive.target, resolved, interner, ty_interner) {
        Ok(a) => a,
        Err(reason) => {
            emit_typecheck(
                diags,
                lint_cfg,
                derive.target.span,
                format!("codegen: `derive` target type cannot be materialised — {reason}"),
            );
            return;
        }
    };
    // Target type's resolver binding — keys the typechecker's
    // `TypeDeclInfo` so the `eq` comparator can be synthesised from the
    // type's field / variant structure.
    let target_binding = match resolved.resolutions().lookup_path(derive.target.span) {
        Some(Resolved::Binding(id)) => Some(id),
        _ => None,
    };

    for item in &derive.items {
        if item.name == edda_intern::Symbol::DUMMY {
            continue;
        }
        let name_text = interner.resolve(item.name);
        let Some(target) = derive_spec_target(name_text) else {
            // resolver already emitted `derive_unknown`; skip silently.
            continue;
        };
        let qualified = target.qualified();
        let found = match find_spec_decl(&qualified, resolved, interner) {
            Some(f) => f,
            None => {
                emit_typecheck(
                    diags,
                    lint_cfg,
                    item.span,
                    format!(
                        "codegen: stdlib spec `{qualified}` is not available — `derive {name_text}` skipped"
                    ),
                );
                continue;
            }
        };
        // For `derive eq`, synthesise a concrete structural comparator
        // from the target's `TypeDeclInfo` and route it through the same
        // instantiation path as the placeholder spec; fall back to the
        // stdlib placeholder (`return false`) for shapes the synthesiser
        // does not yet cover. The generated
        // comparator compares nested nominal fields with `a.f == b.f`, so
        // every such field type also needs its own materialised comparator
        // — emit the transitive closure as additional roots.
        if name_text == "eq" {
            let spec_decl = target_binding
                .and_then(|b| ty_cx.type_decl(b))
                .and_then(|info| crate::derive_eq::synthesize_eq_spec(info, interner))
                .unwrap_or_else(|| found.spec.clone());
            out.push(RootInvocation {
                spec_qualified: SmolStr::new(&qualified),
                args: ArgumentTuple::new(vec![target_arg.clone()]),
                spec_decl,
                source_path: found.source_path.clone(),
                parent_imports: found.parent_imports.clone(),
                parent_qualified: found.parent_qualified.clone(),
                parent_sibling_names: found.parent_sibling_names.clone(),
            });
            if let Some(root_binding) = target_binding {
                emit_eq_cascade_roots(
                    root_binding,
                    &found,
                    resolved,
                    interner,
                    ty_interner,
                    ty_cx,
                    eq_cascade_visited,
                    out,
                );
            }
            continue;
        }
        // For `derive debug`, synthesise a concrete structural formatter
        // from the target's `TypeDeclInfo` (a `format(v: T) -> String`
        // that folds each field / variant through `string_concat` +
        // single-slot f-strings) and route it through the same
        // instantiation path; fall back to the stdlib `?`-byte placeholder
        // for shapes the synthesiser cannot parse.
        // The formatter's f-string slots dispatch nested aggregates to
        // their own `debug_<FieldT>.format`, so every such field type also
        // needs its own materialised formatter — emit the transitive
        // closure as additional roots (mirror of the eq cascade).
        if name_text == "debug" {
            let spec_decl = target_binding
                .and_then(|b| ty_cx.type_decl(b).map(|info| (b, info)))
                .and_then(|(b, info)| {
                    let type_name = interner.resolve(resolved.binding(b).name);
                    crate::derive_debug::synthesize_debug_spec(info, type_name, interner)
                })
                .unwrap_or_else(|| found.spec.clone());
            out.push(RootInvocation {
                spec_qualified: SmolStr::new(&qualified),
                args: ArgumentTuple::new(vec![target_arg.clone()]),
                spec_decl,
                source_path: found.source_path.clone(),
                parent_imports: found.parent_imports.clone(),
                parent_qualified: found.parent_qualified.clone(),
                parent_sibling_names: found.parent_sibling_names.clone(),
            });
            if let Some(root_binding) = target_binding {
                emit_debug_cascade_roots(
                    root_binding,
                    &found,
                    resolved,
                    interner,
                    ty_interner,
                    ty_cx,
                    debug_cascade_visited,
                    out,
                );
            }
            continue;
        }
        out.push(RootInvocation {
            spec_qualified: SmolStr::new(qualified),
            args: ArgumentTuple::new(vec![target_arg.clone()]),
            spec_decl: found.spec,
            source_path: found.source_path,
            parent_imports: found.parent_imports,
            parent_qualified: found.parent_qualified,
            parent_sibling_names: found.parent_sibling_names,
        });
    }
}

//   nominal type reached from any `derive eq` target is materialised once
//   per cascade; the root type itself is recorded by the explicit root in
//   `collect_derive` and skipped here (its comparator is already enqueued)
//   context (`compare_lookup`) but carries the per-type synthesised
//   `spec_decl` and a single `Argument::Type(<qualified type>)`, so the
//   producer composes the same `eq_<T>_<hex>` module path the map resolves
//   whose comparator cannot be synthesised is skipped — the bounded residue
//   surfaced later as a MIR lowering error on the one offending
//   `==`, never a panic
/// Enqueue a `std.core.compare.eq(S)` codegen root for every nominal type
/// `S` transitively reachable (through field / payload types) from
/// `root_binding`'s declaration, so each nested `a.f == b.f` in a
/// synthesised comparator resolves to a real materialised comparator.
/// `compare_lookup` is the
/// already-resolved `std.core.compare.eq` spec context, reused for parent
/// imports / siblings.
#[allow(clippy::too_many_arguments)]
fn emit_eq_cascade_roots(
    root_binding: BindingId,
    compare_lookup: &SpecLookup,
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
    ty_cx: &TyCx,
    visited: &mut std::collections::HashSet<BindingId>,
    out: &mut Vec<RootInvocation>,
) {
    // Collect the reachable nominal types first so the closure walk does
    // not borrow `out` / `resolved` re-entrantly. The root type is added
    // to `visited` by the walk but its comparator is already enqueued by
    // the explicit root, so it is excluded from `cascade_types`.
    let mut cascade_types: Vec<BindingId> = Vec::new();
    for_each_eq_cascade_type(root_binding, ty_cx, ty_interner, visited, &mut |binding| {
        if binding != root_binding {
            cascade_types.push(binding);
        }
    });
    // Resolve the four element-wise collection comparator specs once for this
    // cascade. `None` (a stdlib without those specs) degrades to the
    // earlier behaviour: the collection `==` stays the bounded residue.
    let vec_eq = find_spec_decl(CollectionKind::Vec.compare_spec(), resolved, interner);
    let option_eq = find_spec_decl(CollectionKind::Option.compare_spec(), resolved, interner);
    let box_eq = find_spec_decl(CollectionKind::Box.compare_spec(), resolved, interner);
    let intmap_eq = find_spec_decl(CollectionKind::IntMap.compare_spec(), resolved, interner);
    for type_binding in cascade_types {
        // A materialised `Vec_<E>` / `Option_<E>` / `Box_<E>` / `IntMap_<V>`
        // field delegates its element-wise `==` to the stdlib
        // `std.core.compare.{VecEq,OptionEq,BoxEq,IntMapEq}(E)` spec instead
        // of a per-instantiation structural comparator (which cannot be named
        // cross-module). The element `E` (the value type `V` for `IntMap`)
        // comes straight from the materialising spec invocation's argument.
        if let Some((kind, elem_arg)) =
            collection_of(type_binding, resolved, interner, ty_interner, ty_cx)
        {
            let lookup = match kind {
                CollectionKind::Vec => vec_eq.as_ref(),
                CollectionKind::Option => option_eq.as_ref(),
                CollectionKind::Box => box_eq.as_ref(),
                CollectionKind::IntMap => intmap_eq.as_ref(),
            };
            if let Some(lookup) = lookup {
                out.push(RootInvocation {
                    spec_qualified: SmolStr::new(kind.compare_spec()),
                    args: ArgumentTuple::new(vec![elem_arg]),
                    spec_decl: lookup.spec.clone(),
                    source_path: lookup.source_path.clone(),
                    parent_imports: lookup.parent_imports.clone(),
                    parent_qualified: lookup.parent_qualified.clone(),
                    parent_sibling_names: lookup.parent_sibling_names.clone(),
                });
            }
            continue;
        }
        // A `derive eq` comparator lives in `std.core.compare.*` and can only
        // reference a `public` target type; a non-public, non-collection
        // materialised instantiation has no nameable comparator, so it stays
        // the bounded residue.
        if resolved.binding(type_binding).visibility != Visibility::Public {
            continue;
        }
        let Some(info) = ty_cx.type_decl(type_binding) else {
            continue;
        };
        let Some(spec_decl) = crate::derive_eq::synthesize_eq_spec(info, interner) else {
            continue;
        };
        let type_qualified =
            binding_qualified_name(resolved.binding(type_binding), resolved, interner);
        out.push(RootInvocation {
            spec_qualified: SmolStr::new("std.core.compare.eq"),
            args: ArgumentTuple::new(vec![Argument::Type(SmolStr::new(type_qualified))]),
            spec_decl,
            source_path: compare_lookup.source_path.clone(),
            parent_imports: compare_lookup.parent_imports.clone(),
            parent_qualified: compare_lookup.parent_qualified.clone(),
            parent_sibling_names: compare_lookup.parent_sibling_names.clone(),
        });
    }
}

//   (`build_debug_formatter_map`) walks, so a nominal type reached from any
//   `derive debug` target materialises one `debug_<S>` formatter per cascade;
//   the root type itself is recorded by the explicit root in `collect_derive`
//   and skipped here (its formatter is already enqueued)
//   context (`debug_lookup`) but carries the per-type synthesised `spec_decl`
//   and a single `Argument::Type(<qualified type>)`, so the producer composes
//   the same `debug_<S>_<hex>` module path the map resolves
//   formatter source cannot be parsed is skipped — the field's f-string slot
//   then falls through to the first-word fallback in the MIR fold, never a panic
/// Enqueue a `std.core.fmt.debug(S)` codegen root for every nominal type
/// `S` transitively reachable (through field / payload types) from
/// `root_binding`'s declaration, so each nested aggregate slot in a
/// synthesised formatter resolves to a real materialised formatter.
/// `debug_lookup` is the already-resolved
/// `std.core.fmt.debug` spec context, reused for parent imports / siblings.
#[allow(clippy::too_many_arguments)]
fn emit_debug_cascade_roots(
    root_binding: BindingId,
    debug_lookup: &SpecLookup,
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
    ty_cx: &TyCx,
    visited: &mut std::collections::HashSet<BindingId>,
    out: &mut Vec<RootInvocation>,
) {
    // Collect the reachable nominal types first so the closure walk does
    // not borrow `out` / `resolved` re-entrantly. The root type is added
    // to `visited` by the walk but its formatter is already enqueued by
    // the explicit root, so it is excluded from `cascade_types`.
    let mut cascade_types: Vec<BindingId> = Vec::new();
    for_each_eq_cascade_type(root_binding, ty_cx, ty_interner, visited, &mut |binding| {
        if binding != root_binding {
            cascade_types.push(binding);
        }
    });
    // Resolve the four element-wise collection debug specs once for this
    // cascade. `None` (a stdlib without those specs) degrades to the
    // pre-routing behaviour: the collection slot stays the first-word fallback.
    let vec_debug = find_spec_decl(CollectionKind::Vec.debug_spec(), resolved, interner);
    let option_debug = find_spec_decl(CollectionKind::Option.debug_spec(), resolved, interner);
    let box_debug = find_spec_decl(CollectionKind::Box.debug_spec(), resolved, interner);
    let intmap_debug = find_spec_decl(CollectionKind::IntMap.debug_spec(), resolved, interner);
    for type_binding in cascade_types {
        // A materialised `Vec_<E>` / `Option_<E>` / `Box_<E>` / `IntMap_<V>`
        // field delegates its element-wise debug rendering to the stdlib
        // `std.core.fmt.{VecDebug,OptionDebug,BoxDebug,IntMapDebug}(E)` spec
        // instead of a per-instantiation structural formatter (which cannot be
        // named cross-module). The element `E` (the value type `V` for
        // `IntMap`) comes straight from the materialising spec invocation's
        // argument — exactly parallel to the eq cascade.
        if let Some((kind, elem_arg)) =
            collection_of(type_binding, resolved, interner, ty_interner, ty_cx)
        {
            let lookup = match kind {
                CollectionKind::Vec => vec_debug.as_ref(),
                CollectionKind::Option => option_debug.as_ref(),
                CollectionKind::Box => box_debug.as_ref(),
                CollectionKind::IntMap => intmap_debug.as_ref(),
            };
            if let Some(lookup) = lookup {
                out.push(RootInvocation {
                    spec_qualified: SmolStr::new(kind.debug_spec()),
                    args: ArgumentTuple::new(vec![elem_arg]),
                    spec_decl: lookup.spec.clone(),
                    source_path: lookup.source_path.clone(),
                    parent_imports: lookup.parent_imports.clone(),
                    parent_qualified: lookup.parent_qualified.clone(),
                    parent_sibling_names: lookup.parent_sibling_names.clone(),
                });
            }
            continue;
        }
        // A `debug` formatter lives in `std.core.fmt.*` and references the
        // target by its substituted qualified name, so a non-public target
        // has no nameable formatter — leave its slot as the first-word
        // fallback (mirror of the eq cascade's public gate).
        if resolved.binding(type_binding).visibility != Visibility::Public {
            continue;
        }
        let Some(info) = ty_cx.type_decl(type_binding) else {
            continue;
        };
        let type_name = interner.resolve(resolved.binding(type_binding).name);
        let Some(spec_decl) = crate::derive_debug::synthesize_debug_spec(info, type_name, interner)
        else {
            continue;
        };
        let type_qualified =
            binding_qualified_name(resolved.binding(type_binding), resolved, interner);
        out.push(RootInvocation {
            spec_qualified: SmolStr::new("std.core.fmt.debug"),
            args: ArgumentTuple::new(vec![Argument::Type(SmolStr::new(type_qualified))]),
            spec_decl,
            source_path: debug_lookup.source_path.clone(),
            parent_imports: debug_lookup.parent_imports.clone(),
            parent_qualified: debug_lookup.parent_qualified.clone(),
            parent_sibling_names: debug_lookup.parent_sibling_names.clone(),
        });
    }
}

/// Lower a `derive` target Path into an [`Argument::Type`].
///
/// Resolves the target through the package-wide path resolution map (the
/// canonical source of the qualified name); `ty_interner` is forwarded
/// only so a SpecInvocation target can recurse into
/// [`spec_invocation_qualified_name`], which needs it to rebuild the
/// argument tuple via [`expr_to_argument`].
pub(super) fn derive_target_argument(
    path: &AstPath,
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
) -> Result<Argument, String> {
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
                BindingKind::SpecInvocation => spec_invocation_qualified_name(
                    binding, resolved, interner, ty_interner,
                )
                .map(|q| Argument::Type(SmolStr::new(q)))
                .ok_or_else(|| {
                    "spec-invocation target's parent spec path did not resolve at \
                     typecheck time"
                        .to_string()
                }),
                other => Err(format!(
                    "target resolves to a {other:?} — `derive` requires a type"
                )),
            }
        }
        Some(Resolved::Module(_)) => Err("target resolves to a module, not a type".to_string()),
        Some(Resolved::Error) | None => Err("target path did not resolve".to_string()),
    }
}

/// Build a `RootInvocation` for an implicit spec request.
///
/// All [`ImplicitSpec`] kinds route uniformly through
/// `kind.qualified_name()` + `find_spec_decl`: `Option` resolves to
/// `stdlib/lib/core/option/` and `Range` to `stdlib/lib/core/range/`
/// (both authored). A kind whose stdlib spec is absent is skipped with a
/// diagnostic at the invocation site.
pub(super) fn collect_implicit(
    kind: ImplicitSpec,
    type_arg: TyId,
    span: Span,
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<RootInvocation> {
    let qualified = kind.qualified_name();
    let arg = match ty_id_to_argument(type_arg, ty_interner, resolved, interner) {
        Ok(a) => a,
        Err(reason) => {
            emit_typecheck(
                diags,
                lint_cfg,
                span,
                format!(
                    "codegen: cannot materialise implicit `{qualified}` — {reason}"
                ),
            );
            return None;
        }
    };

    let found = match find_spec_decl(qualified, resolved, interner) {
        Some(found) => found,
        None => {
            emit_typecheck(
                diags,
                lint_cfg,
                span,
                format!("codegen: stdlib spec `{qualified}` is not available — implicit invocation skipped"),
            );
            return None;
        }
    };

    Some(RootInvocation {
        spec_qualified: SmolStr::new(qualified),
        args: ArgumentTuple::new(vec![arg]),
        spec_decl: found.spec,
        source_path: found.source_path,
        parent_imports: found.parent_imports,
        parent_qualified: found.parent_qualified,
        parent_sibling_names: found.parent_sibling_names,
    })
}

/// Build a `RootInvocation` for an explicit `spec Path(args)` directive.
///
/// Only argument expressions whose AST shape is a single-segment
/// primitive name (`i32`, `bool`, `String`, …) or a resolved path that
/// names a top-level type binding are handled. Anything else emits a
/// typecheck-error diagnostic at the arg's span.
pub(super) fn collect_explicit(
    si: &TypedSpecInvocation,
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<RootInvocation> {
    let qualified = match resolve_path_to_qualified(&si.invocation.path, resolved, interner) {
        Some(name) => name,
        None => {
            emit_typecheck(
                diags,
                lint_cfg,
                si.invocation.span,
                "codegen: spec invocation path does not resolve to a spec declaration",
            );
            return None;
        }
    };

    let mut args = Vec::with_capacity(si.invocation.args.len());
    for arg_expr in &si.invocation.args {
        match expr_to_argument(arg_expr, resolved, interner, ty_interner) {
            Ok(a) => args.push(a),
            Err(reason) => {
                emit_typecheck(
                    diags,
                    lint_cfg,
                    arg_expr.span,
                    format!("codegen: spec argument not yet supported — {reason}"),
                );
                return None;
            }
        }
    }

    let found = match find_spec_decl(&qualified, resolved, interner) {
        Some(found) => found,
        None => {
            emit_typecheck(
                diags,
                lint_cfg,
                si.invocation.path.span,
                format!("codegen: cannot locate spec declaration `{qualified}`"),
            );
            return None;
        }
    };

    Some(RootInvocation {
        spec_qualified: SmolStr::new(qualified),
        args: ArgumentTuple::new(args),
        spec_decl: found.spec,
        source_path: found.source_path,
        parent_imports: found.parent_imports,
        parent_qualified: found.parent_qualified,
        parent_sibling_names: found.parent_sibling_names,
    })
}
