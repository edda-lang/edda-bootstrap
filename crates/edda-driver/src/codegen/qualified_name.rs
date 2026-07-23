//! Qualified-name and disambig-suffix resolution for spec-invocation /
//! derive bindings. The `<leaf>_<8hex>` suffix computed here must stay
//! byte-for-byte identical to `edda_codegen::compose_module_path` so the
//! substituted-body and artifact-emission passes name the same module.

use edda_codegen::{Argument, ArgumentTuple, module_disambig_hex};
use edda_intern::Interner;
use edda_resolve::{BindingEntry, Resolved, ResolvedPackage, mangle_spec_invocation_name};
use edda_syntax::ast::{Derive, Expr, ItemKind, Path as AstPath};
use edda_types::TyInterner;

use super::arguments::expr_to_argument;
use super::derive::derive_target_argument;

/// Compose the canonical qualified name of a `SpecInvocation` binding.
///
/// The binding's `name` is the mangled short name (`Box_Payload`); the
/// binding's `module` is the user module that hosts the `spec ...`
/// directive. The generated artifact, however, materialises as a
/// sibling of the *spec's* parent module — for
/// `spec std.mem.alloc.Box(Payload)` the artifact is
/// `std.mem.alloc.Box_Payload_<8hex>`, not `<user_module>.Box_Payload`.
///
/// We walk the binding module's AST to find the matching
/// `SpecInvocation` item (matched by the mangled-name equality already
/// in use by the resolver), resolve the spec's path to its qualified
/// form, take its parent, append the binding's mangled name, then
/// append the same 8-hex disambig suffix the codegen-side
/// [`edda_codegen::compose_module_path`] uses on the artifact's
/// `module` declaration. The hex is computed by re-lowering `si.args`
/// through [`expr_to_argument`] (the same lowering the artifact-emission
/// pass uses) and feeding the resulting `ArgumentTuple` into the
/// codegen-side [`edda_codegen::module_disambig_hex`]. This makes the
/// two emit sites bytewise identical by construction.
///
/// Returns `None` if no matching `SpecInvocation` item exists or its
/// spec path failed to resolve at typecheck time.
pub(super) fn spec_invocation_qualified_name(
    binding: &BindingEntry,
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
) -> Option<String> {
    let module = resolved.module_entry(binding.module);
    let leaf_bare = interner.resolve(binding.name);
    for item in &module.ast.items {
        match &item.kind {
            ItemKind::SpecInvocation(si) => {
                let mangled = mangle_spec_invocation_name(si, interner)?;
                if mangled != binding.name {
                    continue;
                }
                let spec_qualified =
                    resolve_path_to_qualified(&si.path, resolved, interner)?;
                let parent =
                    spec_qualified.rsplit_once('.').map(|(p, _)| p).unwrap_or("");
                let leaf =
                    disambig_leaf(&spec_qualified, leaf_bare, &si.args, resolved, interner, ty_interner);
                let mut out = String::with_capacity(parent.len() + 1 + leaf.len());
                if !parent.is_empty() {
                    out.push_str(parent);
                    out.push('.');
                }
                out.push_str(&leaf);
                return Some(out);
            }
            // `derive` registers its `<item>_<target-leaf>` bindings at
            // `items.rs::register_derive_bindings`. The qualified form
            // is the stdlib spec's parent module + the mangled name +
            // the disambig hex.
            ItemKind::Derive(d) => {
                if let Some(out) =
                    derive_binding_qualified_name(d, leaf_bare, resolved, interner, ty_interner)
                {
                    return Some(out);
                }
            }
            _ => {}
        }
    }
    None
}

