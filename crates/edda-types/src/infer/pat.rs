//! Pattern binding — the recursive walk that extends [`TyEnv`] with names introduced by [`HirPat`].
//!
//! Called from [`super::infer_stmt`]'s `Let` arm and [`super::ctrl::synth_match`]'s per-arm scope.

use edda_span::Span;

use super::mode::BindingState;
use super::{InferCx, TyEnv, check_expr};
use crate::cx::{TypeDeclShape, VariantPayloadInfo};
use crate::hir::{HirPat, HirPatKind, HirPath, HirStructPatField, HirVariantPatPayload};
use crate::prim::Primitive;
use crate::ty::{TyId, TyKind};

/// Bind every name introduced by `pat` to `ty` in `env`. Sets the
/// pattern's `ty` field along the way.
///
/// Every [`HirPatKind`] is handled except `Error` (which is the
/// already-diagnosed parser-recovery sentinel and contributes no
/// bindings):
///
/// - **Wildcard / Literal** — no bindings; `pat.ty = ty` only.
/// - **Binding** — bind the ident at `ty` in the innermost frame.
/// - **Tuple** — scrutinee must be `TyKind::Tuple` with matching
///   arity; recurse element-wise.
/// - **Variant** — scrutinee must be `TyKind::Nominal(sum_id)`; the
///   last path segment is the variant name; recurse on the payload
///   against the declared payload types.
/// - **Struct** — scrutinee must be `TyKind::Nominal(prod_id)`; bind
///   each field pattern at the declared field type; `rest: false`
///   requires every declared field to appear, `rest: true` permits
///   omissions.
/// - **Guard** — recurse on the inner pattern, then check `cond ⇐ bool`.
pub(super) fn bind_pat(env: &mut TyEnv, pat: &mut HirPat, ty: TyId, ic: &mut InferCx<'_>) {
    bind_pat_with_state(env, pat, ty, BindingState::Valid, ic)
}

/// `bind_pat` with an explicit initial [`BindingState`] for every
/// name the pattern introduces. The `var x: T` (no initialiser) path
/// in [`infer_stmt`] passes [`BindingState::Uninit`]; every other
/// caller passes [`BindingState::Valid`]. Names bound this way are
/// permissively mutable — only [`bind_pat_with_state_mut`] stamps a
/// precise mutability (the `let`/`var` statement path).
pub(super) fn bind_pat_with_state(
    env: &mut TyEnv,
    pat: &mut HirPat,
    ty: TyId,
    state: BindingState,
    ic: &mut InferCx<'_>,
) {
    bind_pat_with_state_mut(env, pat, ty, state, true, ic);
}

/// [`bind_pat_with_state`] that also stamps a binding mutability.
/// The `let`/`var` statement path passes the declaration's mutability so
/// the mode checker rejects a `mutable`/`init` borrow of a `let`
/// binding. Destructuring sub-patterns
/// still bind permissively (`mutable = true`) until per-element
/// mutability is threaded.
pub(super) fn bind_pat_with_state_mut(
    env: &mut TyEnv,
    pat: &mut HirPat,
    ty: TyId,
    state: BindingState,
    mutable: bool,
    ic: &mut InferCx<'_>,
) {
    pat.ty = ty;
    let pat_span = pat.span;
    match &mut pat.kind {
        HirPatKind::Wildcard => {}
        HirPatKind::Binding(ident) => env.bind_with_state_mut(ident.name, ty, state, mutable),
        HirPatKind::Literal(_) => {}
        HirPatKind::Tuple(elems) => bind_tuple_pat(env, elems, ty, ic, pat_span),
        HirPatKind::Variant { path, payload } => {
            bind_variant_pat(env, path, payload, ty, ic, pat_span);
        }
        HirPatKind::Struct {
            path,
            fields,
            rest,
        } => {
            bind_struct_pat(env, path, fields, *rest, ty, ic, pat_span);
        }
        HirPatKind::Guard { pat: inner, cond } => {
            bind_pat_with_state_mut(env, inner, ty, state, mutable, ic);
            let bool_ty = ic.lower.ty_interner.prim(Primitive::Bool);
            check_expr(env, cond, bool_ty, ic);
        }
        // A range pattern binds no names; the bounds are literals.
        HirPatKind::Range { .. } => {}
        // `name @ inner` binds the whole value at `ty`, then matches the
        // sub-pattern against the same scrutinee type.
        HirPatKind::AtBinding { name, inner } => {
            env.bind_with_state_mut(name.name, ty, state.clone(), mutable);
            bind_pat_with_state_mut(env, inner, ty, state, mutable, ic);
        }
        HirPatKind::Slice {
            prefix,
            rest,
            suffix,
        } => {
            bind_slice_pat(env, prefix, rest, suffix, ty, ic, pat_span);
        }
        HirPatKind::Error => {}
    }
}

