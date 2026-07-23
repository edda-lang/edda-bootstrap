//! Pre- and post-substitution mangled-name computation for nested
//! `SpecInvocation`s in a spec body.
//!
//! Split out from `map/mod.rs` for file-size reasons. These helpers
//! mirror the resolver-side `mangle_spec_invocation_name` so the
//! `with_sibling_renames` augmentation can map a body's pre-substitution
//! sibling-spec name (`Option_V`) to its post-substitution form
//! (`Option_f64`). The byte forms are locked in lock-step with
//! `edda_resolve::items` and `edda_codegen::mangle::mangle_primitive`.

use edda_intern::{Interner, Symbol};
use edda_syntax::ast::{Expr, ExprKind, Literal};

use crate::argument::Argument;

use super::SubstitutionMap;

/// Compute the `<spec-leaf>(_<arg-leaf>)*` mangled name for a
/// `SpecInvocation` AST node, mirroring the resolver's
/// `mangle_spec_invocation_name`. Used to compute pre-substitution
/// sibling-spec names from the raw spec body.
pub(super) fn ast_mangled_name(
    si: &edda_syntax::ast::SpecInvocation,
    interner: &Interner,
) -> Option<String> {
    let spec_leaf = si.path.segments.last()?.name;
    if spec_leaf == Symbol::DUMMY {
        return None;
    }
    let mut out = String::with_capacity(32);
    out.push_str(interner.resolve(spec_leaf));
    for arg in &si.args {
        out.push('_');
        out.push_str(&ast_arg_leaf(arg, interner)?);
    }
    Some(out)
}

/// Last-segment text of an argument expression — for `Path` args, the
/// rightmost segment's interned name (or `Symbol::DUMMY` bail-out); for
/// integer / bool / string literal args, the decimal / keyword /
/// safe-identifier form that `mangle_primitive` emits.
fn ast_arg_leaf(expr: &Expr, interner: &Interner) -> Option<String> {
    match &expr.kind {
        ExprKind::Path(p) => {
            let last = p.segments.last()?;
            if last.name == Symbol::DUMMY {
                return None;
            }
            Some(interner.resolve(last.name).to_string())
        }
        ExprKind::Literal(lit) => mangle_ast_literal(lit, interner),
        _ => None,
    }
}

fn mangle_ast_literal(lit: &Literal, interner: &Interner) -> Option<String> {
    match lit {
        Literal::Int { value, .. } => Some(value.to_string()),
        Literal::Bool(b) => Some(if *b { "true".to_string() } else { "false".to_string() }),
        Literal::Str(sym) => {
            if *sym == Symbol::DUMMY {
                return None;
            }
            let text = interner.resolve(*sym);
            if !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                Some(text.to_string())
            } else {
                Some("string".to_string())
            }
        }
        _ => None,
    }
}

/// Compute the post-substitution mangled name for `si` under `subst`.
/// For each arg that is a single-segment Path naming a generic parameter
/// bound to `Argument::Type(qname)`, use the leaf of `qname` as the
/// arg-leaf contribution. Otherwise fall back to the raw arg-leaf text.
pub(super) fn post_subst_mangled_name(
    si: &edda_syntax::ast::SpecInvocation,
    subst: &SubstitutionMap,
    interner: &Interner,
) -> Option<String> {
    let spec_leaf = si.path.segments.last()?.name;
    if spec_leaf == Symbol::DUMMY {
        return None;
    }
    let mut out = String::with_capacity(32);
    out.push_str(interner.resolve(spec_leaf));
    for arg in &si.args {
        out.push('_');
        let leaf = post_subst_arg_leaf(arg, subst, interner)?;
        out.push_str(&leaf);
    }
    Some(out)
}

/// Arg-leaf text after substitution: a single-segment Path naming a
/// `Argument::Type`-bound generic resolves to the leaf of the bound
/// qualified name; everything else falls back to [`ast_arg_leaf`].
fn post_subst_arg_leaf(
    expr: &Expr,
    subst: &SubstitutionMap,
    interner: &Interner,
) -> Option<String> {
    if let ExprKind::Path(p) = &expr.kind
        && p.segments.len() == 1
    {
        let head = p.segments[0].name;
        // A generic bound to a Type or Function argument contributes
        // the leaf of its qualified name to a nested invocation's mangled
        // name; both kinds carry a `SmolStr` qname.
        if let Some(binding) = subst.lookup(head)
            && let Argument::Type(qname) | Argument::Function(qname) = &binding.value
        {
            return Some(qname.rsplit('.').next().unwrap_or(qname.as_str()).to_string());
        }
    }
    ast_arg_leaf(expr, interner)
}
