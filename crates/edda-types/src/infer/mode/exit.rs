//! §4 function-body exit check + the droppability predicate.
//!
//! `check_fn_body_exit` verifies every in-scope binding is `Valid` (or
//! legitimately `Consumed`) at function exit; `is_trivially_droppable_at_exit`
//! powers the `take`-param `PartialInit` relaxation.

use super::helpers::type_field_set;
use super::lattice::BindingState;

/// True when a value of `ty` can be silently dropped at function exit
/// without leaking owned resources. Walks `TyKind` recursively:
///
/// - `Primitive(p)` — `p.is_trivially_droppable()` (every integer / float
///   width, `bool`, `()`, `never`, `Type`, `Codepoint`; refuses `String`
///   and `HeapPtr`).
/// - `Tuple(elems)` — every element droppable.
/// - `Nominal(binding_id)` — looks up the [`TypeDeclInfo`]:
///   - If the declaration is `linear` or `affine`, refused (the
///     consumption discipline must be explicit; the bootstrap is
///     conservative and treats `affine` like `linear` here, matching
///     the canonical destructor pattern's fix shape).
///   - `Product { fields }` — every field droppable.
///   - `Sum { variants }` — every variant's payload droppable; `Unit`
///     payloads trivially qualify (this is the case that pattern
///     was blocked on).
///   - Missing layout (cascade or pre-registration miss) — refused
///     conservatively rather than admitting silently.
/// - `Slice(_)`, `Capability(_)`, `FnPtr(_)` — refused (heap-backed,
///   capability, or function-pointer bindings that the spec treats as
///   runtime resources requiring explicit handling).
/// - `Error` — refused; the cascade already reported elsewhere, so
///   admitting it here would silence the reason a user sees diagnostics.
///
/// `visited` carries the recursive set so a self-referential ADT
/// (admitted in principle via `Box(T)` indirection) cannot drive
/// the walker into infinite recursion. Revisiting a `TyId` returns
/// `false` — conservative, since a cyclic ADT typically reaches its
/// own owning storage and that storage is not droppable today.
fn is_trivially_droppable_at_exit(
    ty: crate::ty::TyId,
    ic: &super::super::InferCx<'_>,
) -> bool {
    let mut visited: std::collections::BTreeSet<crate::ty::TyId> =
        std::collections::BTreeSet::new();
    droppable_walk(ty, ic, &mut visited)
}

fn droppable_walk(
    ty: crate::ty::TyId,
    ic: &super::super::InferCx<'_>,
    visited: &mut std::collections::BTreeSet<crate::ty::TyId>,
) -> bool {
    if !visited.insert(ty) {
        return false;
    }
    match ic.lower.ty_interner.kind(ty) {
        crate::ty::TyKind::Primitive(p) => p.is_trivially_droppable(),
        crate::ty::TyKind::Tuple(elems) => {
            elems.iter().all(|e| droppable_walk(*e, ic, visited))
        }
        crate::ty::TyKind::Nominal(binding_id) => {
            let Some(decl) = ic.ty_cx.type_decl(*binding_id) else {
                return false;
            };
            if decl.linearity.is_some() {
                return false;
            }
            match &decl.kind {
                crate::cx::TypeDeclShape::Product { fields } => {
                    fields.iter().all(|f| droppable_walk(f.ty, ic, visited))
                }
                crate::cx::TypeDeclShape::Sum { variants } => variants.iter().all(|v| {
                    match &v.payload {
                        crate::cx::VariantPayloadInfo::Unit => true,
                        crate::cx::VariantPayloadInfo::Tuple { elems } => {
                            elems.iter().all(|e| droppable_walk(*e, ic, visited))
                        }
                        crate::cx::VariantPayloadInfo::Struct { fields } => {
                            fields.iter().all(|f| droppable_walk(f.ty, ic, visited))
                        }
                    }
                }),
            }
        }
        crate::ty::TyKind::Slice(_)
        | crate::ty::TyKind::Capability(_)
        | crate::ty::TyKind::FnPtr(_)
        | crate::ty::TyKind::Error => false,
    }
}

