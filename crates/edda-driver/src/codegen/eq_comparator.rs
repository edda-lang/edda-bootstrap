//! derive-eq comparator-map construction and the collection-element cascade.


use edda_codegen::{
    Argument, ArgumentTuple,
    mangle_short_name, module_disambig_hex,
};
use edda_intern::{Interner, Symbol};
use edda_resolve::{
    BindingEntry, BindingId, BindingKind, ModulePath, Resolved, ResolvedPackage, mangle_spec_invocation_name,
};
use edda_syntax::ast::{
    Derive, ItemKind,
    Visibility,
};
use edda_types::{
    TyCx, TyId, TyInterner, TyKind, TypeDeclShape, VariantPayloadInfo,
};
use smol_str::SmolStr;


use super::arguments::{expr_to_argument, ty_id_to_argument};
use super::{binding_qualified_name, resolve_path_to_qualified};

/// Build the `derive eq` target-type -> comparator-`eq`-fn `BindingId`
/// map MIR lowering consumes to lower `==` / `!=` on a nominal operand.
///
/// Walks every `derive` declaration in the (pass-2) resolved package and,
/// for each that includes `eq`, registers the target type plus the
/// transitive closure of nominal field / payload types reachable from it.
/// For each reached type it resolves the `eq` function inside the
/// cascade-materialised `std.core.compare.eq_<T>_<hex>` module and records
/// `T -> eq_fn`. The module path is composed identically to the codegen
/// producer (`compose_module_path`) via [`eq_comparator_module_qname`], so
/// the two sides cannot drift.
pub(crate) fn build_eq_comparator_map(
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
    ty_cx: &TyCx,
) -> std::collections::HashMap<BindingId, BindingId> {
    let mut map = std::collections::HashMap::new();
    // The cascade may surface the same nominal type from many derives;
    // track which type bindings we have already attempted so the closure
    // walk over the whole package is linear, not quadratic.
    let mut attempted: std::collections::HashSet<BindingId> = std::collections::HashSet::new();
    for module in resolved.graph().modules() {
        for item in &module.ast.items {
            let ItemKind::Derive(derive) = &item.kind else {
                continue;
            };
            if !derive_includes_eq(derive, interner) {
                continue;
            }
            let Some(root) = derive_target_type_binding(derive, resolved) else {
                continue;
            };
            for_each_eq_cascade_type(root, ty_cx, ty_interner, &mut attempted, &mut |type_binding| {
                insert_eq_comparator(&mut map, type_binding, resolved, interner, ty_interner, ty_cx);
            });
        }
    }
    map
}

/// `true` when `derive`'s item list names `eq`.
fn derive_includes_eq(derive: &Derive, interner: &Interner) -> bool {
    derive
        .items
        .iter()
        .any(|it| it.name != Symbol::DUMMY && interner.resolve(it.name) == "eq")
}

/// Resolve a `derive`'s target Path to the type-declaration `BindingId`
/// it names, or `None` when the target does not resolve to a `TypeDecl`.
pub(super) fn derive_target_type_binding(
    derive: &Derive,
    resolved: &ResolvedPackage,
) -> Option<BindingId> {
    let Resolved::Binding(target_binding) =
        resolved.resolutions().lookup_path(derive.target.span)?
    else {
        return None;
    };
    matches!(resolved.binding(target_binding).kind, BindingKind::TypeDecl)
        .then_some(target_binding)
}

/// Visit `root` and every nominal type transitively reachable from its
/// field / payload types, calling `visit` once per distinct type binding.
/// `visited` bounds the recursion against self-referential (`Box`-backed)
/// types and dedups across the many derives in a package.
pub(super) fn for_each_eq_cascade_type(
    root: BindingId,
    ty_cx: &TyCx,
    ty_interner: &TyInterner,
    visited: &mut std::collections::HashSet<BindingId>,
    visit: &mut impl FnMut(BindingId),
) {
    if !visited.insert(root) {
        return;
    }
    visit(root);
    let Some(info) = ty_cx.type_decl(root) else {
        return;
    };
    match &info.kind {
        TypeDeclShape::Product { fields } => {
            for field in fields.iter() {
                walk_eq_cascade_ty(field.ty, ty_cx, ty_interner, visited, visit);
            }
        }
        TypeDeclShape::Sum { variants } => {
            for variant in variants.iter() {
                match &variant.payload {
                    VariantPayloadInfo::Unit => {}
                    VariantPayloadInfo::Tuple { elems } => {
                        for ty in elems.iter() {
                            walk_eq_cascade_ty(*ty, ty_cx, ty_interner, visited, visit);
                        }
                    }
                    VariantPayloadInfo::Struct { fields } => {
                        for field in fields.iter() {
                            walk_eq_cascade_ty(field.ty, ty_cx, ty_interner, visited, visit);
                        }
                    }
                }
            }
        }
    }
}

