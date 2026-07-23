//! Variant-constructor call lowering.
//!
//! A `Call` whose callee resolves to a sum-type's variant name is a
//! constructor — not a function call. The call site lowers to a
//! `Rvalue::MakeVariant` populated from the call's argument list, with
//! named (struct-payload) args reordered to declaration order. See
//! [`super::variant_ctor`] for the bare-Path (no-argument) counterpart.

use edda_intern::Symbol;
use edda_types::{HirCallArg, HirExpr, HirExprKind};

use crate::adt::AdtKind;
use crate::arena::Idx;
use crate::error::{LoweringError, MirError};
use crate::ids::{AdtId, FieldIdx, VariantIdx};
use crate::operand::Operand;
use crate::place::Place;
use crate::rvalue::{Rvalue, RvalueKind};
use crate::ty::{MirType, MirTypeKind};

use super::cfg::push_assign;
use super::ctx::LoweringContext;
use super::expr::lower_expr_to_operand;
use super::ty::lower_ty;

/// Detect a sum-type variant-constructor call site. Returns the target ADT
/// + variant index when the callee path resolves to a `BindingKind::TypeDecl`
/// whose registered MIR ADT is a [`AdtKind::Sum`] containing a variant with
/// the path's last-segment name.
pub(super) fn resolve_variant_constructor(
    ctx: &LoweringContext<'_>,
    callee: &HirExpr,
) -> Option<(AdtId, VariantIdx)> {
    use edda_resolve::Resolved;
    let HirExprKind::Path(path) = &callee.kind else {
        return None;
    };
    if path.segments.len() < 2 {
        return None;
    }
    let resolved = ctx.resolutions.lookup_path(path.span)?;
    let binding_id = match resolved {
        Resolved::Binding(id) => id,
        _ => return None,
    };
    let adt_id = ctx.adt_map.get(&binding_id).copied()?;
    let adt = &ctx.program.program().adts[adt_id];
    if !matches!(adt.kind, AdtKind::Sum) {
        return None;
    }
    let variant_name = path.segments.last().expect("HirPath is non-empty").name;
    let variant_idx = adt
        .variants
        .iter()
        .position(|v| v.name == variant_name)
        .map(|i| VariantIdx::from_raw(i as u32))?;
    Some((adt_id, variant_idx))
}

/// Lower a `<Type>.<variant>(args)` call site to `RvalueKind::MakeVariant`.
/// Args are evaluated in source order; named arguments (struct-payload
/// variants) are reordered to match the variant's declaration-order field
/// list so the emitted operands stay aligned with the ADT's layout.
pub(super) fn lower_variant_constructor_call(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    adt_id: AdtId,
    variant_idx: VariantIdx,
    args: &[HirCallArg],
) -> Operand {
    let (field_names, field_tys): (Vec<Symbol>, Vec<MirType>) = {
        let adt = &ctx.program.program().adts[adt_id];
        let variant = &adt.variants[variant_idx.index()];
        (
            variant.fields.iter().map(|f| f.name).collect(),
            variant.fields.iter().map(|f| f.ty.clone()).collect(),
        )
    };
    let arity = field_names.len();

    // Composite-payload form (D-22): a single
    // positional argument whose MIR type is the payload composite tuple
    // `(T1, …, Tn)` carries the whole payload as one value — destructure it
    // into per-field operands so the emitted `MakeVariant` stays
    // layout-identical to the unpacked writes. The typechecker
    // (`synth_variant_constructor`) already accepts this form; it is told
    // apart from the positional-unpacked form by the exact tuple type (for a
    // single-field variant, `(T)` vs. bare `T`).
    if arity >= 1 && args.len() == 1 && args[0].name.is_none() {
        let arg_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, args[0].expr.ty);
        let composite_ty = MirType::new(MirTypeKind::Tuple(field_tys.clone()));
        if arg_ty == composite_ty {
            return lower_composite_variant_ctor(
                ctx,
                expr,
                adt_id,
                variant_idx,
                &args[0],
                &field_tys,
            );
        }
    }

    if args.len() != arity {
        for arg in args.iter() {
            let _ = lower_expr_to_operand(ctx, &arg.expr);
        }
        push_internal_error(
            ctx,
            expr.span,
            "variant-constructor arity mismatch (typechecker should have rejected)",
        );
        return Operand::Unit;
    }

    // Build the operand list in declaration order. For struct-payload
    // variants, callers may supply args in any order with `name: expr`;
    // for tuple-payload (and unit) variants there are no names.
    let mut field_ops: Vec<Option<Operand>> = (0..arity).map(|_| None).collect();
    let any_named = args.iter().any(|a| a.name.is_some());
    if any_named && arity > 0 {
        for arg in args.iter() {
            let op = lower_expr_to_operand(ctx, &arg.expr);
            let Some(name_ident) = &arg.name else {
                push_internal_error(
                    ctx,
                    arg.span,
                    "mixed named/positional args at variant constructor (typechecker should have rejected)",
                );
                continue;
            };
            let Some(idx) = field_names.iter().position(|n| *n == name_ident.name) else {
                push_internal_error(
                    ctx,
                    arg.span,
                    "unknown field at variant constructor (typechecker should have rejected)",
                );
                continue;
            };
            field_ops[idx] = Some(op);
        }
    } else {
        for (i, arg) in args.iter().enumerate() {
            let op = lower_expr_to_operand(ctx, &arg.expr);
            field_ops[i] = Some(op);
        }
    }

    let nominal_ty = MirType::new(MirTypeKind::Adt(adt_id));
    let Some(body_builder) = ctx.body.as_mut() else {
        return Operand::Unit;
    };
    let dest_local = body_builder.temp(nominal_ty.clone(), expr.span);
    let mut fields: Vec<Operand> = Vec::with_capacity(arity);
    for (i, slot) in field_ops.into_iter().enumerate() {
        match slot {
            Some(op) => fields.push(op),
            None => {
                push_internal_error(
                    ctx,
                    expr.span,
                    "missing field at variant constructor (typechecker should have rejected)",
                );
                let _ = i;
                fields.push(Operand::Unit);
            }
        }
    }
    push_assign(
        ctx,
        expr.span,
        dest_local,
        Rvalue {
            span: expr.span,
            kind: RvalueKind::MakeVariant {
                adt: adt_id,
                variant: variant_idx,
                fields,
            },
            ty: nominal_ty,
        },
    );
    Operand::Move(Place::local(dest_local))
}

