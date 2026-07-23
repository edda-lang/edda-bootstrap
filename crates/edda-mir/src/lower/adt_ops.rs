//! ADT-aware rvalue lowering for the typed-HIR -> MIR pass.
//!
//! Lowering for two HIR shapes, both dependent on the
//! program-wide ADT registry built by the ADT pre-pass:
//!
//! - [`lower_struct_lit`] handles `HirExprKind::StructLit { path, fields }`.
//!   The expression's own `TyId` (already inferred by `edda-types` to
//!   `TyKind::Nominal(binding)`) is the resolution surface — looking the
//!   binding up in `ctx.adt_map` yields the [`AdtId`]. Product types emit
//!   [`RvalueKind::MakeRecord`]; sum types route to a diagnostic because
//!   variant-constructor desugaring is not yet performed by the AST -> HIR
//!   layer.
//! - [`lower_field`] handles `HirExprKind::Field { receiver, name }`.
//!   The receiver's `TyId` carries the nominal type; the field's
//!   [`FieldIdx`] is resolved by walking the product variant's
//!   declaration-order field list. Sum-typed receivers surface a
//!   diagnostic — projecting on a sum requires a downcast that the
//!   `Field` HIR variant does not encode.
//!
//! Both lowerings emit field operands in the ADT's declared field order
//! so the compile-side walker (`crates/edda-compile/src/lower/rvalue.rs`)
//! can index the LLVM struct with the matching [`FieldIdx`] directly.

use edda_intern::Symbol;
use edda_span::Span;
use edda_syntax::ast::Ident;
use edda_types::{HirExpr, HirStructLitField, TyId, TyKind};

use crate::adt::AdtKind;
use crate::error::{LoweringError, MirError};
use crate::ids::{AdtId, FieldIdx, VariantIdx};
use crate::operand::Operand;
use crate::place::Place;
use crate::rvalue::{Rvalue, RvalueKind};
use crate::ty::MirType;

use super::ctx::LoweringContext;
use super::expr::lower_expr_to_operand;
use super::cfg::push_assign;
use super::ty::lower_ty;

/// Lower a `Path { f1: e1, ... }` struct-literal expression.
///
/// Resolves the target ADT through `expr.ty -> TyKind::Nominal(binding)
/// -> ctx.adt_map[binding] -> AdtId`. Each declared field is matched by
/// name against the HIR initialiser list (the order in `fields` is
/// source order, which need not match declaration order); the operands
/// are then collected in declaration order. Missing fields surface
/// `InternalError` — `edda-types` already enforces field completeness,
/// so a hole at this layer signals an upstream defect.
pub(super) fn lower_struct_lit(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    fields: &[HirStructLitField],
) -> Operand {
    let Some((adt_id, adt_kind)) = resolve_nominal_adt(ctx, expr.ty, expr.span) else {
        // Diagnostic was pushed (or `Error`-typed expr — cascade-suppressed).
        return drain_field_initialisers(ctx, fields);
    };
    if adt_kind != AdtKind::Product {
        ctx.errors.push(MirError::from(LoweringError::UnsupportedHirVariant {
            variant: "StructLit on sum type (variant constructor)",
            span: expr.span,
        }));
        return drain_field_initialisers(ctx, fields);
    }
    let field_operands = collect_record_operands(ctx, adt_id, fields, expr.span);
    let result_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, expr.ty);
    let temp = match ctx.body.as_mut() {
        Some(body) => body.temp(result_ty.clone(), expr.span),
        None => return Operand::Unit,
    };
    let rvalue = Rvalue {
        span: expr.span,
        kind: RvalueKind::MakeRecord {
            adt: adt_id,
            fields: field_operands,
        },
        ty: result_ty,
    };
    push_assign(ctx, expr.span, temp, rvalue);
    Operand::Move(Place::local(temp))
}

