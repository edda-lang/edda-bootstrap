//! Structural validation pass for [`MirProgram`].
//!
//! [`validate`] walks an entire program and collects every structural invariant
//! violation it can detect by inspection. The pass is pure (`&MirProgram` ->
//! `Vec<ValidationError>`); it does not allocate beyond the result vector and
//! does not stop at the first error — callers get a complete failure report
//! per invocation.
//!
//! The module is split by check family:
//! - [`mod@self`] (this file) — top-level orchestration + per-body checks
//!   that touch body-scoped state (entry, return slot, params).
//! - [`stmts`] — statement and rvalue walks.
//! - [`term`] — terminator successor / local / ADT-shape walks.
//! - [`locals`] — place / operand / single-local range helpers shared by
//!   the other two.

mod locals;
mod stmts;
mod term;

use crate::block::BasicBlockData;
use crate::body::{Body, LocalSource};
use crate::error::ValidationError;
use crate::ids::BodyId;
use crate::program::MirProgram;

/// Validate a [`MirProgram`] against structural invariants.
///
/// Returns every problem found rather than stopping at the first. Callers
/// who want a `Result` should check `if errors.is_empty()`.
pub fn validate(program: &MirProgram) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    for (body_id, body) in program.bodies.iter_enumerated() {
        check_body(program, body_id, body, &mut errors);
    }
    errors
}

/// Drive every per-body check; helpers append to `out` so the caller can
/// concatenate across bodies without intermediate vectors.
fn check_body(
    program: &MirProgram,
    body_id: BodyId,
    body: &Body,
    out: &mut Vec<ValidationError>,
) {
    check_entry(body_id, body, out);
    check_return_slot(body_id, body, out);
    check_params(body_id, body, out);
    for (block_id, block) in body.blocks.iter_enumerated() {
        check_block(program, body_id, body, block_id, block, out);
    }
}

/// Body `entry` must index into `body.blocks`.
fn check_entry(body_id: BodyId, body: &Body, out: &mut Vec<ValidationError>) {
    if body.entry.as_index() >= body.blocks.len() {
        out.push(ValidationError::BodyEntryDangling {
            body: body_id,
            entry: body.entry,
        });
    }
}

/// Body must have exactly one `LocalSource::ReturnSlot`.
fn check_return_slot(body_id: BodyId, body: &Body, out: &mut Vec<ValidationError>) {
    let count = body
        .locals
        .iter()
        .filter(|l| matches!(l.source, LocalSource::ReturnSlot))
        .count();
    match count {
        0 => out.push(ValidationError::ReturnSlotMissing { body: body_id }),
        1 => {}
        n => out.push(ValidationError::DuplicateReturnSlot {
            body: body_id,
            count: n,
        }),
    }
}

/// Each `ParamInfo` must point at a local whose `source == Param(i)` where
/// `i` matches the param's own position.
fn check_params(body_id: BodyId, body: &Body, out: &mut Vec<ValidationError>) {
    for (i, param) in body.params.iter().enumerate() {
        let idx = i as u32;
        let local = param.local;
        let Some(decl) = body.locals.get(local) else {
            out.push(ValidationError::ParamLocalMismatch {
                body: body_id,
                param_index: idx,
                local,
            });
            continue;
        };
        match decl.source {
            LocalSource::Param(j) if j == idx => {}
            _ => out.push(ValidationError::ParamLocalMismatch {
                body: body_id,
                param_index: idx,
                local,
            }),
        }
    }
}

/// Run every per-block check (statements, terminator).
fn check_block(
    program: &MirProgram,
    body_id: BodyId,
    body: &Body,
    block_id: crate::ids::BlockId,
    block: &BasicBlockData,
    out: &mut Vec<ValidationError>,
) {
    for (idx, stmt) in block.stmts.iter().enumerate() {
        stmts::check_statement(program, body_id, body, block_id, idx, stmt, out);
    }
    term::check_terminator(program, body_id, body, block_id, &block.terminator, out);
}
