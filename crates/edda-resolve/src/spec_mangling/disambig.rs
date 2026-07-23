//! Module-disambiguation hashing and the type-qname resolution it consumes.

use edda_intern::{Interner, Symbol};
use edda_syntax::ast::{Expr, ExprKind, ItemKind, Literal};

use crate::binding::BindingKind;
use crate::items::ItemTable;
use super::MODULE_DISAMBIG_VERSION;
use super::name::{mangle_literal, mangle_spec_invocation_name};

/// Produce the canonical type-qname an arg path contributes to the
/// disambig-hash input. Mirrors the codegen-side `Argument::Type(qname)`
/// construction in `edda_driver::codegen::path_to_type_argument`.
///
/// `resolve_imported_module(seg)` returns the imported module's
/// canonical_path string (no leaf appended) if `seg` is an import leaf
/// in the source module's leaf-import table, otherwise `None`. This is
/// the resolver's substitute for the codegen-side's full resolution map.
fn arg_type_qname_for_hash<F>(
    path: &edda_syntax::ast::Path,
    source_module: &crate::graph::ModuleEntry,
    source_items: &ItemTable,
    interner: &Interner,
    resolve_imported_module: &F,
) -> Option<String>
where
    F: Fn(Symbol) -> Option<String>,
{
    for seg in &path.segments {
        if seg.name == Symbol::DUMMY {
            return None;
        }
    }
    if path.segments.len() == 1 {
        let leaf_sym = path.segments[0].name;
        let leaf_text = interner.resolve(leaf_sym);
        // Local type-decl: qname = source_module.canonical_path + "." + leaf
        // (matches `path_to_type_argument` TypeDecl branch).
        if let Some(binding_id) = source_items.lookup(leaf_sym) {
            let entry = source_items.get(binding_id);
            // A local type-decl OR a local function (the function-arg
            // case) qualifies to `source_module.canonical_path + "." + leaf`,
            // matching the producer-side `path_to_type_argument` for both
            // `BindingKind::TypeDecl` and `BindingKind::Function`. Without
            // the Function arm a bare local function path would fall through
            // to the primitive-name fallback (`leaf` only) and the two sides'
            // disambig hashes would diverge.
            if matches!(entry.kind, BindingKind::TypeDecl | BindingKind::Function) {
                let mut qname = source_module.canonical_path.to_owned_string(interner);
                qname.push('.');
                qname.push_str(leaf_text);
                return Some(qname);
            }
            // Spec-invocation-bound arg — the double-nest case, e.g. the
            // `Box_HExpr` arg inside `Option(Box_HExpr)`. The codegen side
            // lowers such an arg to the generated nominal's qualified name
            // `<spec-parent>.<mangled>_<8hex>` via
            // `spec_invocation_qualified_name`, NOT `<source_module>.<leaf>`.
            // Mirror that so the OUTER spec's disambig hash matches the
            // producer and both consumers' placeholder bindings map to one
            // content-addressed generated module.
            if matches!(entry.kind, BindingKind::SpecInvocation)
                && let Some(qname) = spec_inv_arg_qname(
                    leaf_sym,
                    source_module,
                    source_items,
                    interner,
                    resolve_imported_module,
                )
            {
                return Some(qname);
            }
        }
        // Import-leaf'd typedecl: qname = imported_module's canonical_path + "." + leaf.
        if let Some(imported_canonical) = resolve_imported_module(leaf_sym) {
            let mut qname = imported_canonical;
            qname.push('.');
            qname.push_str(leaf_text);
            return Some(qname);
        }
        // Single-segment that did not resolve to any local or imported typedecl
        // — treat as a primitive name (matches `path_to_type_argument`'s
        // primitive short-circuit).
        Some(leaf_text.to_string())
    } else {
        // Multi-segment: if the first segment is an import alias, replace it
        // with the imported module's canonical_path; otherwise join verbatim.
        let first = path.segments[0].name;
        let mut qname = match resolve_imported_module(first) {
            Some(canonical) => canonical,
            None => interner.resolve(first).to_string(),
        };
        for seg in &path.segments[1..] {
            qname.push('.');
            qname.push_str(interner.resolve(seg.name));
        }
        Some(qname)
    }
}

/// Resolver-side reciprocal of
/// `edda_driver::codegen::spec_invocation_qualified_name`.
///
/// A single-segment arg path that resolves to a local `SpecInvocation`
/// binding is the double-nest case (`Box_HExpr` inside
/// `Option(Box_HExpr)`). The codegen side does NOT lower it to
/// `<source_module>.<leaf>`; it lowers it to the generated nominal's
/// home — the inner spec's parent module, the mangled leaf, then the
/// same 8-hex disambig [`module_disambig_hex_from_ast`] computes. We
/// rebuild that string so the OUTER spec's disambig hash sees the same
/// `Argument::Type(qname)` the producer baked in. Returns `None` when no
/// matching `SpecInvocation` item is found or its spec path does not
/// resolve, in which case the caller degrades to the leaf-only
/// candidate-module match (the pre-fix behaviour).
fn spec_inv_arg_qname<F>(
    leaf_sym: Symbol,
    source_module: &crate::graph::ModuleEntry,
    source_items: &ItemTable,
    interner: &Interner,
    resolve_imported_module: &F,
) -> Option<String>
where
    F: Fn(Symbol) -> Option<String>,
{
    for item in &source_module.ast.items {
        let ItemKind::SpecInvocation(inner_si) = &item.kind else {
            continue;
        };
        let mangled = mangle_spec_invocation_name(inner_si, interner)?;
        if mangled != leaf_sym {
            continue;
        }
        let spec_qualified = resolve_spec_path_qualified(
            &inner_si.path,
            source_module,
            source_items,
            interner,
            resolve_imported_module,
        )?;
        let parent = spec_qualified
            .rsplit_once('.')
            .map(|(p, _)| p)
            .unwrap_or("");
        let leaf_bare = interner.resolve(leaf_sym);
        let leaf = match module_disambig_hex_from_ast(
            inner_si,
            source_module,
            source_items,
            interner,
            resolve_imported_module,
        ) {
            Some(hex) => format!("{leaf_bare}_{hex}"),
            None => leaf_bare.to_string(),
        };
        let mut out = String::with_capacity(parent.len() + 1 + leaf.len());
        if !parent.is_empty() {
            out.push_str(parent);
            out.push('.');
        }
        out.push_str(&leaf);
        return Some(out);
    }
    None
}