/// Bind names introduced by a slice pattern. The scrutinee must be a
/// `TyKind::Slice(elem)`; prefix / suffix sub-patterns bind at `elem`,
/// and a `..name` rest binds at the whole slice type.
fn bind_slice_pat(
    env: &mut TyEnv,
    prefix: &mut [HirPat],
    rest: &mut Option<Option<edda_syntax::ast::Ident>>,
    suffix: &mut [HirPat],
    scrut_ty: TyId,
    ic: &mut InferCx<'_>,
    span: Span,
) {
    let error = ic.ty_error();
    let elem_ty = if scrut_ty == error {
        error
    } else {
        match ic.lower.ty_interner.kind(scrut_ty) {
            TyKind::Slice(elem) => *elem,
            _ => {
                let display = ic.display_ty(scrut_ty).to_string();
                ic.emit_typecheck_error(
                    span,
                    format!("slice pattern requires a slice type; scrutinee has type `{display}`"),
                );
                error
            }
        }
    };
    for p in prefix.iter_mut() {
        bind_pat(env, p, elem_ty, ic);
    }
    if let Some(Some(name)) = rest {
        // The `..name` rest binds the remaining elements as a sub-slice
        // of the same type as the scrutinee.
        let rest_ty = if elem_ty == error { error } else { scrut_ty };
        env.bind_with_state_mut(name.name, rest_ty, BindingState::Valid, true);
    }
    for p in suffix.iter_mut() {
        bind_pat(env, p, elem_ty, ic);
    }
}

fn bind_tuple_pat(
    env: &mut TyEnv,
    elems: &mut [HirPat],
    scrut_ty: TyId,
    ic: &mut InferCx<'_>,
    span: Span,
) {
    let error = ic.ty_error();
    if scrut_ty == error {
        for elem in elems.iter_mut() {
            bind_pat(env, elem, error, ic);
        }
        return;
    }

    // Extract element types — clone-out so we drop the ty_cx borrow.
    let elem_tys: Option<Vec<TyId>> = match ic.lower.ty_interner.kind(scrut_ty) {
        TyKind::Tuple(ts) if ts.len() == elems.len() => Some(ts.to_vec()),
        TyKind::Tuple(_)
        | TyKind::Primitive(_)
        | TyKind::Slice(_)
        | TyKind::Nominal(_)
        | TyKind::Capability(_)
        | TyKind::FnPtr(_)
        | TyKind::Error => None,
    };
    match elem_tys {
        Some(tys) => {
            for (elem, t) in elems.iter_mut().zip(tys.iter()) {
                bind_pat(env, elem, *t, ic);
            }
        }
        None => {
            let display = ic.display_ty(scrut_ty).to_string();
            let arity = elems.len();
            ic.emit_typecheck_error(
                span,
                format!(
                    "tuple pattern of arity {arity} does not match scrutinee of type `{display}`",
                ),
            );
            for elem in elems.iter_mut() {
                bind_pat(env, elem, error, ic);
            }
        }
    }
}

