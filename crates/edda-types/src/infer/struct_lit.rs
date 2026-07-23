//! Struct-literal inference.
//!
//! Per `inference-rules.md §1`, *Checking unfolds for forms that need
//! an expected type* — struct / variant field initialisers are
//! checking positions:
//!
//! ```text
//!    path resolves to a `BindingKind::TypeDecl` binding with product shape
//!    declared fields = { f₁ : T₁, …, fₙ : Tₙ }
//!    (Γ ⊢ eᵢ ⇐ Tᵢ uses Rᵢ)ᵢ₌₁ⁿ        every required field initialised exactly once
//!    ──────────────────────────────────────────────────────────────────────  (T-StructLit)
//!    Γ ⊢ Path { f₁: e₁, …, fₙ: eₙ } ⇒ Nominal(BindingId) uses ⋃ᵢ Rᵢ
//! ```
//!
//! This enforces:
//! - the path must resolve to a `TypeDecl` binding (`Nominal(id)`);
//! - the type must be a product (sum-variant struct payloads are a
//!   future task);
//! - every initialiser checks against the declared field type;
//! - duplicate field names diagnose;
//! - missing required fields diagnose;
//! - unknown field names diagnose.

use edda_intern::Symbol;
use edda_resolve::{BindingKind, Resolved};
use edda_span::Span;

use crate::cx::TypeDeclShape;
use crate::hir::{HirPath, HirStructLitField};
use crate::ty::TyId;

use super::{InferCx, TyEnv, check_expr};

/// Synthesise a `Path { f1: e1, ... }` struct-literal expression.
pub(super) fn synth_struct_lit(
    env: &mut TyEnv,
    path: &HirPath,
    fields: &mut [HirStructLitField],
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    let path_span = path.span;

    // Resolve the path to a TypeDecl binding via the package's
    // `Resolutions` map.
    let package = match ic.lower.package {
        Some(p) => p,
        None => {
            synth_initializers_for_diagnostics(env, fields, ic);
            ic.emit_typecheck_error(
                span,
                "struct literals require a resolved-package context",
            );
            return ic.ty_error();
        }
    };
    let (binding_id, entry_name, entry_kind) = match package
        .resolutions()
        .lookup_path(path_span)
    {
        Some(Resolved::Binding(id)) => {
            let entry = package.binding(id);
            (id, entry.name, entry.kind)
        }
        Some(Resolved::Module(_)) => {
            synth_initializers_for_diagnostics(env, fields, ic);
            ic.emit_typecheck_error(
                span,
                "module name used as a struct type; expected a `type` declaration",
            );
            return ic.ty_error();
        }
        Some(Resolved::Error) => {
            synth_initializers_for_diagnostics(env, fields, ic);
            return ic.ty_error();
        }
        None => {
            synth_initializers_for_diagnostics(env, fields, ic);
            ic.emit_typecheck_error(
                span,
                "struct-literal path has no recorded resolution",
            );
            return ic.ty_error();
        }
    };
    if !matches!(entry_kind, BindingKind::TypeDecl | BindingKind::SpecInvocation) {
        let name = ic.lower.interner.resolve(entry_name).to_string();
        let kind_label = match entry_kind {
            BindingKind::Function => "function",
            BindingKind::Spec => "spec",
            BindingKind::Const => "constant",
            BindingKind::ImportLeaf => "module",
            BindingKind::Param | BindingKind::Local => "binding",
            BindingKind::TypeDecl | BindingKind::SpecInvocation => unreachable!(),
        };
        synth_initializers_for_diagnostics(env, fields, ic);
        ic.emit_typecheck_error(
            span,
            format!("cannot construct {kind_label} `{name}` as a struct"),
        );
        return ic.ty_error();
    }

    // Extract the product's declared field table out of the `ty_cx`
    // borrow so we can use `ic` mutably afterward.
    let product_fields = match ic.ty_cx.type_decl(binding_id) {
        Some(info) => match &info.kind {
            TypeDeclShape::Product { fields } => fields
                .iter()
                .map(|f| (f.name, f.ty))
                .collect::<Vec<(Symbol, TyId)>>(),
            TypeDeclShape::Sum { .. } => {
                let name = ic.lower.interner.resolve(entry_name).to_string();
                synth_initializers_for_diagnostics(env, fields, ic);
                ic.emit_typecheck_error(
                    span,
                    format!(
                        "cannot construct sum type `{name}` with struct-literal syntax; \
                         use a variant constructor instead",
                    ),
                );
                return ic.ty_error();
            }
        },
        None => {
            let name = ic.lower.interner.resolve(entry_name).to_string();
            synth_initializers_for_diagnostics(env, fields, ic);
            ic.emit_typecheck_error(
                span,
                format!("no layout recorded for type `{name}`"),
            );
            return ic.ty_error();
        }
    };

    // Check each provided field initialiser against the declared
    // field type. Track which declared fields have been initialised
    // so we can diagnose missing / duplicate ones.
    let mut seen = vec![false; product_fields.len()];
    for hir_field in fields.iter_mut() {
        let field_sym = hir_field.name.name;
        match product_fields.iter().position(|(n, _)| *n == field_sym) {
            Some(idx) => {
                let declared_ty = product_fields[idx].1;
                check_expr(env, &mut hir_field.value, declared_ty, ic);
                // An explicit `take`/`mutable`/`init` prefix on the field
                // drives the §4/§5 binding-state transition, exactly as a
                // call argument does.
                super::mode::apply_struct_field_mode_transition(env, hir_field, ic);
                if seen[idx] {
                    let field_name = ic.lower.interner.resolve(field_sym).to_string();
                    ic.emit_typecheck_error(
                        hir_field.span,
                        format!("duplicate initialiser for field `{field_name}`"),
                    );
                } else {
                    seen[idx] = true;
                }
            }
            None => {
                let name = ic.lower.interner.resolve(entry_name).to_string();
                let field_name = ic.lower.interner.resolve(field_sym).to_string();
                let _ = super::synth_expr(env, &mut hir_field.value, ic);
                ic.emit_typecheck_error(
                    hir_field.span,
                    format!("type `{name}` has no field `{field_name}`"),
                );
            }
        }
    }

    // Diagnose any uninitialised fields.
    let missing: Vec<Symbol> = product_fields
        .iter()
        .zip(seen.iter())
        .filter_map(|((n, _), present)| if *present { None } else { Some(*n) })
        .collect();
    if !missing.is_empty() {
        let name = ic.lower.interner.resolve(entry_name).to_string();
        let missing_names: Vec<String> = missing
            .iter()
            .map(|s| format!("`{}`", ic.lower.interner.resolve(*s)))
            .collect();
        ic.emit_typecheck_error(
            span,
            format!(
                "struct literal for `{name}` is missing field(s) {}",
                missing_names.join(", "),
            ),
        );
    }

    ic.lower.ty_interner.nominal(binding_id)
}