/// Resolve a `spec PATH(args)` invocation's spec path to its qualified
/// `<module>.<spec-name>` form. An imported or aliased spec name resolves
/// through the leaf-import closure; a locally-declared spec uses the
/// source module's own canonical path. Reciprocal of the codegen-side
/// resolution-map walk, which is unavailable this early in the pipeline.
fn resolve_spec_path_qualified<F>(
    path: &edda_syntax::ast::Path,
    source_module: &crate::graph::ModuleEntry,
    source_items: &ItemTable,
    interner: &Interner,
    resolve_imported_module: &F,
) -> Option<String>
where
    F: Fn(Symbol) -> Option<String>,
{
    for seg in &path.segments {
        if seg.name == Symbol::DUMMY {
            return None;
        }
    }
    if path.segments.len() == 1 {
        let leaf = path.segments[0].name;
        let leaf_text = interner.resolve(leaf);
        // Imported spec name (`import std.mem.alloc.Box; spec Box(...)`).
        if let Some(module_canonical) = resolve_imported_module(leaf) {
            return Some(format!("{module_canonical}.{leaf_text}"));
        }
        // Locally-declared spec in this module.
        if let Some(bid) = source_items.lookup(leaf)
            && matches!(source_items.get(bid).kind, BindingKind::Spec)
        {
            let mut q = source_module.canonical_path.to_owned_string(interner);
            q.push('.');
            q.push_str(leaf_text);
            return Some(q);
        }
        None
    } else {
        // Fully- or alias-qualified path (`std.mem.alloc.Box`): resolve the
        // first segment through the import closure when it is an alias,
        // otherwise take it verbatim, then join the remaining segments.
        let first = path.segments[0].name;
        let mut q = match resolve_imported_module(first) {
            Some(canonical) => canonical,
            None => interner.resolve(first).to_string(),
        };
        for seg in &path.segments[1..] {
            q.push('.');
            q.push_str(interner.resolve(seg.name));
        }
        Some(q)
    }
}

/// 8-hex disambiguator suffix for a spec invocation's expected
/// generated-module leaf, computed from the raw `SpecInvocation` AST plus
/// the source module's item-table for ad-hoc single-segment-arg
/// resolution. Reciprocal of the codegen-side
/// [`edda_codegen::module_disambig_hex`].
pub fn module_disambig_hex_from_ast<F>(
    si: &edda_syntax::ast::SpecInvocation,
    source_module: &crate::graph::ModuleEntry,
    source_items: &ItemTable,
    interner: &Interner,
    resolve_imported_leaf: &F,
) -> Option<String>
where
    F: Fn(Symbol) -> Option<String>,
{
    let spec_leaf = si.path.segments.last()?.name;
    if spec_leaf == Symbol::DUMMY {
        return None;
    }
    let leaf_text = interner.resolve(spec_leaf);
    module_disambig_hex_for_args(
        leaf_text,
        &si.args,
        source_module,
        source_items,
        interner,
        resolve_imported_leaf,
    )
}

/// Core disambig-hex computation. Takes a pre-resolved spec leaf text
/// and the raw arg expressions; used by both
/// [`module_disambig_hex_from_ast`] (for hand-written `spec PATH(args)`
/// invocations) and the derive-arm of `build_spec_inv_targets` (which
/// synthesises spec invocations from `derive items for T` declarations).
pub fn module_disambig_hex_for_args<F>(
    spec_leaf_text: &str,
    args: &[Expr],
    source_module: &crate::graph::ModuleEntry,
    source_items: &ItemTable,
    interner: &Interner,
    resolve_imported_leaf: &F,
) -> Option<String>
where
    F: Fn(Symbol) -> Option<String>,
{
    let mut s = String::with_capacity(spec_leaf_text.len() + 32 * args.len());
    s.push(MODULE_DISAMBIG_VERSION as char);
    s.push_str(spec_leaf_text);
    for arg in args {
        s.push('\0');
        match &arg.kind {
            ExprKind::Path(p) => {
                s.push_str("T:");
                s.push_str(&arg_type_qname_for_hash(
                    p,
                    source_module,
                    source_items,
                    interner,
                    resolve_imported_leaf,
                )?);
            }
            ExprKind::Literal(lit) => {
                s.push_str("P:");
                s.push_str(&mangle_literal(lit, interner)?);
            }
            ExprKind::Unary { op: edda_syntax::ast::UnOp::Neg, expr: inner } => {
                // `-N` int literal — codegen lowers via `PrimitiveValue::I64`
                // and `mangle_primitive` emits "-<decimal>". Mirror that.
                if let ExprKind::Literal(Literal::Int { value, .. }) = inner.kind {
                    let signed = (value as i128).wrapping_neg();
                    s.push_str("P:");
                    s.push_str(&signed.to_string());
                } else {
                    return None;
                }
            }
            _ => return None,
        }
    }
    let hash = edda_cache::hash_bytes(s.as_bytes());
    let full = hash.to_string();
    Some(full[..8].to_string())
}