/// Descend one `TyId` looking for nominal types to cascade into. Slices
/// and tuples are transparent containers — their element / member types
/// are followed so a nested nominal still surfaces.
fn walk_eq_cascade_ty(
    ty: TyId,
    ty_cx: &TyCx,
    ty_interner: &TyInterner,
    visited: &mut std::collections::HashSet<BindingId>,
    visit: &mut impl FnMut(BindingId),
) {
    match ty_interner.kind(ty) {
        TyKind::Nominal(binding) => {
            for_each_eq_cascade_type(*binding, ty_cx, ty_interner, visited, visit);
        }
        TyKind::Slice(inner) => {
            walk_eq_cascade_ty(*inner, ty_cx, ty_interner, visited, visit);
        }
        TyKind::Tuple(elems) => {
            let elems = elems.clone();
            for inner in elems.iter() {
                walk_eq_cascade_ty(*inner, ty_cx, ty_interner, visited, visit);
            }
        }
        _ => {}
    }
}

/// Compose the canonical module path a cascade-materialised comparator spec
/// instantiation lands under: `<parent>.<mangle_short_name>_<8hex>`. Used for
/// both `std.core.compare.eq(T)` and the element-wise
/// `std.core.compare.{VecEq,OptionEq,BoxEq}(E)` specs.
pub(super) fn comparator_module_qname(spec_qualified: &str, args: &ArgumentTuple) -> String {
    let short = mangle_short_name(spec_qualified, args);
    let leaf = match module_disambig_hex(spec_qualified, args) {
        Some(hex) => format!("{short}_{hex}"),
        None => short.to_string(),
    };
    match spec_qualified.rsplit_once('.') {
        Some((parent, _)) => format!("{parent}.{leaf}"),
        None => leaf,
    }
}

/// Compose the `std.core.compare.eq_<T>_<8hex>` module path for the `derive
/// eq` comparator of the type whose fully-qualified name is `type_qualified`
/// (e.g. `std.core.compare.eq_TypeDecl_<8hex>` for `syntax.ast.item.TypeDecl`).
fn eq_comparator_module_qname(type_qualified: &str) -> String {
    let args = ArgumentTuple::new(vec![Argument::Type(SmolStr::new(type_qualified))]);
    comparator_module_qname("std.core.compare.eq", &args)
}

/// The stdlib `std.core.compare` collection whose element-wise `eq` spec a
/// materialised `Vec_<E>` / `Option_<E>` / `Box_<E>` / `IntMap_<V>` field
/// delegates to.
#[derive(Clone, Copy)]
pub(super) enum CollectionKind {
    Vec,
    Option,
    Box,
    IntMap,
}

impl CollectionKind {
    /// The qualified `std.core.compare` element-wise comparator spec name.
    pub(super) fn compare_spec(self) -> &'static str {
        match self {
            CollectionKind::Vec => "std.core.compare.VecEq",
            CollectionKind::Option => "std.core.compare.OptionEq",
            CollectionKind::Box => "std.core.compare.BoxEq",
            CollectionKind::IntMap => "std.core.compare.IntMapEq",
        }
    }

    /// The qualified `std.core.fmt` element-wise debug-formatter spec name.
    pub(super) fn debug_spec(self) -> &'static str {
        match self {
            CollectionKind::Vec => "std.core.fmt.VecDebug",
            CollectionKind::Option => "std.core.fmt.OptionDebug",
            CollectionKind::Box => "std.core.fmt.BoxDebug",
            CollectionKind::IntMap => "std.core.fmt.IntMapDebug",
        }
    }

    /// Recognise a collection by the spec path its materialising
    /// `spec <path>(E)` invocation targets.
    fn from_spec_path(qualified: &str) -> Option<Self> {
        match qualified {
            "std.collections.vec.Vec" => Some(CollectionKind::Vec),
            "std.core.option.Option" => Some(CollectionKind::Option),
            "std.mem.alloc.Box" => Some(CollectionKind::Box),
            "std.collections.hashmap.IntMap" => Some(CollectionKind::IntMap),
            _ => None,
        }
    }
}