/// Lower a `receiver.name` field-access expression.
///
/// Resolves the receiver's ADT through `receiver.ty -> Nominal -> adt_map`,
/// walks the product variant's `FieldDef` list to find `name`, and emits
/// `ExtractField`. Field-name lookup failures surface `InternalError`
/// because `edda-types` already proves the field exists before this
/// layer runs.
pub(super) fn lower_field(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    receiver: &HirExpr,
    name: Ident,
) -> Operand {
    // Lower the receiver first regardless of subsequent resolution — its
    // side-effects (loads, drops, sub-diagnostics) must be observed even
    // when we later fall back to a diagnostic-only return.
    let subject = lower_expr_to_operand(ctx, receiver);
    let Some((adt_id, adt_kind)) = resolve_nominal_adt(ctx, receiver.ty, receiver.span) else {
        return Operand::Unit;
    };
    if adt_kind != AdtKind::Product {
        ctx.errors.push(MirError::from(LoweringError::UnsupportedHirVariant {
            variant: "Field on sum type",
            span: expr.span,
        }));
        return Operand::Unit;
    }
    let Some((field_idx, field_ty)) = resolve_product_field(ctx, adt_id, name.name, expr.span)
    else {
        return Operand::Unit;
    };
    let temp = match ctx.body.as_mut() {
        Some(body) => body.temp(field_ty.clone(), expr.span),
        None => return Operand::Unit,
    };
    let rvalue = Rvalue {
        span: expr.span,
        kind: RvalueKind::ExtractField {
            subject,
            variant: None,
            field: field_idx,
        },
        ty: field_ty,
    };
    push_assign(ctx, expr.span, temp, rvalue);
    Operand::Move(Place::local(temp))
}

/// Lower a `receiver.N` positional-field access expression.
///
/// A tuple receiver resolves element `N` through `receiver.ty ->
/// MirTypeKind::Tuple` and emits `RvalueKind::ExtractField` with
/// `variant: None`. A sum-typed receiver is the variant-payload READ:
/// `N` names the variant in declaration order and the access projects
/// the active variant's payload region as its payload *composite* —
/// `()` for a payload-less variant (`Operand::Unit`), else a `MakeTuple`
/// of the per-field `ExtractField { variant: Some(N), field: k }` reads,
/// layout-identical to the declared payload fields and matching
/// `synth_tuple_index`'s payload-composite type (mirroring the native's
/// `variant_payload` place projection). Out-of-range indices and other
/// receivers surface `InternalError`.
pub(super) fn lower_tuple_index(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    receiver: &HirExpr,
    index: u32,
) -> Operand {
    let subject = lower_expr_to_operand(ctx, receiver);
    let recv_mir_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, receiver.ty);
    match &recv_mir_ty.kind {
        crate::ty::MirTypeKind::Tuple(elems) => {
            let Some(elem_ty) = elems.get(index as usize).cloned() else {
                ctx.errors.push(MirError::from(LoweringError::InternalError {
                    message: format!("tuple-index .{index} out of range at MIR-lowering"),
                    span: expr.span,
                }));
                return Operand::Unit;
            };
            let temp = match ctx.body.as_mut() {
                Some(body) => body.temp(elem_ty.clone(), expr.span),
                None => return Operand::Unit,
            };
            push_assign(
                ctx,
                expr.span,
                temp,
                Rvalue {
                    span: expr.span,
                    kind: RvalueKind::ExtractField {
                        subject,
                        variant: None,
                        field: FieldIdx::from_raw(index),
                    },
                    ty: elem_ty,
                },
            );
            Operand::Move(Place::local(temp))
        }
        crate::ty::MirTypeKind::Adt(adt_id) => {
            let adt_id = *adt_id;
            let is_sum = ctx
                .program
                .program()
                .adts
                .get(adt_id)
                .is_some_and(|adt| adt.kind == AdtKind::Sum);
            if !is_sum {
                ctx.errors.push(MirError::from(LoweringError::InternalError {
                    message: "positional index on a product receiver at MIR-lowering"
                        .to_string(),
                    span: expr.span,
                }));
                return Operand::Unit;
            }
            lower_variant_payload_composite(ctx, expr, subject, recv_mir_ty.clone(), adt_id, index)
        }
        _ => {
            // Receiver was already error-typed (cascade-suppress) or the
            // typechecker failed to prove tuple shape; stay silent on the
            // error sentinel path, diagnose otherwise.
            if receiver.ty == ctx.ty_interner.error() {
                return Operand::Unit;
            }
            ctx.errors.push(MirError::from(LoweringError::InternalError {
                message: "tuple-index on non-tuple receiver at MIR-lowering".to_string(),
                span: expr.span,
            }));
            Operand::Unit
        }
    }
}

