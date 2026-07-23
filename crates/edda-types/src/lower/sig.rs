//! Function-header lowering — `ast::FnDecl` → [`FnSig`].
//!
//! Composes type lowering and
//! effect-row lowering: each param's type lowers via [`lower_type`],
//! the return type ditto (absent return clause → [`Primitive::Unit`]),
//! the effect row via [`lower_effect_row`] (absent clause →
//! [`EffectRow::empty`]).
//!
//! Generics and refinement clauses are not part of [`FnSig`]: generics
//! need substitution machinery that lands with the spec-instantiation
//! pass; refinements are `edda-refine`'s responsibility.

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, Label, LintConfig};
use edda_syntax::ast;

use crate::effect::{EffectRow, GradedBound, GradedKind};
use crate::prim::Primitive;
use crate::sig::{FnSig, Param, ParamMode, ReturnMode};

use super::{LowerCx, row::lower_effect_row, ty::lower_type};

/// Lower a function-declaration header to its [`FnSig`].
///
/// Walks every positional parameter in source order, lowering each
/// type via [`lower_type`] and translating the mode via
/// [`ParamMode::from_ast`]. The return type comes from
/// [`ast::FnDecl::return_ty`] (absent → [`Primitive::Unit`] per
/// `docs/syntax/declarations.md`, *Function declarations*); the effect
/// row from [`ast::FnDecl::effects`] (absent → [`EffectRow::empty`]).
///
/// The function body, the function's *name* and *visibility*, generic
/// parameters, and refinement clauses (`requires` / `ensures`) are not
/// part of the type-system signature and are skipped here. Generic
/// parameters surface indirectly through the per-param-type lowering:
/// a reference to a generic type parameter inside a parameter or
/// return type produces a `typecheck_error` from [`lower_type`] and
/// the corresponding [`TyId`](crate::TyId) is the error sentinel. The
/// surrounding signature still materialises.
pub(crate) fn lower_fn_sig(
    decl: &ast::FnDecl,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> FnSig {
    let params: Vec<Param> = decl
        .params
        .iter()
        .map(|p| Param {
            span: p.span,
            name: p.name.name,
            mode: ParamMode::from_ast(p.mode),
            ty: lower_type(&p.ty, cx, diags, lint_cfg),
        })
        .collect();

    let return_ty = match &decl.return_ty {
        Some(t) => lower_type(t, cx, diags, lint_cfg),
        None => cx.ty_interner.prim(Primitive::Unit),
    };

    let effects = match &decl.effects {
        Some(row) => lower_effect_row(row, cx, diags, lint_cfg),
        None => EffectRow::empty(),
    };

    let graded_bounds = match &decl.effects {
        Some(row) => lower_graded_bounds(row, cx, diags, lint_cfg),
        None => Box::from([]),
    };

    FnSig {
        params: params.into_boxed_slice(),
        return_ty,
        return_mode: ReturnMode::from_ast(decl.return_mode),
        effects,
        graded_bounds,
        refinement_stable: decl.refinement_stable,
    }
}

/// Walk the AST row for `EffectMember::Graded` entries, validate each
/// against §5 of `02-modes-effects-refinements.md`, and return the
/// resulting [`GradedBound`] slice.
///
/// Per §5.2 the kind must be one of `alloc`, `io`, `time`. Per §5.3
/// the surface form is `kind(<resource_var> <= EXPR)` where the
/// resource variable is implicit (`bytes`/`calls`/`ops`); the RHS
/// expression is extracted for storage. Per §5.6 duplicate kinds in
/// one row are rejected.
///
/// All rejections route through
/// [`DiagnosticClass::EffectGradedBoundExceeded`].
fn lower_graded_bounds(
    row: &ast::EffectRow,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Box<[GradedBound]> {
    use ast::{BinOp, ExprKind};
    let mut out: Vec<GradedBound> = Vec::new();
    for member in &row.members {
        let ast::EffectMember::Graded { kind, bound } = member else {
            continue;
        };
        // Validate kind name.
        let kind_str = cx.interner.resolve(kind.name);
        let Some(graded_kind) = GradedKind::from_name(kind_str) else {
            emit_graded_error(
                diags,
                lint_cfg,
                kind.span,
                format!(
                    "unknown graded kind `{kind_str}` — the locked set is `alloc`, `io`, `time`"
                ),
            );
            continue;
        };
        // Validate bound shape: `<resource_var> <= EXPR`.
        let ExprKind::Binary { op: BinOp::Le, lhs, rhs } = &bound.kind else {
            emit_graded_error(
                diags,
                lint_cfg,
                bound.span,
                format!(
                    "graded bound must be `{var} <= EXPR` (the kind's resource variable on the left)",
                    var = graded_kind.resource_var()
                ),
            );
            continue;
        };
        // Validate LHS names the kind's resource variable.
        let ExprKind::Path(path) = &lhs.kind else {
            emit_graded_error(
                diags,
                lint_cfg,
                lhs.span,
                format!(
                    "graded bound's left operand must be `{var}` — the kind's resource variable",
                    var = graded_kind.resource_var()
                ),
            );
            continue;
        };
        let resource_ok = path.segments.len() == 1
            && cx.interner.resolve(path.segments[0].name) == graded_kind.resource_var();
        if !resource_ok {
            emit_graded_error(
                diags,
                lint_cfg,
                lhs.span,
                format!(
                    "graded bound's left operand must be `{var}` — the kind's resource variable",
                    var = graded_kind.resource_var()
                ),
            );
            continue;
        }
        // §5.6: per-kind uniqueness within a single row.
        if out.iter().any(|gb| gb.kind == graded_kind) {
            emit_graded_error(
                diags,
                lint_cfg,
                kind.span,
                format!(
                    "duplicate graded entry for `{}` in one row — a kind appears at most once",
                    graded_kind.as_str()
                ),
            );
            continue;
        }
        out.push(GradedBound {
            kind: graded_kind,
            bound: rhs.clone(),
            span: member_span(member),
        });
    }
    out.into_boxed_slice()
}

/// Best-effort source span for an [`ast::EffectMember`]. The
/// `EffectMember` variants don't carry their own span field, so we
/// derive one from the contained ident/expr/path.
fn member_span(member: &ast::EffectMember) -> edda_span::Span {
    match member {
        ast::EffectMember::Capability(id) => id.span,
        ast::EffectMember::Named { name, .. } => name.span,
        ast::EffectMember::Spread(path) => path.span,
        ast::EffectMember::Graded { kind, bound } => {
            edda_span::Span::new(kind.span.file, kind.span.lo, bound.span.hi)
        }
    }
}

/// Push a `EffectGradedBoundExceeded` diagnostic.
fn emit_graded_error(
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
    span: edda_span::Span,
    msg: impl Into<String>,
) {
    let sev = lint_cfg.effective(DiagnosticClass::EffectGradedBoundExceeded);
    let msg = msg.into();
    let label = Label::with_message(span, msg.clone());
    let d = Diagnostic::new(DiagnosticClass::EffectGradedBoundExceeded, sev, span, msg)
        .with_label(label.span, "");
    diags.push(d);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        Harness, ast_fn_decl, ast_param, ident_for, synthesize_unit_expr, ty_path,
    };
    use crate::{EffectEntry, ParamMode as TyParamMode, PureEffect};
    use edda_span::Span;
    use edda_syntax::ast::{Type, TypeKind};

    fn lower_sig(h: &mut Harness, decl: &ast::FnDecl) -> FnSig {
        let cx = LowerCx {
            interner: &h.interner,
            ty_interner: &h.ty_interner,
            package: None,
        };
        lower_fn_sig(decl, &cx, &mut h.diags, &h.lint_cfg)
    }

    #[test]
    fn header_without_return_lowers_to_unit() {
        let mut h = Harness::new();
        let decl = ast_fn_decl(&h.interner, "f", vec![], None, None);
        let sig = lower_sig(&mut h, &decl);
        assert_eq!(sig.arity(), 0);
        assert_eq!(sig.return_ty, h.ty_interner.prim(Primitive::Unit));
        assert!(sig.effects.is_empty());
        assert!(h.diags.is_empty());
    }

    #[test]
    fn header_with_return_lowers_return_ty() {
        let mut h = Harness::new();
        let decl = ast_fn_decl(
            &h.interner,
            "answer",
            vec![],
            Some(ty_path(&h.interner, "i64")),
            None,
        );
        let sig = lower_sig(&mut h, &decl);
        assert_eq!(sig.return_ty, h.ty_interner.prim(Primitive::I64));
        assert!(h.diags.is_empty());
    }

    #[test]
    fn params_lower_with_modes_preserved() {
        let mut h = Harness::new();
        let u8_ast = Type {
            span: Span::DUMMY,
            kind: TypeKind::Slice(Box::new(ty_path(&h.interner, "u8"))),
        };
        let decl = ast_fn_decl(
            &h.interner,
            "fill",
            vec![
                ast_param(
                    &h.interner,
                    "x",
                    ast::ParamMode::Default,
                    ty_path(&h.interner, "i32"),
                ),
                ast_param(&h.interner, "buf", ast::ParamMode::Mutable, u8_ast),
                ast_param(
                    &h.interner,
                    "data",
                    ast::ParamMode::Take,
                    ty_path(&h.interner, "String"),
                ),
                ast_param(
                    &h.interner,
                    "out",
                    ast::ParamMode::Init,
                    ty_path(&h.interner, "bool"),
                ),
            ],
            None,
            None,
        );
        let sig = lower_sig(&mut h, &decl);
        assert_eq!(sig.arity(), 4);

        let i32_id = h.ty_interner.prim(Primitive::I32);
        let u8_id = h.ty_interner.prim(Primitive::U8);
        let slice_u8 = h.ty_interner.slice(u8_id);
        let str_id = h.ty_interner.prim(Primitive::String);
        let bool_id = h.ty_interner.prim(Primitive::Bool);

        assert_eq!(sig.params[0].mode, TyParamMode::Default);
        assert_eq!(sig.params[0].ty, i32_id);
        assert_eq!(sig.params[1].mode, TyParamMode::Mutable);
        assert_eq!(sig.params[1].ty, slice_u8);
        assert_eq!(sig.params[2].mode, TyParamMode::Take);
        assert_eq!(sig.params[2].ty, str_id);
        assert_eq!(sig.params[3].mode, TyParamMode::Init);
        assert_eq!(sig.params[3].ty, bool_id);
        assert!(h.diags.is_empty());
    }

    #[test]
    fn effects_lower_alongside_signature() {
        let mut h = Harness::new();
        let row = ast::EffectRow {
            span: Span::DUMMY,
            members: vec![
                ast::EffectMember::Capability(ident_for(&h.interner, "allocator")),
                ast::EffectMember::Named {
                    name: ident_for(&h.interner, "err"),
                    ty: ty_path(&h.interner, "i32"),
                },
            ],
        };
        let decl = ast_fn_decl(
            &h.interner,
            "alloc_widget",
            vec![],
            Some(ty_path(&h.interner, "String")),
            Some(row),
        );
        let sig = lower_sig(&mut h, &decl);
        assert_eq!(sig.effects.len(), 2);
        let alloc_sym = h.interner.intern("allocator");
        let i32_id = h.ty_interner.prim(Primitive::I32);
        assert!(sig.effects.contains(&EffectEntry::Capability(alloc_sym)));
        assert!(sig.effects.contains(&EffectEntry::Pure(PureEffect::Err(i32_id))));
        assert!(h.diags.is_empty());
    }

    #[test]
    fn generic_param_reference_cascades_to_error() {
        let mut h = Harness::new();
        let decl = ast_fn_decl(
            &h.interner,
            "f",
            vec![ast_param(
                &h.interner,
                "x",
                ast::ParamMode::Default,
                ty_path(&h.interner, "T"),
            )],
            Some(ty_path(&h.interner, "T")),
            None,
        );
        let sig = lower_sig(&mut h, &decl);
        assert_eq!(sig.arity(), 1);
        assert_eq!(sig.params[0].ty, h.ty_interner.error());
        assert_eq!(sig.return_ty, h.ty_interner.error());
        assert_eq!(h.diags.error_count(), 2);
    }

    #[test]
    fn refinement_clauses_are_silently_ignored() {
        let mut h = Harness::new();
        let mut decl = ast_fn_decl(
            &h.interner,
            "f",
            vec![ast_param(
                &h.interner,
                "x",
                ast::ParamMode::Default,
                ty_path(&h.interner, "i32"),
            )],
            Some(ty_path(&h.interner, "i32")),
            None,
        );
        decl.refinements = vec![ast::RefinementClause {
            span: Span::DUMMY,
            kind: ast::RefinementKind::Requires,
            pred: synthesize_unit_expr(),
        }];
        let sig = lower_sig(&mut h, &decl);
        assert_eq!(sig.arity(), 1);
        assert_eq!(sig.params[0].ty, h.ty_interner.prim(Primitive::I32));
        assert!(h.diags.is_empty());
    }
}
