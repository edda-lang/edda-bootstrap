//! Resolved-package driver — walks a [`ResolvedPackage`] and populates a [`TyCx`].
//!
//! For every Function / TypeDecl item in the resolved package,
//! lower its signature (functions) or layout (type decls) once and insert into
//! the resulting [`TyCx`]. The data types live in [`crate::cx`]; this file
//! holds the AST-walking logic that materialises them.

use ahash::AHashMap;
use edda_diag::{Diagnostics, LintConfig};
use edda_intern::Symbol;
use edda_resolve::{BindingKind, Resolved, ResolvedPackage};
use edda_syntax::ast::{
    self, Expr, ExprKind, GenericKind, ItemKind, Literal, Spec, TypeDeclKind, TypeKind, UnOp,
    VariantPayload,
};

use crate::cx::{
    ConstInit, FieldInfo, TyCx, TypeDeclInfo, TypeDeclShape, VariantInfo, VariantPayloadInfo,
};
use crate::lower::{LowerCx, lower_fn_sig, lower_type};
use crate::ty::TyId;

/// Lower a `type` declaration to its typed [`TypeDeclInfo`].
///
/// Each field's annotated type lowers via [`lower_type`] against the
/// same [`LowerCx`] the caller supplies; cascade diagnostics from
/// per-type lowering surface through `diags`. Generic parameters are
/// silently dropped — generics need the spec-instantiation pass.
/// Field-level `where` refinements carry over onto
/// [`FieldInfo::refinement`] (unlowered AST, extracted via
/// [`field_refinement_pred`]); `edda-refine`'s `field_refinement_facts`
/// is the sole consumer.
pub(crate) fn lower_type_decl(
    decl: &ast::TypeDecl,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> TypeDeclInfo {
    let kind = match &decl.kind {
        TypeDeclKind::Product { fields } => {
            let lowered: Vec<FieldInfo> = fields
                .iter()
                .map(|f| FieldInfo {
                    span: f.span,
                    name: f.name.name,
                    ty: lower_type(&f.ty, cx, diags, lint_cfg),
                    refinement: field_refinement_pred(f),
                })
                .collect();
            TypeDeclShape::Product {
                fields: lowered.into_boxed_slice(),
            }
        }
        TypeDeclKind::Sum { variants } => {
            let lowered: Vec<VariantInfo> = variants
                .iter()
                .map(|v| VariantInfo {
                    span: v.span,
                    name: v.name.name,
                    payload: lower_variant_payload(&v.payload, cx, diags, lint_cfg),
                })
                .collect();
            TypeDeclShape::Sum {
                variants: lowered.into_boxed_slice(),
            }
        }
    };
    TypeDeclInfo {
        span: decl.span,
        linearity: decl.linearity,
        kind,
    }
}

fn lower_variant_payload(
    payload: &VariantPayload,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> VariantPayloadInfo {
    match payload {
        VariantPayload::Unit => VariantPayloadInfo::Unit,
        VariantPayload::Tuple(elems) => {
            let lowered: Vec<TyId> = elems
                .iter()
                .map(|t| lower_type(t, cx, diags, lint_cfg))
                .collect();
            VariantPayloadInfo::Tuple {
                elems: lowered.into_boxed_slice(),
            }
        }
        VariantPayload::Struct(fields) => {
            let lowered: Vec<FieldInfo> = fields
                .iter()
                .map(|f| FieldInfo {
                    span: f.span,
                    name: f.name.name,
                    ty: lower_type(&f.ty, cx, diags, lint_cfg),
                    refinement: field_refinement_pred(f),
                })
                .collect();
            VariantPayloadInfo::Struct {
                fields: lowered.into_boxed_slice(),
            }
        }
    }
}

/// Extract a `TypeDecl`/variant-payload field's own inline `where`
/// predicate, if any. The predicate lives inside `f.ty`'s
/// `TypeKind::Refined { base, pred }` wrapper (`nanos: i64 where nanos
/// >= 0` parses its type position as `Refined { base: i64, pred:
/// "nanos >= 0" }`), not in `f.refinement` — that AST slot is never
/// populated by the current grammar for this position.
fn field_refinement_pred(f: &ast::TypeField) -> Option<Expr> {
    if let TypeKind::Refined { pred, .. } = &f.ty.kind {
        return Some(pred.clone());
    }
    f.refinement.clone()
}

/// Build a [`TyCx`] for an already-resolved package.
///
/// Walks every module in `package.modules()`, finds each Function /
/// TypeDecl / module-level `let` item by name in the parsed AST,
/// lowers its signature / layout / declared type, and inserts into
/// the resulting [`TyCx`]. `cx.package` should be `Some(package)` so
/// the embedded [`lower_type`] calls (for parameter / return / field
/// / let-annotation types) can resolve multi-segment Path type
/// expressions to nominal handles.
///
/// Module-level `let` initialisers are NOT evaluated here — the
/// declared annotation (which `declarations.md` §"Module-level let"
/// makes mandatory at module scope) supplies the type. Comptime-pure
/// evaluation of the initialiser is a later wave's responsibility
/// (`edda-comptime`).
///
/// Spec declarations are recognised but not yet lowered into the
/// context — spec-instantiation typing lands with `edda-comptime`.
pub(crate) fn build_ty_cx(
    package: &ResolvedPackage,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> TyCx {
    let mut ty_cx = TyCx::new();
    for module in package.modules() {
        let entry = package.module_entry(module.id);
        let ast = entry.ast.clone();
        for item in &ast.items {
            match &item.kind {
                ItemKind::Function(fn_decl) => {
                    // Outbound-generic templates (`function f<comptime
                    // U: Type>(...)`)
                    // have no concrete signature until the mono pass
                    // specializes them per call site — lowering here
                    // would register an Error-sentinel sig and emit a
                    // spurious "`U` is not a type" cascade.
                    if crate::mono::is_template(fn_decl) {
                        continue;
                    }
                    let Some(id) = module.items.lookup(fn_decl.name.name) else {
                        continue;
                    };
                    if module.items.get(id).kind != BindingKind::Function {
                        continue;
                    }
                    let sig = lower_fn_sig(fn_decl, cx, diags, lint_cfg);
                    ty_cx.insert_sig(id, sig);
                }
                ItemKind::TypeDecl(decl) => {
                    let Some(id) = module.items.lookup(decl.name.name) else {
                        continue;
                    };
                    if module.items.get(id).kind != BindingKind::TypeDecl {
                        continue;
                    }
                    let info = lower_type_decl(decl, cx, diags, lint_cfg);
                    ty_cx.insert_type_decl(id, info);
                }
                ItemKind::Let(let_decl) => {
                    let Some(id) = module.items.lookup(let_decl.name.name) else {
                        continue;
                    };
                    if module.items.get(id).kind != BindingKind::Const {
                        continue;
                    }
                    let ty = lower_type(&let_decl.ty, cx, diags, lint_cfg);
                    let init = fold_const_init(&let_decl.init, cx);
                    ty_cx.insert_const(id, ty, init);
                }
                ItemKind::SpecInvocation(si) => {
                    let Some(info) = lower_spec_invocation(si, package, cx, diags, lint_cfg)
                    else {
                        continue;
                    };
                    // Re-look up the invocation's binding id under its
                    // CA1 mangled short name. `walk_spec_invocation`'s
                    // pre-pass declared it during resolution; if the
                    // mangling failed (unsupported arg shape) the
                    // resolver skipped the declaration, so we skip too.
                    let Some(short_name) =
                        edda_resolve::mangle_spec_invocation_name(si, cx.interner)
                    else {
                        continue;
                    };
                    let Some(id) = module.items.lookup(short_name) else {
                        continue;
                    };
                    if module.items.get(id).kind != BindingKind::SpecInvocation {
                        continue;
                    }
                    ty_cx.insert_type_decl(id, info);
                }
                _ => {}
            }
        }
    }
    ty_cx
}

/// Lower one [`ast::SpecInvocation`] to its substituted [`TypeDeclInfo`].
///
/// The type-layer counterpart to `edda-codegen`'s
/// `substitute_spec_body` — restricted to the first `type` declaration
/// inside the spec body, with substitutions performed at the AST level
/// before the regular [`lower_type_decl`] runs.
///
/// Returns `None` when:
/// - the invocation's path does not resolve to a `Spec` binding,
/// - the resolved Spec's AST cannot be located in its owning module,
/// - the arity of `si.args` does not match `spec.generics`,
/// - any generic is non-`Type`-kind or its argument is not a Path or
///   a resolved nominal type,
/// - the spec body contains no `type` declaration.
///
/// All failure cases emit a `typecheck_error` diagnostic and the
/// caller carries on without registering the binding.
fn lower_spec_invocation(
    si: &ast::SpecInvocation,
    package: &ResolvedPackage,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<TypeDeclInfo> {
    // Locate the Spec declaration from the invocation's resolved path.
    let spec = locate_spec_decl(&si.path, package)?;

    // Build name -> substitution-type AST map.
    if spec.generics.len() != si.args.len() {
        // Arity mismatch is the resolver/codegen's domain to diagnose;
        // we silently skip here so we don't double-emit.
        return None;
    }
    let mut subs: AHashMap<Symbol, ast::Type> = AHashMap::with_capacity(spec.generics.len());
    for (gp, arg_expr) in spec.generics.iter().zip(si.args.iter()) {
        if !is_type_generic(gp) {
            // Only `Type`-kind and `comptime <T>: Type`-form
            // generics are admitted. Other comptime generics (`usize` values,
            // effect rows, user-defined values) are deferred.
            return None;
        }
        let arg_ty = arg_expr_to_ast_type(arg_expr)?;
        subs.insert(gp.name.name, arg_ty);
    }
    // Find the first `type` declaration in the spec body.
    let inner_decl = spec.body.iter().find_map(|item| match &item.kind {
        ItemKind::TypeDecl(d) => Some(d.as_ref()),
        _ => None,
    })?;

    // Substitute generic-param references throughout the type-decl, then
    // lower normally.
    let substituted = substitute_type_decl(inner_decl, &subs);
    Some(lower_type_decl(&substituted, cx, diags, lint_cfg))
}

/// Locate the [`Spec`] AST that the invocation's path resolves to.
fn locate_spec_decl<'a>(
    path: &ast::Path,
    package: &'a ResolvedPackage,
) -> Option<&'a Spec> {
    let Resolved::Binding(id) = package.resolutions().lookup_path(path.span)? else {
        return None;
    };
    let binding = package.binding(id);
    if binding.kind != BindingKind::Spec {
        return None;
    }
    let module = package.module_entry(binding.module);
    module.ast.items.iter().find_map(|item| match &item.kind {
        ItemKind::Spec(s) if s.name.name == binding.name => Some(s.as_ref()),
        _ => None,
    })
}

/// `true` when the generic parameter is a *type parameter* in the
/// substitution sense — even if syntactically spelled `comptime T: Type`.
fn is_type_generic(gp: &ast::GenericParam) -> bool {
    if matches!(gp.kind, GenericKind::Type) {
        return true;
    }
    if matches!(gp.kind, GenericKind::Comptime) {
        if let Some(ty) = gp.ty.as_ref()
            && matches!(ty.kind, TypeKind::Meta)
        {
            return true;
        }
    }
    false
}

/// Convert an admitted argument [`ast::Expr`] into an [`ast::Type`]
/// suitable for substitution into the spec body.
///
/// Admitted forms:
/// - `ExprKind::Path(p)` → `TypeKind::Path(p.clone())`. Both single-segment
///   primitive paths (`i32`) and multi-segment user-type paths
///   (`some.module.Foo`) work because the path's span carries the
///   original resolution record.
///
/// Every other expression form returns `None`; the caller surfaces this
/// as a spec-argument deferral.
fn arg_expr_to_ast_type(expr: &ast::Expr) -> Option<ast::Type> {
    match &expr.kind {
        ExprKind::Path(path) => Some(ast::Type {
            span: expr.span,
            kind: TypeKind::Path(path.clone()),
        }),
        _ => None,
    }
}

/// Clone an [`ast::TypeDecl`] with every single-segment Path-Type whose
/// head ident is in `subs` rewritten to the substituted type.
fn substitute_type_decl(
    decl: &ast::TypeDecl,
    subs: &AHashMap<Symbol, ast::Type>,
) -> ast::TypeDecl {
    let kind = match &decl.kind {
        TypeDeclKind::Product { fields } => TypeDeclKind::Product {
            fields: fields
                .iter()
                .map(|f| ast::TypeField {
                    span: f.span,
                    name: f.name.clone(),
                    ty: substitute_type(&f.ty, subs),
                    refinement: f.refinement.clone(),
                })
                .collect(),
        },
        TypeDeclKind::Sum { variants } => TypeDeclKind::Sum {
            variants: variants
                .iter()
                .map(|v| ast::Variant {
                    span: v.span,
                    name: v.name.clone(),
                    payload: substitute_variant_payload(&v.payload, subs),
                })
                .collect(),
        },
    };
    ast::TypeDecl {
        span: decl.span,
        stability: decl.stability,
        visibility: decl.visibility,
        linearity: decl.linearity,
        name: decl.name.clone(),
        generics: decl.generics.clone(),
        kind,
    }
}

fn substitute_variant_payload(
    payload: &VariantPayload,
    subs: &AHashMap<Symbol, ast::Type>,
) -> VariantPayload {
    match payload {
        VariantPayload::Unit => VariantPayload::Unit,
        VariantPayload::Tuple(elems) => {
            VariantPayload::Tuple(elems.iter().map(|t| substitute_type(t, subs)).collect())
        }
        VariantPayload::Struct(fields) => VariantPayload::Struct(
            fields
                .iter()
                .map(|f| ast::TypeField {
                    span: f.span,
                    name: f.name.clone(),
                    ty: substitute_type(&f.ty, subs),
                    refinement: f.refinement.clone(),
                })
                .collect(),
        ),
    }
}

/// Substitute generic-parameter references in an [`ast::Type`].
fn substitute_type(ty: &ast::Type, subs: &AHashMap<Symbol, ast::Type>) -> ast::Type {
    match &ty.kind {
        TypeKind::Path(path) if path.segments.len() == 1 => {
            if let Some(sub) = subs.get(&path.segments[0].name) {
                return sub.clone();
            }
            ty.clone()
        }
        TypeKind::Slice(elem) => ast::Type {
            span: ty.span,
            kind: TypeKind::Slice(Box::new(substitute_type(elem, subs))),
        },
        TypeKind::Tuple(elems) => ast::Type {
            span: ty.span,
            kind: TypeKind::Tuple(elems.iter().map(|t| substitute_type(t, subs)).collect()),
        },
        TypeKind::Function { params, ret, effects } => ast::Type {
            span: ty.span,
            kind: TypeKind::Function {
                params: params
                    .iter()
                    .map(|p| ast::FnTypeParam {
                        span: p.span,
                        name: p.name.clone(),
                        mode: p.mode,
                        ty: substitute_type(&p.ty, subs),
                    })
                    .collect(),
                ret: Box::new(substitute_type(ret, subs)),
                effects: effects.clone(),
            },
        },
        TypeKind::Refined { base, pred } => ast::Type {
            span: ty.span,
            kind: TypeKind::Refined {
                base: Box::new(substitute_type(base, subs)),
                pred: pred.clone(),
            },
        },
        // Path (multi-segment), Unit, Meta, Comptime, Error — no
        // single-segment generic reference can hide here; clone as-is.
        _ => ty.clone(),
    }
}

/// Fold a module-level `let` initialiser expression to a flat
/// [`ConstInit`]. Returns [`ConstInit::Unsupported`] for shapes
/// beyond literal / unary-negated-literal — see the type's docs for
/// the currently supported surface.
fn fold_const_init(expr: &ast::Expr, cx: &LowerCx<'_>) -> ConstInit {
    match &expr.kind {
        ExprKind::Literal(lit) => fold_literal(lit, cx, false),
        ExprKind::Unary { op: UnOp::Neg, expr: inner } => {
            if let ExprKind::Literal(lit) = &inner.kind {
                fold_literal(lit, cx, true)
            } else {
                ConstInit::Unsupported
            }
        }
        _ => ConstInit::Unsupported,
    }
}

/// Fold a single literal (optionally with an outer unary negation
/// applied) to a [`ConstInit`].
fn fold_literal(lit: &Literal, cx: &LowerCx<'_>, negate: bool) -> ConstInit {
    match lit {
        Literal::Int { value, .. } => {
            // Edda parses Int as u128. Stored signed; codegen narrows.
            // Reject values whose magnitude exceeds `i128::MAX` so we
            // never silently wrap on the cast — the user surface is
            // `Unsupported` and the reference site errors out cleanly
            // rather than silently producing a wrong value.
            if *value > i128::MAX as u128 {
                return ConstInit::Unsupported;
            }
            let signed = *value as i128;
            ConstInit::Int(if negate { -signed } else { signed })
        }
        Literal::Float(sym) => {
            let raw = cx.interner.resolve(*sym);
            match raw.parse::<f64>() {
                Ok(parsed) => {
                    let v = if negate { -parsed } else { parsed };
                    ConstInit::Float(v.to_bits())
                }
                Err(_) => ConstInit::Unsupported,
            }
        }
        Literal::Bool(b) if !negate => ConstInit::Bool(*b),
        Literal::Str(sym) if !negate => ConstInit::Str(*sym),
        // Bool / Str with a `-` prefix and Unit are not foldable
        // literal values.
        _ => ConstInit::Unsupported,
    }
}
