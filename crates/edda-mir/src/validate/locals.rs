//! Place / operand / local range helpers shared by the statement and
//! terminator walks. Two variants exist for each (`_stmt` / `_term`) so the
//! pushed [`ValidationError`] variant carries the correct scope tag without
//! the caller plumbing it through.

use crate::body::Body;
use crate::error::ValidationError;
use crate::ids::{BlockId, BodyId, LocalId};
use crate::operand::Operand;
use crate::place::{Place, Projection};

/// Validate a place's root local + index projections at statement scope.
pub(super) fn check_place_locals_stmt(
    body_id: BodyId,
    body: &Body,
    block_id: BlockId,
    stmt_index: usize,
    place: &Place,
    out: &mut Vec<ValidationError>,
) {
    check_local_stmt(body_id, body, block_id, stmt_index, place.local, out);
    for proj in &place.projection {
        if let Projection::Index(l) = proj {
            check_local_stmt(body_id, body, block_id, stmt_index, *l, out);
        }
    }
}

/// Validate a place's root local + index projections at terminator scope.
pub(super) fn check_place_locals_term(
    body_id: BodyId,
    body: &Body,
    block_id: BlockId,
    place: &Place,
    out: &mut Vec<ValidationError>,
) {
    check_local_term(body_id, body, block_id, place.local, out);
    for proj in &place.projection {
        if let Projection::Index(l) = proj {
            check_local_term(body_id, body, block_id, *l, out);
        }
    }
}

/// Validate an operand's local references at statement scope.
pub(super) fn check_operand_stmt(
    body_id: BodyId,
    body: &Body,
    block_id: BlockId,
    stmt_index: usize,
    op: &Operand,
    out: &mut Vec<ValidationError>,
) {
    match op {
        Operand::Copy(p) | Operand::Move(p) => {
            check_place_locals_stmt(body_id, body, block_id, stmt_index, p, out);
        }
        Operand::Const(_) | Operand::Unit => {}
    }
}

/// Validate an operand's local references at terminator scope.
pub(super) fn check_operand_term(
    body_id: BodyId,
    body: &Body,
    block_id: BlockId,
    op: &Operand,
    out: &mut Vec<ValidationError>,
) {
    match op {
        Operand::Copy(p) | Operand::Move(p) => {
            check_place_locals_term(body_id, body, block_id, p, out);
        }
        Operand::Const(_) | Operand::Unit => {}
    }
}

/// Range-check a single `LocalId` at statement scope.
pub(super) fn check_local_stmt(
    body_id: BodyId,
    body: &Body,
    block_id: BlockId,
    stmt_index: usize,
    local: LocalId,
    out: &mut Vec<ValidationError>,
) {
    if local.as_index() >= body.locals.len() {
        out.push(ValidationError::StatementLocalDangling {
            body: body_id,
            block: block_id,
            stmt_index,
            local,
        });
    }
}

/// Range-check a single `LocalId` at terminator scope.
pub(super) fn check_local_term(
    body_id: BodyId,
    body: &Body,
    block_id: BlockId,
    local: LocalId,
    out: &mut Vec<ValidationError>,
) {
    if local.as_index() >= body.locals.len() {
        out.push(ValidationError::TerminatorLocalDangling {
            body: body_id,
            block: block_id,
            local,
        });
    }
}