/// Lower the composite-payload constructor form `<Type>.<variant>(payload)`
/// where `payload` is the single payload composite tuple value.
/// Materialises the composite in a temp,
/// extracts each element into a per-field operand (a tuple read —
/// `ExtractField { variant: None, .. }`, layout-identical to the payload
/// fields), and assembles them into `RvalueKind::MakeVariant`.
fn lower_composite_variant_ctor(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    adt_id: AdtId,
    variant_idx: VariantIdx,
    arg: &HirCallArg,
    field_tys: &[MirType],
) -> Operand {
    let composite_op = lower_expr_to_operand(ctx, &arg.expr);
    let composite_ty = MirType::new(MirTypeKind::Tuple(field_tys.to_vec()));
    // Materialise the composite so each element read copies it rather than
    // consuming the operand.
    let subject_local = match ctx.body.as_mut() {
        Some(body) => body.temp(composite_ty.clone(), expr.span),
        None => return Operand::Unit,
    };
    push_assign(
        ctx,
        expr.span,
        subject_local,
        Rvalue {
            span: expr.span,
            kind: RvalueKind::Use(composite_op),
            ty: composite_ty,
        },
    );

    let mut fields: Vec<Operand> = Vec::with_capacity(field_tys.len());
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
                    variant: None,
                    field: FieldIdx::from_raw(k as u32),
                },
                ty: field_ty.clone(),
            },
        );
        fields.push(Operand::Move(Place::local(temp)));
    }

    let nominal_ty = MirType::new(MirTypeKind::Adt(adt_id));
    let dest_local = match ctx.body.as_mut() {
        Some(body) => body.temp(nominal_ty.clone(), expr.span),
        None => return Operand::Unit,
    };
    push_assign(
        ctx,
        expr.span,
        dest_local,
        Rvalue {
            span: expr.span,
            kind: RvalueKind::MakeVariant {
                adt: adt_id,
                variant: variant_idx,
                fields,
            },
            ty: nominal_ty,
        },
    );
    Operand::Move(Place::local(dest_local))
}

/// Push `InternalError { message, span }`.
fn push_internal_error(ctx: &mut LoweringContext<'_>, span: edda_span::Span, message: &str) {
    ctx.errors.push(MirError::from(LoweringError::InternalError {
        message: message.to_string(),
        span,
    }));
}