/// Compute the `<leaf>_<8hex>` (or bare `<leaf>` on failure) suffix that
/// `compose_module_path` would emit for `(spec_qualified, args)`.
///
/// Routes args through [`expr_to_argument`] — the exact lowering the
/// artifact-emission pass uses — and feeds the resulting tuple into the
/// codegen-side [`edda_codegen::module_disambig_hex`]. Argument shapes
/// the codegen lowering rejects (composite expressions, EffectRow,
/// UserDefined) fall back to bare leaf, matching `compose_module_path`'s
/// own fallback.
fn disambig_leaf(
    spec_qualified: &str,
    leaf_bare: &str,
    args: &[Expr],
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
) -> String {
    let mut lowered: Vec<Argument> = Vec::with_capacity(args.len());
    for arg in args {
        match expr_to_argument(arg, resolved, interner, ty_interner) {
            Ok(a) => lowered.push(a),
            Err(_) => return leaf_bare.to_string(),
        }
    }
    let tuple = ArgumentTuple::new(lowered);
    match module_disambig_hex(spec_qualified, &tuple) {
        Some(hex) => format!("{leaf_bare}_{hex}"),
        None => leaf_bare.to_string(),
    }
}

/// If `leaf` matches an `<item>_<target-leaf>` pair on the given `Derive`,
/// return the qualified module path the codegen artifact materialises
/// under (e.g. `std.core.compare.eq_Point_<8hex>` for `derive eq for Point`).
///
/// Mirrors the input the codegen-side artifact-emission pass produces
/// for the synthesised `spec <item>(<target>)` invocation: the derive
/// target path lowers to a single `Argument::Type` via
/// [`derive_target_argument`], and the resulting `ArgumentTuple` feeds
/// the same [`edda_codegen::module_disambig_hex`] the producer side
/// uses. The two sides cannot disagree by construction.
fn derive_binding_qualified_name(
    derive: &Derive,
    leaf: &str,
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
) -> Option<String> {
    let target_seg = derive.target.segments.last()?;
    if target_seg.name == edda_intern::Symbol::DUMMY {
        return None;
    }
    let target_text = interner.resolve(target_seg.name);
    for item in &derive.items {
        if item.name == edda_intern::Symbol::DUMMY {
            continue;
        }
        let item_text = interner.resolve(item.name);
        let candidate = format!("{item_text}_{target_text}");
        if candidate != leaf {
            continue;
        }
        let target = edda_resolve::derive_spec_target(item_text)?;
        // Build the per-derive synthesised spec qname (`std.<path>.<item>`),
        // then route the target lowering through the same
        // `derive_target_argument` the codegen-side `collect_derive` uses.
        // Whatever `Argument` falls out of there is what the producer
        // pass also sees — so the hash agrees.
        let mut spec_qualified = String::with_capacity(64);
        for (i, seg) in target.module_segments.iter().enumerate() {
            if i > 0 {
                spec_qualified.push('.');
            }
            spec_qualified.push_str(seg);
        }
        spec_qualified.push('.');
        spec_qualified.push_str(item_text);
        let suffixed =
            match derive_target_argument(&derive.target, resolved, interner, ty_interner) {
                Ok(target_arg) => {
                    let tuple = ArgumentTuple::new(vec![target_arg]);
                    match module_disambig_hex(&spec_qualified, &tuple) {
                        Some(hex) => format!("{candidate}_{hex}"),
                        None => candidate.clone(),
                    }
                }
                Err(_) => candidate.clone(),
            };
        let mut out = String::with_capacity(64);
        for (i, seg) in target.module_segments.iter().enumerate() {
            if i > 0 {
                out.push('.');
            }
            out.push_str(seg);
        }
        out.push('.');
        out.push_str(&suffixed);
        return Some(out);
    }
    None
}


/// Resolve an AST [`AstPath`] to its qualified name.
pub(super) fn resolve_path_to_qualified(
    path: &AstPath,
    resolved: &ResolvedPackage,
    interner: &Interner,
) -> Option<String> {
    match resolved.resolutions().lookup_path(path.span) {
        Some(Resolved::Binding(id)) => Some(binding_qualified_name(
            resolved.binding(id),
            resolved,
            interner,
        )),
        _ => None,
    }
}

/// Compose a binding's fully qualified name as `<module>.<binding>`.
pub(super) fn binding_qualified_name(
    binding: &BindingEntry,
    resolved: &ResolvedPackage,
    interner: &Interner,
) -> String {
    let module = resolved.module_entry(binding.module);
    let mut out = module.canonical_path.to_owned_string(interner);
    out.push('.');
    out.push_str(interner.resolve(binding.name));
    out
}