/// Project a sum receiver's variant-`index` payload region as its
/// payload *composite* value (D-22).
///
/// The sum subject is materialised in a local so each field read copies
/// it (a bare `Move` would be consumed by the first `ExtractField`);
/// each payload field `k` is read with `ExtractField { variant:
/// Some(index), field: k }` and the reads are assembled into a
/// `MakeTuple` typed as the composite (`expr.ty` — `(T)` for a single
/// payload, `(T1, …, Tn)` otherwise). A payload-less variant has no
/// fields and reads as the unit value.
fn lower_variant_payload_composite(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    subject: Operand,
    sum_mir_ty: MirType,
    adt_id: AdtId,
    index: u32,
) -> Operand {
    // Snapshot the variant's payload field types before borrowing `ctx`
    // mutably to emit the reads.
    let field_tys: Vec<MirType> = ctx
        .program
        .program()
        .adts
        .get(adt_id)
        .and_then(|adt| adt.variants.get(index as usize))
        .map(|v| v.fields.iter().map(|f| f.ty.clone()).collect())
        .unwrap_or_default();

    // Payload-less variant → the `()` composite.
    if field_tys.is_empty() {
        return Operand::Unit;
    }

    // Materialise the sum subject so each field read copies it.
    let subject_local = match ctx.body.as_mut() {
        Some(body) => body.temp(sum_mir_ty.clone(), expr.span),
        None => return Operand::Unit,
    };
    push_assign(
        ctx,
        expr.span,
        subject_local,
        Rvalue {
            span: expr.span,
            kind: RvalueKind::Use(subject),
            ty: sum_mir_ty,
        },
    );

    let mut elems: Vec<Operand> = Vec::with_capacity(field_tys.len());
    for (k, field_ty) in field_tys.iter().enumerate() {
        let temp = match ctx.body.as_mut() {
            Some(body) => body.temp(field_ty.clone(), expr.span),
            None => return Operand::Unit,
        };
        push_assign(
            ctx,
            expr.span,
            temp,
            Rvalue {
                span: expr.span,
                kind: RvalueKind::ExtractField {
                    subject: Operand::Copy(Place::local(subject_local)),
                    variant: Some(VariantIdx::from_raw(index)),
                    field: FieldIdx::from_raw(k as u32),
                },
                ty: field_ty.clone(),
            },
        );
        elems.push(Operand::Move(Place::local(temp)));
    }

    let composite_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, expr.ty);
    let result = match ctx.body.as_mut() {
        Some(body) => body.temp(composite_ty.clone(), expr.span),
        None => return Operand::Unit,
    };
    push_assign(
        ctx,
        expr.span,
        result,
        Rvalue {
            span: expr.span,
            kind: RvalueKind::MakeTuple { elems },
            ty: composite_ty,
        },
    );
    Operand::Move(Place::local(result))
}

/// Resolve `ty` to a `(AdtId, AdtKind)` pair. Returns `None` and pushes
/// a `MultiSegmentPath`-flavoured diagnostic when the type does not
/// resolve to a registered nominal ADT — the typechecker should have
/// already proved it does, so this branch only fires when the lowering
/// caller passed an incomplete `type_decls` slice.
pub(super) fn resolve_nominal_adt(
    ctx: &mut LoweringContext<'_>,
    ty: TyId,
    span: Span,
) -> Option<(AdtId, AdtKind)> {
    // Error sentinel: a diagnostic was already pushed upstream — stay
    // silent at this layer to avoid cascading.
    if ty == ctx.ty_interner.error() {
        return None;
    }
    let binding = match ctx.ty_interner.kind(ty) {
        TyKind::Nominal(b) => *b,
        _ => {
            ctx.errors.push(MirError::from(LoweringError::InternalError {
                message: "ADT operation on non-nominal type".to_string(),
                span,
            }));
            return None;
        }
    };
    let adt_id = match ctx.adt_map.get(&binding) {
        Some(id) => *id,
        None => {
            ctx.errors.push(MirError::from(LoweringError::InternalError {
                message: "nominal type missing from ADT registry".to_string(),
                span,
            }));
            return None;
        }
    };
    let kind = ctx
        .program
        .program()
        .adts
        .get(adt_id)
        .map(|adt| adt.kind)?;
    Some((adt_id, kind))
}

