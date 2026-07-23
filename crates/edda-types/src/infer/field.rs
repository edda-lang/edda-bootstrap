//! Field-access inference.
//!
//! Implements **T-FieldAccess** from `inference-rules.md §1a.4`:
//!
//! ```text
//!    Γ ⊢ e ⇒ T uses R    T = ProductType{ ..., f : Tᶠ, ... }
//!    Σ(e.f) ≠ consumed
//!    ─────────────────────────────────────────────────────────  (T-FieldAccess)
//!    Γ ⊢ e.f ⇒ Tᶠ uses R
//! ```
//!
//! The §5 per-field state gate: when the receiver is
//! a single-segment binding, the field's per-field state (derived
//! from the binding's [`BindingState`]) must be `Valid` for the read
//! to succeed. `Uninit` and `Consumed` fields emit a diagnostic and
//! return the error sentinel. Non-trivial receivers (other
//! expressions, nested field chains) skip the gate — those flow
//! through standard receiver synthesis whose read-gate fires on the
//! whole binding.
//!
//! Sum-variant payload projection (`Color.red.payload_field`) is
//! deferred: it requires pattern-style narrowing that is
//! not yet provided.

use edda_intern::Symbol;
use edda_span::Span;
use edda_syntax::ast::Ident;

use crate::cx::TypeDeclShape;
use crate::hir::HirExpr;
use crate::ty::{TyId, TyKind};

use super::mode::{path_binding, BindingState};
use super::{InferCx, TyEnv, synth_expr};

/// Synthesise a `receiver.field` expression.
///
/// When `receiver` is a single-segment path naming a binding, the §5
/// per-field gate fires: the field's derived state (from the
/// binding's [`BindingState`]) must be [`BindingState::Valid`].
/// Otherwise the receiver synthesises through [`synth_expr`] (which
/// applies its own read-gate on the whole binding).
pub(super) fn synth_field(
    env: &mut TyEnv,
    receiver: &mut HirExpr,
    name: Ident,
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    // §5 per-field gate: when receiver is a binding, check the field's
    // state before allowing the read. The check fires BEFORE
    // synth_expr so a binding in `PartialInit` doesn't trigger the
    // whole-binding read-gate first.
    let receiver_binding = path_binding(receiver);
    if let Some(binding_sym) = receiver_binding {
        if let Some(state) = env.lookup_state(binding_sym) {
            let field_state = state.field_state(name.name);
            if !matches!(field_state, BindingState::Valid) {
                let bname = ic.lower.interner.resolve(binding_sym).to_string();
                let fname = ic.lower.interner.resolve(name.name).to_string();
                ic.emit_typecheck_error(
                    span,
                    format!(
                        "cannot read `{bname}.{fname}`: the field is {} here",
                        field_state.describe(),
                    ),
                );
                // Run synth_expr on the receiver so its `ty` field
                // still gets stamped (but it'll diagnose too — that's
                // accepted: the field-level diag is the primary one).
                let _ = synth_expr(env, receiver, ic);
                return ic.ty_error();
            }
            // Stamp the receiver's `ty` from the env without
            // routing through synth_expr — that would trigger the
            // whole-binding read-gate on a `PartialInit` binding.
            receiver.ty = env.lookup(binding_sym).unwrap_or_else(|| ic.ty_error());
            return synth_field_after_gate(receiver, name, ic, span);
        }
    }
    let recv_ty = synth_expr(env, receiver, ic);
    let error = ic.ty_error();
    if recv_ty == error {
        return error;
    }
    synth_field_after_gate(receiver, name, ic, span)
}

/// Field lookup after the §5 per-field gate (or after `synth_expr` for
/// non-trivial receivers). Verifies the receiver's type is a nominal
/// product type that declares the requested field and returns the
/// field's TyId, or emits a diagnostic and returns `Error`.
fn synth_field_after_gate(
    receiver: &HirExpr,
    name: Ident,
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    let recv_ty = receiver.ty;
    let error = ic.ty_error();
    if recv_ty == error {
        return error;
    }
    let binding_id = match ic.lower.ty_interner.kind(recv_ty) {
        TyKind::Nominal(id) => *id,
        _ => {
            let recv_display = ic.display_ty(recv_ty).to_string();
            let field_name = ic.lower.interner.resolve(name.name).to_string();
            ic.emit_typecheck_error(
                span,
                format!(
                    "cannot access field `.{field_name}` on `{recv_display}`; \
                     only product types support field access",
                ),
            );
            return error;
        }
    };

    let field_ty = lookup_field(ic, binding_id, name.name);
    match field_ty {
        Some(t) => t,
        None => {
            let recv_display = ic.display_ty(recv_ty).to_string();
            let field_name = ic.lower.interner.resolve(name.name).to_string();
            ic.emit_typecheck_error(
                span,
                format!("type `{recv_display}` has no field `{field_name}`"),
            );
            error
        }
    }
}


/// Look up a field type on a nominal product. Returns `None` if the
/// nominal type is not a product or the field is not declared.
///
/// Extracts the `TyId` out of the `TyCx` borrow before returning so
/// callers can use `ic` mutably afterward.
fn lookup_field(
    ic: &InferCx<'_>,
    binding_id: edda_resolve::BindingId,
    field: Symbol,
) -> Option<TyId> {
    let info = ic.ty_cx.type_decl(binding_id)?;
    match &info.kind {
        TypeDeclShape::Product { fields } => fields.iter().find(|f| f.name == field).map(|f| f.ty),
        TypeDeclShape::Sum { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cx::TyCx;
    use crate::lower::LowerCx;
    use crate::lower::lower_expr;
    use crate::test_support::{Harness, ident_for};
    use edda_syntax::ast::{Expr, ExprKind, Literal};

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
        synth_expr(
            &mut env,
            &mut hir,
            &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
        )
    }

    #[test]
    fn field_on_integer_diagnoses() {
        // `1.foo` — receiver is i64, not a product type.
        let mut h = Harness::new();
        let field = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Field {
                receiver: Box::new(lit_int(1)),
                name: ident_for(&h.interner, "foo"),
            },
        };
        let ty = lower_and_synth(&mut h, &field);
        assert_eq!(ty, h.ty_interner.error());
        assert!(
            h.diags
                .iter()
                .any(|d| d.message.contains("only product types support field access")),
        );
    }

    #[test]
    fn field_on_error_receiver_propagates_silently() {
        // `unknown.foo` — receiver synth fails (no binding `unknown`);
        // the field projection must NOT add a second diagnostic.
        let mut h = Harness::new();
        let field = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Field {
                receiver: Box::new(Expr {
                    span: Span::DUMMY,
                    kind: ExprKind::Path(crate::test_support::path_for(
                        &h.interner,
                        &["unknown"],
                    )),
                }),
                name: ident_for(&h.interner, "foo"),
            },
        };
        let ty = lower_and_synth(&mut h, &field);
        assert_eq!(ty, h.ty_interner.error());
        // Exactly one diagnostic — from the unknown binding lookup.
        assert_eq!(h.diags.error_count(), 1);
    }
}