/// Synthesise every initialiser for its side-effect (sub-expression
/// type-error surfacing). Used by error paths to surface diagnostics
/// for ill-typed initialisers even when the surrounding literal
/// itself is malformed.
fn synth_initializers_for_diagnostics(
    env: &mut TyEnv,
    fields: &mut [HirStructLitField],
    ic: &mut InferCx<'_>,
) {
    for f in fields.iter_mut() {
        let _ = super::synth_expr(env, &mut f.value, ic);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cx::TyCx;
    use crate::lower::LowerCx;
    use crate::lower::lower_expr;
    use crate::test_support::{Harness, ident_for, path_for};
    use edda_syntax::ast::{Expr, ExprKind, Literal, StructLitField as AstStructLitField};

    fn lit_int(value: u128) -> Expr {
        Expr {
            span: Span::DUMMY,
            kind: ExprKind::Literal(Literal::Int {
                value,
                base: edda_syntax::IntBase::Dec,
            }),
        }
    }

    fn lower_and_synth(h: &mut Harness, ast: &Expr) -> TyId {
        let cx = LowerCx {
            interner: &h.interner,
            ty_interner: &h.ty_interner,
            package: None,
        };
        let ty_cx = TyCx::new();
        let mut hir = lower_expr(ast, &cx, &mut h.diags, &h.lint_cfg);
        let mut env = TyEnv::new();
        super::super::synth_expr(
            &mut env,
            &mut hir,
            &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
        )
    }

    #[test]
    fn struct_lit_without_package_diagnoses() {
        let mut h = Harness::new();
        let lit = Expr {
            span: Span::DUMMY,
            kind: ExprKind::StructLit {
                path: path_for(&h.interner, &["Point"]),
                fields: vec![AstStructLitField {
                    span: Span::DUMMY,
                    name: ident_for(&h.interner, "x"),
                    mode: None,
                    value: lit_int(0),
                }],
            },
        };
        let ty = lower_and_synth(&mut h, &lit);
        assert_eq!(ty, h.ty_interner.error());
        assert!(
            h.diags
                .iter()
                .any(|d| d.message.contains("resolved-package")),
        );
    }
}
