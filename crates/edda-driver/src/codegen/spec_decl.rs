//! Spec-declaration lookup and nested (post-substitution) invocation
//! lowering. Locates the `Spec` AST backing a qualified name and builds
//! the `RootInvocation`s the cascade worklist drains.

use std::path::PathBuf;

use edda_codegen::{Argument, ArgumentTuple, PrimitiveValue};
use edda_diag::{Diagnostics, LintConfig};
use edda_intern::{Interner, Symbol};
use edda_resolve::ResolvedPackage;
use edda_syntax::ast::{Expr, ExprKind, Item, ItemKind, Literal, Spec, SpecInvocation};
use edda_types::{Primitive, TyInterner};
use smol_str::SmolStr;

use super::arguments::{literal_to_argument, path_to_type_argument};
use super::support::{emit_typecheck, join_segments};
use super::RootInvocation;

/// Result of [`find_spec_decl`] — every piece of parent-module context
/// the codegen pipeline needs to monomorphise one spec invocation.
pub(super) struct SpecLookup {
    /// Cloned spec declaration AST whose body will be substituted.
    pub spec: Spec,
    /// On-disk path of the source file the spec lives in (used for
    /// repo-tier vs cache-tier classification).
    pub source_path: PathBuf,
    /// `ItemKind::Import` items from the spec's parent module, cloned
    /// in source order; prepended to the generated artifact.
    pub parent_imports: Vec<Item>,
    /// Parent module's dotted qualified path (`"std.alloc"` for a spec
    /// declared in `std.alloc`).
    pub parent_qualified: SmolStr,
    /// Names of every top-level non-spec / non-import item in the
    /// parent module (type-decls, functions, let-decls). Used to
    /// re-qualify bare references in the substituted spec body.
    pub parent_sibling_names: Vec<Symbol>,
}

/// Locate the [`Spec`] AST declaration that backs the qualified name.
///
/// Walks every module in the resolved package, matches against the
/// module's `canonical_path`, and collects everything the codegen
/// pipeline needs to monomorphise the spec — see [`SpecLookup`].
pub(super) fn find_spec_decl(
    qualified: &str,
    resolved: &ResolvedPackage,
    interner: &Interner,
) -> Option<SpecLookup> {
    let (module_prefix, leaf) = match qualified.rsplit_once('.') {
        Some((m, l)) => (m, l),
        None => ("", qualified),
    };

    for module in resolved.graph().modules() {
        let path_text = module.canonical_path.to_owned_string(interner);
        if path_text != module_prefix {
            continue;
        }
        let mut found: Option<Spec> = None;
        let mut imports: Vec<Item> = Vec::new();
        let mut sibling_names: Vec<Symbol> = Vec::new();
        for item in &module.ast.items {
            match &item.kind {
                ItemKind::Spec(spec) => {
                    if found.is_none() {
                        let spec_name = interner.resolve(spec.name.name);
                        if spec_name == leaf {
                            found = Some((**spec).clone());
                        }
                    }
                }
                ItemKind::Import(_) => {
                    imports.push(item.clone());
                }
                ItemKind::TypeDecl(td) => sibling_names.push(td.name.name),
                ItemKind::Function(fd) => sibling_names.push(fd.name.name),
                ItemKind::Let(ld) => sibling_names.push(ld.name.name),
                ItemKind::SpecInvocation(_) | ItemKind::Module(_) => {}
                // `derive` is not a sibling name in the spec-lookup
                // scan; C7 desugars it to SpecInvocations during
                // codegen expansion, which then surface as siblings on
                // the next cascade pass.
                ItemKind::Derive(_) => {}
            }
        }
        if let Some(spec) = found {
            return Some(SpecLookup {
                spec,
                source_path: module.file_path.clone(),
                parent_imports: imports,
                parent_qualified: SmolStr::new(module_prefix),
                parent_sibling_names: sibling_names,
            });
        }
    }
    None
}

/// Build a `RootInvocation` from a `SpecInvocation` extracted from a
/// post-substitution spec body.
/// Returns `None` (and emits a diagnostic at the invocation span) when
/// any argument expression has a shape the cascade does not yet admit,
/// or when the parent spec declaration cannot be located.
pub(super) fn root_from_substituted_invocation(
    si: &SpecInvocation,
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<RootInvocation> {
    let qualified = join_segments(&si.path, interner);
    let mut args = Vec::with_capacity(si.args.len());
    for arg_expr in &si.args {
        match lower_substituted_arg(arg_expr, resolved, interner, ty_interner) {
            Ok(a) => args.push(a),
            Err(reason) => {
                emit_typecheck(
                    diags,
                    lint_cfg,
                    arg_expr.span,
                    format!(
                        "codegen: nested spec `{qualified}` — argument shape not admitted ({reason})"
                    ),
                );
                return None;
            }
        }
    }
    let found = match find_spec_decl(&qualified, resolved, interner) {
        Some(f) => f,
        None => {
            emit_typecheck(
                diags,
                lint_cfg,
                si.span,
                format!(
                    "codegen: nested spec `{qualified}` not located in the resolved package"
                ),
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

/// Lower one argument expression of a post-substitution nested spec
/// invocation to an [`Argument`], preferring resolver-mediated
/// lowering (correct kind + full qname) and falling back to the
/// text-only form for generic-substituted args and literals.
fn lower_substituted_arg(
    expr: &Expr,
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
) -> Result<Argument, String> {
    if let ExprKind::Path(p) = &expr.kind
        && let Ok(arg) = path_to_type_argument(p, resolved, interner, ty_interner)
    {
        return Ok(arg);
    }
    substituted_expr_to_argument(expr, interner)
}

/// Convert one expression from a post-substitution spec invocation's
/// arg list into an [`Argument`].
fn substituted_expr_to_argument(expr: &Expr, interner: &Interner) -> Result<Argument, String> {
    match &expr.kind {
        ExprKind::Path(p) => {
            if p.segments.len() == 1 {
                let text = interner.resolve(p.segments[0].name);
                if let Some(prim) = Primitive::from_name(text) {
                    return Ok(Argument::Type(SmolStr::new(prim.name())));
                }
            }
            Ok(Argument::Type(SmolStr::new(join_segments(p, interner))))
        }
        ExprKind::Literal(lit) => literal_to_argument(*lit, interner),
        ExprKind::Unary { op, expr: inner }
            if matches!(op, edda_syntax::ast::UnOp::Neg)
                && matches!(inner.kind, ExprKind::Literal(Literal::Int { .. })) =>
        {
            if let ExprKind::Literal(Literal::Int { value, .. }) = inner.kind {
                let signed = (value as i128).wrapping_neg();
                Ok(Argument::Primitive(PrimitiveValue::I64(signed as i64)))
            } else {
                Err("non-literal unary expression".to_string())
            }
        }
        _ => Err("expression shape not admitted in nested spec arg".to_string()),
    }
}