/// Walk the AdtDef's variant-0 field list in declaration order and
/// collect the matching HIR-side initialiser operand for each. Builds
/// a `name -> usize` map by walking the HIR fields once, then drives the
/// per-declared-field lookup. Missing entries surface `InternalError`
/// and contribute a placeholder unit operand so the rvalue still has
/// the right arity.
fn collect_record_operands(
    ctx: &mut LoweringContext<'_>,
    adt_id: AdtId,
    fields: &[HirStructLitField],
    span: Span,
) -> Vec<Operand> {
    // Snapshot the declaration order out of the program so we can use
    // `ctx` mutably afterwards. The `Symbol` is `Copy`, so this is a
    // small fixed-cost clone.
    let declared: Vec<Symbol> = ctx
        .program
        .program()
        .adts
        .get(adt_id)
        .and_then(|adt| adt.variants.first())
        .map(|variant| variant.fields.iter().map(|f| f.name).collect())
        .unwrap_or_default();

    // Index the HIR initialisers by name. Source-order is not assumed
    // to match declaration order — `edda-types` does not reorder.
    let mut by_name: Vec<(Symbol, usize)> = fields
        .iter()
        .enumerate()
        .map(|(i, f)| (f.name.name, i))
        .collect();

    let mut operands: Vec<Operand> = Vec::with_capacity(declared.len());
    for decl_name in declared.iter().copied() {
        let pos = by_name.iter().position(|(n, _)| *n == decl_name);
        match pos {
            Some(p) => {
                let (_, hir_idx) = by_name.swap_remove(p);
                let op = lower_expr_to_operand(ctx, &fields[hir_idx].value);
                operands.push(op);
            }
            None => {
                ctx.errors.push(MirError::from(LoweringError::InternalError {
                    message: "struct literal missing declared field at MIR-lowering"
                        .to_string(),
                    span,
                }));
                operands.push(Operand::Unit);
            }
        }
    }
    // Any leftover HIR initialisers (unknown fields) get their
    // sub-expressions lowered for side effects only; the typechecker
    // already diagnosed the unknown-field name.
    for (_, leftover_idx) in by_name.into_iter() {
        let _ = lower_expr_to_operand(ctx, &fields[leftover_idx].value);
    }
    operands
}

/// Lower every initialiser in `fields` for its side-effects (call
/// emissions, sub-diagnostics) and return `Operand::Unit`. Used by the
/// error paths in [`lower_struct_lit`] so the operand the caller
/// receives is a placeholder even though no rvalue was emitted.
fn drain_field_initialisers(
    ctx: &mut LoweringContext<'_>,
    fields: &[HirStructLitField],
) -> Operand {
    for f in fields.iter() {
        let _ = lower_expr_to_operand(ctx, &f.value);
    }
    Operand::Unit
}

/// Walk variant 0's `FieldDef` list looking for `name`. Returns
/// `(FieldIdx, field_type)` on success; pushes an `InternalError` and
/// returns `None` otherwise (the typechecker should have proven the
/// field exists).
pub(super) fn resolve_product_field(
    ctx: &mut LoweringContext<'_>,
    adt_id: AdtId,
    name: Symbol,
    span: Span,
) -> Option<(FieldIdx, MirType)> {
    let resolution = ctx
        .program
        .program()
        .adts
        .get(adt_id)
        .and_then(|adt| adt.variants.first())
        .and_then(|variant| {
            variant
                .fields
                .iter()
                .position(|f| f.name == name)
                .map(|idx| (idx, variant.fields[idx].ty.clone()))
        });
    match resolution {
        Some((idx, ty)) => Some((FieldIdx::from_raw(idx as u32), ty)),
        None => {
            ctx.errors.push(MirError::from(LoweringError::InternalError {
                message: "field name not in ADT registry at MIR-lowering".to_string(),
                span,
            }));
            None
        }
    }
}