/// When `binding` is a materialised-collection `SpecInvocation` alias
/// (`Vec_<E>` / `Option_<E>` / `Box_<E>`, brought into scope by a module-scope
/// `spec std.collections.vec.Vec(E)` / `…core.option.Option(E)` /
/// `…mem.alloc.Box(E)`), return the collection kind and the element type `E`
/// as a codegen [`Argument`]. The `derive eq` cascade delegates such a field's
/// `==` to `std.core.compare.{VecEq,OptionEq,BoxEq}(E)` rather than leaving it
/// as the unlowered bounded residue.
fn collection_spec_invocation(
    binding_id: BindingId,
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
) -> Option<(CollectionKind, Argument)> {
    let binding = resolved.binding(binding_id);
    if !matches!(binding.kind, BindingKind::SpecInvocation) {
        return None;
    }
    let module = resolved.module_entry(binding.module);
    for item in &module.ast.items {
        let ItemKind::SpecInvocation(si) = &item.kind else {
            continue;
        };
        let Some(mangled) = mangle_spec_invocation_name(si, interner) else {
            continue;
        };
        if mangled != binding.name {
            continue;
        }
        let spec_qualified = resolve_path_to_qualified(&si.path, resolved, interner)?;
        let kind = CollectionKind::from_spec_path(&spec_qualified)?;
        let elem = si.args.first()?;
        let elem_arg = expr_to_argument(elem, resolved, interner, ty_interner).ok()?;
        return Some((kind, elem_arg));
    }
    None
}

/// Recognise a materialised collection's `type` declaration by its module
/// path: `type Vec` in `std.collections.vec.Vec_<E>`, `type Option` in
/// `std.core.option.Option_<E>`, `type Box` in `std.mem.alloc.Box_<E>`, or
/// `type IntMap` in `std.collections.hashmap.IntMap_<V>`.
/// Mirrors `crate::cascade::is_box_t_materialisation`'s stricter
/// module-path test so a user module named `Vec_Foo` cannot trigger it.
fn collection_kind_from_module(
    binding: &BindingEntry,
    resolved: &ResolvedPackage,
    interner: &Interner,
) -> Option<CollectionKind> {
    let segs = resolved.module_entry(binding.module).canonical_path.segments();
    if segs.len() != 4 {
        return None;
    }
    let s0 = interner.resolve(segs[0]);
    let s1 = interner.resolve(segs[1]);
    let s2 = interner.resolve(segs[2]);
    let leaf = interner.resolve(segs[3]);
    let name = interner.resolve(binding.name);
    if s0 == "std" && s1 == "collections" && s2 == "vec" && name == "Vec" && leaf.starts_with("Vec_") {
        return Some(CollectionKind::Vec);
    }
    if s0 == "std" && s1 == "core" && s2 == "option" && name == "Option" && leaf.starts_with("Option_") {
        return Some(CollectionKind::Option);
    }
    if s0 == "std" && s1 == "mem" && s2 == "alloc" && name == "Box" && leaf.starts_with("Box_") {
        return Some(CollectionKind::Box);
    }
    if s0 == "std" && s1 == "collections" && s2 == "hashmap" && name == "IntMap" && leaf.starts_with("IntMap_") {
        return Some(CollectionKind::IntMap);
    }
    None
}

