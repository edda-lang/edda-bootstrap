//! Statement and rvalue walks.

use crate::adt::AdtKind;
use crate::body::Body;
use crate::error::ValidationError;
use crate::ids::{AdtId, BlockId, BodyId, VariantIdx};
use crate::operand::Operand;
use crate::program::MirProgram;
use crate::rvalue::{Rvalue, RvalueKind};
use crate::statement::{Statement, StatementKind};

use super::locals;

/// Walk a statement: check locals it touches and any nested rvalue/operands.
pub(super) fn check_statement(
    program: &MirProgram,
    body_id: BodyId,
    body: &Body,
    block_id: BlockId,
    stmt_index: usize,
    stmt: &Statement,
    out: &mut Vec<ValidationError>,
) {
    match &stmt.kind {
        StatementKind::Assign { place, rvalue } => {
            locals::check_place_locals_stmt(body_id, body, block_id, stmt_index, place, out);
            check_rvalue(program, body_id, body, block_id, stmt_index, rvalue, out);
        }
        StatementKind::StorageLive(l)
        | StatementKind::StorageDead(l)
        | StatementKind::SetInit(l)
        | StatementKind::Drop(l) => {
            locals::check_local_stmt(body_id, body, block_id, stmt_index, *l, out);
        }
        StatementKind::Nop => {}
    }
}

/// Validate every operand in an rvalue plus its ADT-shape invariants.
fn check_rvalue(
    program: &MirProgram,
    body_id: BodyId,
    body: &Body,
    block_id: BlockId,
    stmt_index: usize,
    rvalue: &Rvalue,
    out: &mut Vec<ValidationError>,
) {
    for op in operands_of_rvalue(&rvalue.kind) {
        locals::check_operand_stmt(body_id, body, block_id, stmt_index, op, out);
    }
    check_rvalue_adt_shape(program, body_id, block_id, &rvalue.kind, out);
}

/// Yield every operand referenced by an rvalue. Mirrors the
/// [`RvalueKind`] variant layout — kept structural so a new variant
/// added in the future triggers a non-exhaustive-match compile error.
fn operands_of_rvalue(kind: &RvalueKind) -> Vec<&Operand> {
    match kind {
        RvalueKind::Use(op) => vec![op],
        RvalueKind::BinOp { lhs, rhs, .. } => vec![lhs, rhs],
        RvalueKind::UnOp { arg, .. } => vec![arg],
        RvalueKind::Cast { src, .. } => vec![src],
        RvalueKind::MakeArray { elems } | RvalueKind::MakeTuple { elems } => {
            elems.iter().collect()
        }
        RvalueKind::MakeRecord { fields, .. } | RvalueKind::MakeVariant { fields, .. } => {
            fields.iter().collect()
        }
        RvalueKind::ArrayIndex { array, idx } => vec![array, idx],
        RvalueKind::SliceSubrange { source, lo, hi } => vec![source, lo, hi],
        RvalueKind::ArrayLen { array } => vec![array],
        RvalueKind::ExtractField { subject, .. } => vec![subject],
        RvalueKind::ExtractTag { subject } => vec![subject],
        RvalueKind::StringBytes(op) => vec![op],
        // Fn-ptr addressing is a leaf — no operand inputs to walk.
        RvalueKind::FunctionRef(_) => Vec::new(),
        // The env word is the only operand input; `code` is a BodyId, not a local.
        RvalueKind::MakeClosure { env, .. } => vec![env],
        // Address-of is a leaf — its input is a `Place`, not an `Operand`.
        RvalueKind::Ref { .. } => Vec::new(),
    }
}

/// Check ADT-shape invariants on `MakeRecord` / `MakeVariant`. `ExtractField`
/// shape is deferred until the operand carries enough type info to resolve
/// the target variant.
fn check_rvalue_adt_shape(
    program: &MirProgram,
    body_id: BodyId,
    block_id: BlockId,
    kind: &RvalueKind,
    out: &mut Vec<ValidationError>,
) {
    match kind {
        RvalueKind::MakeRecord { adt, fields } => {
            check_make_record(program, body_id, block_id, *adt, fields.len(), out);
        }
        RvalueKind::MakeVariant {
            adt,
            variant,
            fields,
        } => {
            check_make_variant(program, body_id, block_id, *adt, *variant, fields.len(), out);
        }
        _ => {}
    }
}

/// MakeRecord must target a Product ADT and supply exactly the variant-0
/// field count.
fn check_make_record(
    program: &MirProgram,
    body_id: BodyId,
    block_id: BlockId,
    adt_id: AdtId,
    field_count: usize,
    out: &mut Vec<ValidationError>,
) {
    let Some(adt) = program.adts.get(adt_id) else {
        return;
    };
    if adt.kind != AdtKind::Product {
        out.push(ValidationError::MakeRecordOnSum {
            body: body_id,
            block: block_id,
            adt: adt_id,
        });
        return;
    }
    if let Some(variant) = adt.variants.first()
        && variant.fields.len() != field_count
    {
        out.push(ValidationError::FieldCountMismatch {
            body: body_id,
            block: block_id,
            adt: adt_id,
            variant: None,
            expected: variant.fields.len(),
            found: field_count,
        });
    }
}

/// MakeVariant must target a Sum ADT, the variant index must be in range,
/// and the operand count must match the variant's field count.
fn check_make_variant(
    program: &MirProgram,
    body_id: BodyId,
    block_id: BlockId,
    adt_id: AdtId,
    variant: VariantIdx,
    field_count: usize,
    out: &mut Vec<ValidationError>,
) {
    let Some(adt) = program.adts.get(adt_id) else {
        return;
    };
    if adt.kind == AdtKind::Product {
        out.push(ValidationError::MakeVariantOnProduct {
            body: body_id,
            block: block_id,
            adt: adt_id,
        });
        return;
    }
    let Some(variant_def) = adt.variants.get(variant.as_index()) else {
        out.push(ValidationError::SwitchTagVariantDangling {
            body: body_id,
            block: block_id,
            adt: adt_id,
            variant,
        });
        return;
    };
    if variant_def.fields.len() != field_count {
        out.push(ValidationError::FieldCountMismatch {
            body: body_id,
            block: block_id,
            adt: adt_id,
            variant: Some(variant),
            expected: variant_def.fields.len(),
            found: field_count,
        });
    }
}
