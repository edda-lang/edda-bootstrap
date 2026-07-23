//! derive-debug formatter-map construction.
//!
//! The MIR f-string fold consumes this map to lower an aggregate
//! interpolation slot (`f"{v}"` where `v` is a `derive debug` nominal) to a
//! `Call` into the materialised `std.core.fmt.debug_<T>_<hex>.format(v)`
//! formatter — the structural mirror of the `eq_comparators` dispatch for
//! `==` / `!=` ([`super::eq_comparator::build_eq_comparator_map`]).

use edda_intern::{Interner, Symbol};
use edda_resolve::{BindingId, BindingKind, ModulePath, ResolvedPackage};
use edda_syntax::ast::{Derive, ItemKind, Visibility};
use edda_types::{TyCx, TyInterner};

use edda_codegen::{Argument, ArgumentTuple};
use smol_str::SmolStr;

use super::binding_qualified_name;
use super::eq_comparator::{
    collection_of, comparator_module_qname, derive_target_type_binding, for_each_eq_cascade_type,
};

/// Build the `derive debug` target-type -> formatter-`format`-fn `BindingId`
/// map MIR lowering consumes to render an aggregate f-string interpolation
/// slot.
///
/// Walks every `derive` declaration in the (pass-2) resolved package and,
/// for each that includes `debug`, registers the target type plus the
/// transitive closure of nominal field / payload types reachable from it.
/// For each reached type it resolves the `format` function inside the
/// cascade-materialised `std.core.fmt.debug_<T>_<hex>` module and records
/// `T -> format_fn`. The module path is composed identically to the codegen
/// producer (`compose_module_path`) via [`debug_formatter_module_qname`], so
/// the two sides cannot drift.
pub(crate) fn build_debug_formatter_map(
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
    ty_cx: &TyCx,
) -> std::collections::HashMap<BindingId, BindingId> {
    let mut map = std::collections::HashMap::new();
    // The cascade may surface the same nominal type from many derives; track
    // which type bindings we have already attempted so the closure walk over
    // the whole package is linear, not quadratic (mirror of the eq map).
    let mut attempted: std::collections::HashSet<BindingId> = std::collections::HashSet::new();
    for module in resolved.graph().modules() {
        for item in &module.ast.items {
            let ItemKind::Derive(derive) = &item.kind else {
                continue;
            };
            if !derive_includes_debug(derive, interner) {
                continue;
            }
            let Some(root) = derive_target_type_binding(derive, resolved) else {
                continue;
            };
            for_each_eq_cascade_type(root, ty_cx, ty_interner, &mut attempted, &mut |type_binding| {
                insert_debug_formatter(
                    &mut map,
                    type_binding,
                    resolved,
                    interner,
                    ty_interner,
                    ty_cx,
                );
            });
        }
    }
    map
}

/// `true` when `derive`'s item list names `debug`.
fn derive_includes_debug(derive: &Derive, interner: &Interner) -> bool {
    derive.items.iter().any(|item| {
        item.name != Symbol::DUMMY && interner.resolve(item.name) == "debug"
    })
}

/// Resolve one type's cascade-materialised formatter and, on success,
/// record `type_binding -> format_fn` in `map`. A materialised collection
/// field is mapped to its element-wise `std.core.fmt.{Vec,Option,Box,IntMap}Debug(E)`
/// formatter; a plain `derive debug` target is mapped to its
/// `std.core.fmt.debug_<T>` formatter. Silently skips non-type bindings,
/// non-public plain targets, and formatter modules that did not materialise.
fn insert_debug_formatter(
    map: &mut std::collections::HashMap<BindingId, BindingId>,
    type_binding: BindingId,
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
    ty_cx: &TyCx,
) {
    // A materialised `Vec_<E>` / `Option_<E>` / `Box_<E>` / `IntMap_<V>` field's
    // aggregate debug slot lowers to a `Call` into the stdlib element-wise
    // formatter, keyed in the map by the binding MIR lowering sees on the
    // operand (in pass-2 that is the materialised `TypeDecl`). The element `E`
    // (and thus the formatter module name) is composed from the SAME element
    // type the producer monomorphised from, so the map key and the on-disk
    // module cannot drift — exactly parallel to `insert_eq_comparator`.
    if let Some((kind, elem_arg)) =
        collection_of(type_binding, resolved, interner, ty_interner, ty_cx)
    {
        let args = ArgumentTuple::new(vec![elem_arg]);
        let module_qname = comparator_module_qname(kind.debug_spec(), &args);
        let segments: Vec<Symbol> = module_qname.split('.').map(|s| interner.intern(s)).collect();
        let module_path = ModulePath::new(segments.into_boxed_slice());
        let Some(module_id) = resolved.graph().lookup_by_path(&module_path) else {
            return;
        };
        let items = &resolved.module(module_id).items;
        let format_sym = interner.intern("format");
        let Some(format_fn) = items.lookup(format_sym) else {
            return;
        };
        if matches!(items.get(format_fn).kind, BindingKind::Function) {
            map.insert(type_binding, format_fn);
        }
        return;
    }
    if !matches!(resolved.binding(type_binding).kind, BindingKind::TypeDecl) {
        return;
    }
    if resolved.binding(type_binding).visibility != Visibility::Public {
        return;
    }
    let type_qualified = binding_qualified_name(resolved.binding(type_binding), resolved, interner);
    let module_qname = debug_formatter_module_qname(&type_qualified);
    let segments: Vec<Symbol> = module_qname.split('.').map(|s| interner.intern(s)).collect();
    let module_path = ModulePath::new(segments.into_boxed_slice());
    let Some(module_id) = resolved.graph().lookup_by_path(&module_path) else {
        return;
    };
    let items = &resolved.module(module_id).items;
    let format_sym = interner.intern("format");
    let Some(format_fn) = items.lookup(format_sym) else {
        return;
    };
    if !matches!(items.get(format_fn).kind, BindingKind::Function) {
        return;
    }
    // Only record the *pure* synthesised formatter (`format(v: T) -> String`,
    // one parameter). A materialised module carrying the stdlib placeholder
    // (`format(v, allocator)`, two parameters) instead — which happens when an
    // explicit `spec std.core.fmt.debug(T)` for the same type won the
    // `(spec, args)` dedup over the `derive debug` synthesis (explicit
    // invocations are collected before derives) or a synthesis fallback fired —
    // is skipped, so the f-string fold's one-argument `format(v)` call can
    // never collide with a two-parameter signature; that slot falls through to
    // the first-word fallback instead of emitting an arity-mismatched Call.
    let module_ast = &resolved.graph().module(module_id).ast;
    let format_is_unary = module_ast.items.iter().any(|item| match &item.kind {
        ItemKind::Function(f) => f.name.name == format_sym && f.params.len() == 1,
        _ => false,
    });
    if !format_is_unary {
        return;
    }
    map.insert(type_binding, format_fn);
}

/// Compose the `std.core.fmt.debug_<T>_<8hex>` module path for the `derive
/// debug` formatter of the type whose fully-qualified name is
/// `type_qualified` (e.g. `std.core.fmt.debug_HItem_<8hex>` for
/// `hir.item.HItem`).
fn debug_formatter_module_qname(type_qualified: &str) -> String {
    let args = ArgumentTuple::new(vec![Argument::Type(SmolStr::new(type_qualified))]);
    comparator_module_qname("std.core.fmt.debug", &args)
}