/// When `binding` is a materialised collection `TypeDecl` — the form a
/// `Vec_<E>` / `Option_<E>` / `Box_<E>` / `IntMap_<V>` field alias resolves
/// *through to* in pass-2 (after codegen materialises the module) — return the
/// collection kind and element type E as an [`Argument`], recovered
/// structurally: Vec from its `[E]` data field, Option from its `.some`
/// payload, Box from its `get(b) -> E` accessor's return type (Box's
/// `type Box {}` is empty, so the element is not a field), IntMap from the
/// `value: V` field of its nested `Entry` type (see [`intmap_value_ty`]).
fn collection_typedecl(
    binding_id: BindingId,
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
    ty_cx: &TyCx,
) -> Option<(CollectionKind, Argument)> {
    let binding = resolved.binding(binding_id);
    if !matches!(binding.kind, BindingKind::TypeDecl) {
        return None;
    }
    let kind = collection_kind_from_module(binding, resolved, interner)?;
    let elem_ty = match kind {
        CollectionKind::Vec => {
            let info = ty_cx.type_decl(binding_id)?;
            info.fields().iter().find_map(|f| match ty_interner.kind(f.ty) {
                TyKind::Slice(inner) => Some(*inner),
                _ => None,
            })?
        }
        CollectionKind::Option => {
            let info = ty_cx.type_decl(binding_id)?;
            info.variants().iter().find_map(|v| match &v.payload {
                VariantPayloadInfo::Tuple { elems } => elems.first().copied(),
                VariantPayloadInfo::Struct { fields } => fields.first().map(|f| f.ty),
                VariantPayloadInfo::Unit => None,
            })?
        }
        CollectionKind::Box => {
            // `type Box {}` is empty — recover E from the sibling
            // `get(b: Box) -> E` accessor's return type.
            let get_sym = interner.intern("get");
            let get_binding = resolved.module(binding.module).items.lookup(get_sym)?;
            ty_cx.sig(get_binding)?.return_ty
        }
        CollectionKind::IntMap => intmap_value_ty(binding_id, interner, ty_interner, ty_cx)?,
    };
    let elem_arg = element_argument(elem_ty, resolved, interner, ty_interner)?;
    Some((kind, elem_arg))
}

/// Recover the value type `V` of a materialised `IntMap_<V>` `TypeDecl` from
/// the `value: V` field of the `Entry` record reached through its
/// `entries: [Entry]` slice. Two stdlib layouts are admitted: the flat
/// `IntMap { entries: [Entry] }` (the original layout) where the slice sits directly on
/// the IntMap typedecl, and the HashMap-backed `IntMap { inner: HashMap_i64_V_... }`
/// (where StringMap/IntMap are unified over one generic
/// `HashMap(K, V, hash_fn, eq_fn)`) where the `entries: [Entry]` slice lives
/// one hop down on the `inner` HashMap typedecl. The flat probe is tried first;
/// on miss the single `inner`-nominal hop is followed. Chosen over the sibling
/// `get(map, key: i64) -> Option_V` accessor because IntMap's `get` returns the
/// wrapped `Option_V`, not bare V (unlike Box's `get(b) -> E`).
fn intmap_value_ty(
    binding_id: BindingId,
    interner: &Interner,
    ty_interner: &TyInterner,
    ty_cx: &TyCx,
) -> Option<TyId> {
    let info = ty_cx.type_decl(binding_id)?;
    // Flat layout (the original layout): `entries: [Entry]` sits directly on the IntMap.
    if let Some(v) = value_ty_from_entries(info, interner, ty_interner, ty_cx) {
        return Some(v);
    }
    // HashMap-backed layout: follow the single `inner: HashMap_i64_V_...`
    // nominal field to the generic HashMap typedecl, which carries the
    // `entries: [Entry]` slice and thus the `Entry.value: V` field.
    info.fields().iter().find_map(|f| {
        let TyKind::Nominal(inner_binding) = ty_interner.kind(f.ty) else {
            return None;
        };
        let inner_info = ty_cx.type_decl(*inner_binding)?;
        value_ty_from_entries(inner_info, interner, ty_interner, ty_cx)
    })
}

/// Recover `Entry.value`'s type from a record `info` that owns an
/// `entries: [Entry]` slice field: follow the first slice field to its
/// element `Entry` binding and read that record's `value` field type.
fn value_ty_from_entries(
    info: &edda_types::TypeDeclInfo,
    interner: &Interner,
    ty_interner: &TyInterner,
    ty_cx: &TyCx,
) -> Option<TyId> {
    let entry_ty = info.fields().iter().find_map(|f| match ty_interner.kind(f.ty) {
        TyKind::Slice(inner) => Some(*inner),
        _ => None,
    })?;
    let TyKind::Nominal(entry_binding) = ty_interner.kind(entry_ty) else {
        return None;
    };
    let entry_info = ty_cx.type_decl(*entry_binding)?;
    let value_sym = interner.intern("value");
    Some(entry_info.field(value_sym)?.ty)
}

