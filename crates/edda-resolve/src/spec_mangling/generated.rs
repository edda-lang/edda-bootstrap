//! Generated-module leaf composition for spec invocations whose args may
//! themselves be spec-generated.

use edda_intern::{Interner, Symbol};
use edda_syntax::ast::{Expr, ExprKind, ItemKind, SpecInvocation};

use crate::binding::BindingKind;
use crate::items::ItemTable;
use super::disambig::module_disambig_hex_from_ast;
use super::name::{arg_leaf_name, mangle_spec_invocation_name};

/// Codegen-mirroring short name for a spec invocation whose args may
/// themselves be spec-generated. Where [`mangle_spec_invocation_name`]
/// emits the bare user-facing leaf (`Option_Box_HExpr` — the placeholder
/// binding name authors write), this emits the form the codegen artifact
/// materialises its module under (`Option_Box_HExpr_<innerhex>`), so
/// `build_spec_inv_targets` can match the consumer's placeholder against
/// the generated module. Mutually recursive with [`generated_arg_leaf`]
/// so nesting of arbitrary depth resolves.
pub fn mangle_spec_invocation_generated_leaf<F>(
    si: &SpecInvocation,
    source_module: &crate::graph::ModuleEntry,
    source_items: &ItemTable,
    interner: &Interner,
    resolve_imported_module: &F,
) -> Option<Symbol>
where
    F: Fn(Symbol) -> Option<String>,
{
    let spec_leaf = si.path.segments.last()?.name;
    if spec_leaf == Symbol::DUMMY {
        return None;
    }
    let mut out = String::with_capacity(32);
    out.push_str(interner.resolve(spec_leaf));
    for arg in &si.args {
        out.push('_');
        out.push_str(&generated_arg_leaf(
            arg,
            source_module,
            source_items,
            interner,
            resolve_imported_module,
        )?);
    }
    Some(interner.intern(&out))
}

/// Per-arg leaf for [`mangle_spec_invocation_generated_leaf`]: the
/// codegen-side artifact leaf for a spec-generated arg, the bare
/// [`arg_leaf_name`] otherwise.
fn generated_arg_leaf<F>(
    arg: &Expr,
    source_module: &crate::graph::ModuleEntry,
    source_items: &ItemTable,
    interner: &Interner,
    resolve_imported_module: &F,
) -> Option<String>
where
    F: Fn(Symbol) -> Option<String>,
{
    if let ExprKind::Path(p) = &arg.kind
        && p.segments.len() == 1
    {
        let leaf_sym = p.segments[0].name;
        if leaf_sym != Symbol::DUMMY
            && let Some(bid) = source_items.lookup(leaf_sym)
            && matches!(source_items.get(bid).kind, BindingKind::SpecInvocation)
        {
            for item in &source_module.ast.items {
                let ItemKind::SpecInvocation(inner_si) = &item.kind else {
                    continue;
                };
                if mangle_spec_invocation_name(inner_si, interner) != Some(leaf_sym) {
                    continue;
                }
                let inner_short = mangle_spec_invocation_generated_leaf(
                    inner_si,
                    source_module,
                    source_items,
                    interner,
                    resolve_imported_module,
                )?;
                let inner_short_text = interner.resolve(inner_short).to_string();
                return Some(
                    match module_disambig_hex_from_ast(
                        inner_si,
                        source_module,
                        source_items,
                        interner,
                        resolve_imported_module,
                    ) {
                        Some(hex) => format!("{inner_short_text}_{hex}"),
                        None => inner_short_text,
                    },
                );
            }
        }
    }
    arg_leaf_name(arg, interner)
}

/// Compose the expected last-segment of the generated module for a spec
/// invocation, including the 8-hex disambig suffix when computable. Used
/// by `build_spec_inv_targets` to find the right materialised module in
/// the source graph.
///
/// Returns the leaf-only form (no suffix) when
/// [`module_disambig_hex_from_ast`] cannot canonicalise the args — this
/// matches the codegen-side [`edda_codegen::compose_module_path`]
/// fallback. Callers SHOULD try the disambig-suffixed form first and
/// fall back to the leaf-only form.
pub fn spec_invocation_module_leaf<F>(
    si: &SpecInvocation,
    source_module: &crate::graph::ModuleEntry,
    source_items: &ItemTable,
    interner: &Interner,
    resolve_imported_leaf: &F,
) -> Option<Symbol>
where
    F: Fn(Symbol) -> Option<String>,
{
    let leaf_only = mangle_spec_invocation_name(si, interner)?;
    match module_disambig_hex_from_ast(
        si,
        source_module,
        source_items,
        interner,
        resolve_imported_leaf,
    ) {
        Some(hex) => {
            let leaf_only_text = interner.resolve(leaf_only);
            let combined = format!("{leaf_only_text}_{hex}");
            Some(interner.intern(&combined))
        }
        None => Some(leaf_only),
    }
}