/// Verify that every in-scope binding is [`BindingState::Valid`] at
/// the function's exit point per `inference-rules.md §4`. Each
/// non-`Valid` binding produces one diagnostic attributed to
/// `exit_span`. `take_param_syms` is the set of parameter symbols
/// declared with the `take` mode — `PartialInit` bindings whose
/// symbol is in the set and whose remaining-valid fields are all
/// trivially droppable are admitted without a diagnostic.
pub(crate) fn check_fn_body_exit(
    env: &super::super::TyEnv,
    ic: &mut super::super::InferCx<'_>,
    exit_span: edda_span::Span,
    take_param_syms: &[edda_intern::Symbol],
) {
    let mut reported: std::collections::HashSet<edda_intern::Symbol> =
        std::collections::HashSet::new();
    // Snapshot the iterator into a Vec so we drop the env borrow
    // before threading `ic` into the diagnostic emission.
    let snapshot: Vec<(edda_intern::Symbol, BindingState)> = env.iter_states().collect();
    for (sym, state) in snapshot {
        // A binding that ends `Consumed` was legitimately moved out via
        // `take` or destructuring — that is the expected terminal state
        // for owning values handed onwards. Per the spirit of
        // `inference-rules.md §4` (no value escapes ownership), the
        // exit-side rule only forbids `Uninit` (declared but never
        // assigned) and `PartialInit` (some fields still uninitialised);
        // `Consumed` is admissible.
        if state == BindingState::Valid
            || state == BindingState::Consumed
            || reported.contains(&sym)
        {
            continue;
        }
        match &state {
            BindingState::PartialInit(valid_fields) => {
                // §5 function-exit rule: every field of every in-scope
                // binding must be Valid. Report each non-Valid field
                // by name so the user knows what's missing.
                let binding_ty = match env.lookup(sym) {
                    Some(t) => t,
                    None => continue,
                };
                let full_set = match type_field_set(binding_ty, ic) {
                    Some(s) => s,
                    None => continue,
                };
                let missing: Vec<edda_intern::Symbol> = full_set
                    .into_iter()
                    .filter(|f| !valid_fields.contains(f))
                    .collect();
                if missing.is_empty() {
                    continue; // Should have been promoted to Valid.
                }
                // Take-param relaxation: when the binding is a `take`-mode
                // parameter and every still-valid field's type is trivially
                // droppable per [`is_trivially_droppable_at_exit`], admit
                // the partial state silently. The missing fields can only
                // be `Consumed` (take params start `Valid` and the field
                // tracker has no Valid→Uninit transition), so the
                // remaining-valid fields are the only ones that would leak
                // if dropped — and the recursive droppability predicate
                // covers scalar primitives, tuples thereof, and freely-
                // copyable user-defined product / sum types composed of
                // droppable parts.
                if take_param_syms.contains(&sym) {
                    let valid_field_tys = valid_fields.iter().map(|f| {
                        // Look up the field's declared type on the binding's
                        // product layout. Missing-from-layout is unreachable
                        // (the field tracker only admits fields in `full_set`),
                        // but fall through conservatively if it ever happens.
                        field_ty(binding_ty, *f, ic)
                    });
                    let all_droppable = valid_field_tys
                        .clone()
                        .all(|ty| ty.is_some_and(|t| is_trivially_droppable_at_exit(t, ic)));
                    if all_droppable && valid_field_tys.count() == valid_fields.len() {
                        continue;
                    }
                }
                let bname = ic.lower.interner.resolve(sym).to_string();
                let fields_rendered: Vec<String> = missing
                    .iter()
                    .map(|f| ic.lower.interner.resolve(*f).to_string())
                    .collect();
                ic.emit_typecheck_error(
                    exit_span,
                    format!(
                        "binding `{bname}` is partially initialised at function exit; \
                         these fields are not valid: {}",
                        fields_rendered.join(", "),
                    ),
                );
            }
            _ => {
                let name = ic.lower.interner.resolve(sym).to_string();
                ic.emit_typecheck_error(
                    exit_span,
                    format!(
                        "binding `{name}` is {} at function exit; \
                         every in-scope binding must be valid",
                        state.describe(),
                    ),
                );
            }
        }
        reported.insert(sym);
    }
}

/// Look up a single field's declared type on a nominal product layout.
/// Returns `None` for non-product / unknown types or fields not declared
/// on the layout — both are unreachable by construction at the
/// function-exit call site (the field tracker only admits fields the
/// product layout declares).
fn field_ty(
    binding_ty: crate::ty::TyId,
    field: edda_intern::Symbol,
    ic: &super::super::InferCx<'_>,
) -> Option<crate::ty::TyId> {
    let crate::ty::TyKind::Nominal(binding_id) = ic.lower.ty_interner.kind(binding_ty) else {
        return None;
    };
    let decl = ic.ty_cx.type_decl(*binding_id)?;
    match &decl.kind {
        crate::cx::TypeDeclShape::Product { fields } => {
            fields.iter().find(|f| f.name == field).map(|f| f.ty)
        }
        crate::cx::TypeDeclShape::Sum { .. } => None,
    }
}