fn bind_variant_pat(
    env: &mut TyEnv,
    path: &HirPath,
    payload: &mut HirVariantPatPayload,
    scrut_ty: TyId,
    ic: &mut InferCx<'_>,
    span: Span,
) {
    let error = ic.ty_error();
    if scrut_ty == error {
        bind_payload_with_error(env, payload, error, ic);
        return;
    }
    let sum_id = match ic.lower.ty_interner.kind(scrut_ty) {
        TyKind::Nominal(id) => *id,
        _ => {
            let display = ic.display_ty(scrut_ty).to_string();
            ic.emit_typecheck_error(
                span,
                format!("variant pattern requires a sum type; scrutinee has type `{display}`"),
            );
            bind_payload_with_error(env, payload, error, ic);
            return;
        }
    };

    // The variant name is the last segment of the pattern's path
    // (e.g. `Color.red` → variant `red` on `Color`).
    let Some(last_seg) = path.segments.last() else {
        bind_payload_with_error(env, payload, error, ic);
        return;
    };
    let variant_name = last_seg.name;

    // Clone the matched variant's payload info to drop the ty_cx borrow.
    let payload_info: Option<VariantPayloadInfo> = ic
        .ty_cx
        .type_decl(sum_id)
        .and_then(|info| info.variant(variant_name).map(|v| v.payload.clone()));

    let Some(payload_info) = payload_info else {
        let scrut_display = ic.display_ty(scrut_ty).to_string();
        let variant_text = ic.lower.interner.resolve(variant_name).to_string();
        ic.emit_typecheck_error(
            span,
            format!(
                "no variant `{variant_text}` on sum type `{scrut_display}`",
            ),
        );
        bind_payload_with_error(env, payload, error, ic);
        return;
    };

    match (payload, payload_info) {
        (HirVariantPatPayload::None, VariantPayloadInfo::Unit) => {}
        (HirVariantPatPayload::Tuple(pats), VariantPayloadInfo::Tuple { elems }) => {
            if pats.len() != elems.len() {
                let variant_text = ic.lower.interner.resolve(variant_name).to_string();
                let declared = elems.len();
                let supplied = pats.len();
                ic.emit_typecheck_error(
                    span,
                    format!(
                        "variant `{variant_text}` has {declared} positional field(s) \
                         but pattern supplied {supplied}",
                    ),
                );
                for pat in pats.iter_mut() {
                    bind_pat(env, pat, error, ic);
                }
            } else {
                for (pat, ty) in pats.iter_mut().zip(elems.iter()) {
                    bind_pat(env, pat, *ty, ic);
                }
            }
        }
        // Positional patterns against a named-payload variant bind the
        // i-th sub-pattern to the i-th declared field in source order.
        // The stdlib (`Option.some(value: T)`, the mini-lisp `Expr`
        // variants) declares variant payloads as named for documentation
        // and uses positional patterns at every match site — `case
        // .some(let v)`, `case .binop(let op, let lhs, let rhs)` —
        // because the field names rarely add information at the use
        // site. Arity must match; field names are discarded.
        (HirVariantPatPayload::Tuple(pats), VariantPayloadInfo::Struct { fields }) => {
            if pats.len() != fields.len() {
                let variant_text = ic.lower.interner.resolve(variant_name).to_string();
                let declared = fields.len();
                let supplied = pats.len();
                ic.emit_typecheck_error(
                    span,
                    format!(
                        "variant `{variant_text}` has {declared} field(s) \
                         but positional pattern supplied {supplied}",
                    ),
                );
                for pat in pats.iter_mut() {
                    bind_pat(env, pat, error, ic);
                }
            } else {
                for (pat, field) in pats.iter_mut().zip(fields.iter()) {
                    bind_pat(env, pat, field.ty, ic);
                }
            }
        }
        (HirVariantPatPayload::Struct(field_pats), VariantPayloadInfo::Struct { fields }) => {
            let field_tys: Vec<(edda_intern::Symbol, TyId)> =
                fields.iter().map(|f| (f.name, f.ty)).collect();
            bind_struct_fields(env, field_pats, &field_tys, false, ic, span, variant_name);
        }
        (pat_payload, _info_mismatch) => {
            let variant_text = ic.lower.interner.resolve(variant_name).to_string();
            ic.emit_typecheck_error(
                span,
                format!("variant `{variant_text}` payload shape does not match the pattern"),
            );
            bind_payload_with_error(env, pat_payload, error, ic);
        }
    }
}