/// Lower an element `TyId` to the codegen [`Argument`] the producer named
/// the element comparator's module from. A materialised-collection element
/// is named by its module path (matching how a `Box_<E>` / `Vec_<E>` /
/// `Option_<E>` spec-invocation alias lowers through `expr_to_argument` on
/// the root side); every other element uses `ty_id_to_argument`.
fn element_argument(
    elem_ty: TyId,
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
) -> Option<Argument> {
    if let TyKind::Nominal(b) = ty_interner.kind(elem_ty) {
        let binding = resolved.binding(*b);
        if collection_kind_from_module(binding, resolved, interner).is_some() {
            let module_path = resolved
                .module_entry(binding.module)
                .canonical_path
                .to_owned_string(interner);
            return Some(Argument::Type(SmolStr::new(module_path)));
        }
    }
    ty_id_to_argument(elem_ty, ty_interner, resolved, interner).ok()
}

/// Unified collection recogniser used by both derive-eq cascade sides.
pub(super) fn collection_of(
    binding_id: BindingId,
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
    ty_cx: &TyCx,
) -> Option<(CollectionKind, Argument)> {
    collection_spec_invocation(binding_id, resolved, interner, ty_interner)
        .or_else(|| collection_typedecl(binding_id, resolved, interner, ty_interner, ty_cx))
}

/// Resolve one type's cascade-materialised comparator and, on success,
/// record `type_binding -> eq_fn` in `map`. A materialised `Vec_<E>` /
/// `Option_<E>` / `Box_<E>` field is mapped to its stdlib element-wise
/// `std.core.compare.{VecEq,OptionEq,BoxEq}(E)` comparator;
/// a plain `derive eq` target is mapped to its `std.core.compare.eq_<T>`
/// comparator. Silently skips non-type bindings, non-public plain targets,
/// and comparator modules that did not materialise (the residual `[E]`-slice
/// field whose `==` does not yet lower).
fn insert_eq_comparator(
    map: &mut std::collections::HashMap<BindingId, BindingId>,
    type_binding: BindingId,
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
    ty_cx: &TyCx,
) {
    // A materialised `Vec_<E>` / `Option_<E>` / `Box_<E>` field's `==` lowers
    // to a `Call` into the stdlib element-wise comparator, keyed in the map by
    // the binding MIR lowering sees on the operand (in pass-2 that is the
    // materialised `TypeDecl`). The element `E` (and thus the comparator module
    // name) is composed from the SAME element type the producer monomorphised
    // from, so the map key and the on-disk module cannot drift.
    if let Some((kind, elem_arg)) =
        collection_of(type_binding, resolved, interner, ty_interner, ty_cx)
    {
        let args = ArgumentTuple::new(vec![elem_arg]);
        let module_qname = comparator_module_qname(kind.compare_spec(), &args);
        let segments: Vec<Symbol> = module_qname.split('.').map(|s| interner.intern(s)).collect();
        let module_path = ModulePath::new(segments.into_boxed_slice());
        let Some(module_id) = resolved.graph().lookup_by_path(&module_path) else {
            return;
        };
        let items = &resolved.module(module_id).items;
        let eq_sym = interner.intern("eq");
        let Some(eq_fn) = items.lookup(eq_sym) else {
            return;
        };
        if matches!(items.get(eq_fn).kind, BindingKind::Function) {
            map.insert(type_binding, eq_fn);
        }
        return;
    }
    if !matches!(resolved.binding(type_binding).kind, BindingKind::TypeDecl) {
        return;
    }
    // Mirror the emit-side gate: a `std.core.compare.*` comparator can only
    // reference a `public` target, so a non-public materialised instantiation
    // has no comparator to map to.
    if resolved.binding(type_binding).visibility != Visibility::Public {
        return;
    }
    let type_qualified = binding_qualified_name(resolved.binding(type_binding), resolved, interner);
    let module_qname = eq_comparator_module_qname(&type_qualified);
    let segments: Vec<Symbol> = module_qname.split('.').map(|s| interner.intern(s)).collect();
    let module_path = ModulePath::new(segments.into_boxed_slice());
    let Some(module_id) = resolved.graph().lookup_by_path(&module_path) else {
        return;
    };
    let items = &resolved.module(module_id).items;
    let eq_sym = interner.intern("eq");
    let Some(eq_fn) = items.lookup(eq_sym) else {
        return;
    };
    if matches!(items.get(eq_fn).kind, BindingKind::Function) {
        map.insert(type_binding, eq_fn);
    }
}
