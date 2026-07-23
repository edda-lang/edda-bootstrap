//! Bare-Path → unit-variant constructor lowering.
//!
//! When a `HirExprKind::Path` resolves to a `BindingId` whose ADT is a sum
//! with a zero-payload variant, the path is a constructor reference and
//! lowers to a `Rvalue::MakeVariant` with an empty fields list. Payload
//! variants reach codegen through `super::call::lower_call`'s
//! variant-constructor branch instead — this module rejects them with a
//! diagnostic so the structural-validity of MIR is preserved.

use edda_types::HirExpr;

use crate::adt::AdtKind;
use crate::error::{LoweringError, MirError};
use crate::ids::{AdtId, VariantIdx};
use crate::operand::Operand;
use crate::place::Place;
use crate::rvalue::{Rvalue, RvalueKind};
use crate::ty::{MirType, MirTypeKind};

use super::cfg::push_assign;
use super::ctx::LoweringContext;

/// Lower a multi-segment value-position Path that names a sum-type
/// unit-variant constructor. Resolution path: span -> Resolved::Binding ->
/// TypeDecl -> AdtKind::Sum -> variant by name -> empty payload ->
/// `Rvalue::MakeVariant`.
pub(super) fn lower_unit_variant_constructor(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    path: &edda_types::HirPath,
) -> Operand {
    use edda_resolve::Resolved;
    let Some(resolved) = ctx.resolutions.lookup_path(path.span) else {
        ctx.errors.push(MirError::from(LoweringError::MultiSegmentPath {
            span: expr.span,
        }));
        return Operand::Unit;
    };
    let binding_id = match resolved {
        Resolved::Binding(id) => id,
        Resolved::Module(_) => {
            ctx.errors.push(MirError::from(LoweringError::UnsupportedHirVariant {
                variant: "Path (module in value position)",
                span: expr.span,
            }));
            return Operand::Unit;
        }
        Resolved::Error => return Operand::Unit,
    };
    let Some(adt_id) = ctx.adt_map.get(&binding_id).copied() else {
        ctx.errors.push(MirError::from(LoweringError::MultiSegmentPath {
            span: expr.span,
        }));
        return Operand::Unit;
    };
    let variant_name = path.segments.last().expect("HirPath is non-empty").name;
    let (variant_idx, payload_arity) = {
        let adt = &ctx.program.program().adts[adt_id];
        if !matches!(adt.kind, AdtKind::Sum) {
            ctx.errors.push(MirError::from(LoweringError::UnsupportedHirVariant {
                variant: "Path (non-sum nominal in value position)",
                span: expr.span,
            }));
            return Operand::Unit;
        }
        match adt.variants.iter().enumerate().find(|(_, v)| v.name == variant_name) {
            Some((idx, v)) => (VariantIdx::from_raw(idx as u32), v.fields.len()),
            None => {
                ctx.errors.push(MirError::from(LoweringError::MultiSegmentPath {
                    span: expr.span,
                }));
                return Operand::Unit;
            }
        }
    };
    if payload_arity != 0 {
        // Payload-variant constructors require call-site arguments; the
        // typechecker should route these through Call instead of bare
        // Path. Surface the issue rather than silently emitting a
        // structurally-invalid `MakeVariant`.
        ctx.errors.push(MirError::from(LoweringError::UnsupportedHirVariant {
            variant: "Path (payload variant without arguments)",
            span: expr.span,
        }));
        return Operand::Unit;
    }
    emit_unit_variant(ctx, expr, adt_id, variant_idx)
}

/// Emit `dest = MakeVariant(adt, variant, [])` into a fresh temp typed as
/// the nominal sum and return `Move(dest)`.
fn emit_unit_variant(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    adt_id: AdtId,
    variant_idx: VariantIdx,
) -> Operand {
    let nominal_ty = MirType::new(MirTypeKind::Adt(adt_id));
    let Some(body_builder) = ctx.body.as_mut() else {
        return Operand::Unit;
    };
    let dest_local = body_builder.temp(nominal_ty.clone(), expr.span);
    push_assign(
        ctx,
        expr.span,
        dest_local,
        Rvalue {
            span: expr.span,
            kind: RvalueKind::MakeVariant {
                adt: adt_id,
                variant: variant_idx,
                fields: Vec::new(),
            },
            ty: nominal_ty,
        },
    );
    Operand::Move(Place::local(dest_local))
}