fn bind_struct_pat(
    env: &mut TyEnv,
    path: &HirPath,
    fields: &mut [HirStructPatField],
    rest: bool,
    scrut_ty: TyId,
    ic: &mut InferCx<'_>,
    span: Span,
) {
    let error = ic.ty_error();
    if scrut_ty == error {
        for f in fields.iter_mut() {
            bind_pat(env, &mut f.pat, error, ic);
        }
        return;
    }
    let prod_id = match ic.lower.ty_interner.kind(scrut_ty) {
        TyKind::Nominal(id) => *id,
        _ => {
            let display = ic.display_ty(scrut_ty).to_string();
            ic.emit_typecheck_error(
                span,
                format!("struct pattern requires a product type; scrutinee has type `{display}`"),
            );
            for f in fields.iter_mut() {
                bind_pat(env, &mut f.pat, error, ic);
            }
            return;
        }
    };

    // Pull declared fields out of the ty_cx borrow.
    let declared = ic.ty_cx.type_decl(prod_id).map(|info| match &info.kind {
        TypeDeclShape::Product { fields } => fields
            .iter()
            .map(|f| (f.name, f.ty))
            .collect::<Vec<(edda_intern::Symbol, TyId)>>(),
        TypeDeclShape::Sum { .. } => Vec::new(),
    });
    let Some(declared) = declared else {
        let display = ic.display_ty(scrut_ty).to_string();
        ic.emit_typecheck_error(
            span,
            format!("no layout recorded for type `{display}`"),
        );
        for f in fields.iter_mut() {
            bind_pat(env, &mut f.pat, error, ic);
        }
        return;
    };
    if declared.is_empty() {
        let display = ic.display_ty(scrut_ty).to_string();
        ic.emit_typecheck_error(
            span,
            format!(
                "struct pattern requires a product type; `{display}` is a sum type \
                 (use a variant pattern instead)",
            ),
        );
        for f in fields.iter_mut() {
            bind_pat(env, &mut f.pat, error, ic);
        }
        return;
    }

    // Resolve the type name once for diagnostic strings.
    let type_text = path
        .segments
        .last()
        .map(|s| ic.lower.interner.resolve(s.name).to_string())
        .unwrap_or_else(|| ic.display_ty(scrut_ty).to_string());

    bind_struct_fields(
        env,
        fields,
        &declared,
        rest,
        ic,
        span,
        ic.lower.interner.intern(&type_text),
    );
}

/// Shared field-binding helper used by both `Variant.Struct` and
/// plain `Struct` patterns. Walks `field_pats` in source order,
/// looks each name up in `declared`, and binds the sub-pattern at
/// the corresponding type. Unknown / duplicate fields and (when
/// `rest == false`) missing required fields diagnose.
fn bind_struct_fields(
    env: &mut TyEnv,
    field_pats: &mut [HirStructPatField],
    declared: &[(edda_intern::Symbol, TyId)],
    rest: bool,
    ic: &mut InferCx<'_>,
    span: Span,
    type_name: edda_intern::Symbol,
) {
    let error = ic.ty_error();
    let mut seen = vec![false; declared.len()];
    for f in field_pats.iter_mut() {
        let fname = f.name.name;
        match declared.iter().position(|(n, _)| *n == fname) {
            Some(idx) => {
                let declared_ty = declared[idx].1;
                bind_pat(env, &mut f.pat, declared_ty, ic);
                if seen[idx] {
                    let fname_text = ic.lower.interner.resolve(fname).to_string();
                    ic.emit_typecheck_error(
                        f.span,
                        format!("duplicate field pattern `{fname_text}`"),
                    );
                } else {
                    seen[idx] = true;
                }
            }
            None => {
                let tname = ic.lower.interner.resolve(type_name).to_string();
                let fname_text = ic.lower.interner.resolve(fname).to_string();
                bind_pat(env, &mut f.pat, error, ic);
                ic.emit_typecheck_error(
                    f.span,
                    format!("type `{tname}` has no field `{fname_text}`"),
                );
            }
        }
    }
    if !rest {
        let missing: Vec<edda_intern::Symbol> = declared
            .iter()
            .zip(seen.iter())
            .filter_map(|((n, _), p)| if *p { None } else { Some(*n) })
            .collect();
        if !missing.is_empty() {
            let tname = ic.lower.interner.resolve(type_name).to_string();
            let missing_names: Vec<String> = missing
                .iter()
                .map(|s| format!("`{}`", ic.lower.interner.resolve(*s)))
                .collect();
            ic.emit_typecheck_error(
                span,
                format!(
                    "struct pattern for `{tname}` is missing field(s) {} \
                     (use `..` to ignore omitted fields)",
                    missing_names.join(", "),
                ),
            );
        }
    }
}

/// Fall-through helper: when a variant pattern cannot be matched to
/// declared payload (mismatch, missing, or non-nominal scrutinee),
/// stamp every sub-pattern's `ty` with the Error sentinel so the
/// next agent sees the gap.
fn bind_payload_with_error(
    env: &mut TyEnv,
    payload: &mut HirVariantPatPayload,
    error: TyId,
    ic: &mut InferCx<'_>,
) {
    match payload {
        HirVariantPatPayload::None => {}
        HirVariantPatPayload::Tuple(pats) => {
            for pat in pats.iter_mut() {
                bind_pat(env, pat, error, ic);
            }
        }
        HirVariantPatPayload::Struct(fields) => {
            for f in fields.iter_mut() {
                bind_pat(env, &mut f.pat, error, ic);
            }
        }
    }
}
