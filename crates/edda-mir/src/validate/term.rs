//! Terminator successor / local / ADT-shape walks.

use crate::adt::{AdtDef, AdtKind};
use crate::body::Body;
use crate::error::ValidationError;
use crate::ids::{AdtId, BlockId, BodyId, LocalId, VariantIdx};
use crate::operand::Operand;
use crate::place::Place;
use crate::program::MirProgram;
use crate::terminator::{CallArg, FuncRef, Terminator, TerminatorKind, ThreadedCapability};
use crate::ty::MirTypeKind;

use super::locals;

/// Walk a terminator: check successor block ids, local ids, and ADT-shape
/// invariants where applicable.
pub(super) fn check_terminator(
    program: &MirProgram,
    body_id: BodyId,
    body: &Body,
    block_id: BlockId,
    term: &Terminator,
    out: &mut Vec<ValidationError>,
) {
    check_terminator_successors(body_id, body, block_id, term, out);
    check_terminator_locals(body_id, body, block_id, term, out);
    check_terminator_adt_shape(program, body_id, body, block_id, term, out);
}

/// Every successor `BlockId` carried in the terminator must index `body.blocks`.
fn check_terminator_successors(
    body_id: BodyId,
    body: &Body,
    block_id: BlockId,
    term: &Terminator,
    out: &mut Vec<ValidationError>,
) {
    for succ in successors_of(&term.kind) {
        if succ.as_index() >= body.blocks.len() {
            out.push(ValidationError::BlockSuccessorDangling {
                body: body_id,
                block: block_id,
                successor: succ,
            });
        }
    }
}

/// Enumerate the successor blocks a terminator can branch to.
fn successors_of(kind: &TerminatorKind) -> Vec<BlockId> {
    match kind {
        TerminatorKind::Return(_)
        | TerminatorKind::Raise { .. }
        | TerminatorKind::Panic { .. }
        | TerminatorKind::Unreachable => Vec::new(),
        TerminatorKind::Goto(b) => vec![*b],
        TerminatorKind::SwitchBool {
            true_bb, false_bb, ..
        } => vec![*true_bb, *false_bb],
        TerminatorKind::SwitchTag {
            arms, otherwise, ..
        } => {
            let mut v = Vec::with_capacity(arms.len() + 1);
            for (_, b) in arms {
                v.push(*b);
            }
            v.push(*otherwise);
            v
        }
        TerminatorKind::Call {
            target, on_error, ..
        } => {
            let mut v = vec![*target];
            if let Some(e) = on_error {
                v.push(*e);
            }
            v
        }
        TerminatorKind::Spawn { target, .. } | TerminatorKind::Await { target, .. } => {
            vec![*target]
        }
    }
}

/// Every local referenced via Operand / Place in the terminator must index
/// `body.locals`.
fn check_terminator_locals(
    body_id: BodyId,
    body: &Body,
    block_id: BlockId,
    term: &Terminator,
    out: &mut Vec<ValidationError>,
) {
    for op in operands_of_terminator(&term.kind) {
        locals::check_operand_term(body_id, body, block_id, op, out);
    }
    if let Some(place) = destination_of_terminator(&term.kind) {
        locals::check_place_locals_term(body_id, body, block_id, place, out);
    }
    for local in bare_locals_of_terminator(&term.kind) {
        locals::check_local_term(body_id, body, block_id, local, out);
    }
}

/// Yield operands carried by a terminator.
fn operands_of_terminator(kind: &TerminatorKind) -> Vec<&Operand> {
    match kind {
        TerminatorKind::Return(op) => vec![op],
        TerminatorKind::Goto(_) | TerminatorKind::Unreachable => Vec::new(),
        TerminatorKind::SwitchBool { cond, .. } => vec![cond],
        TerminatorKind::SwitchTag { subject, .. } => vec![subject],
        TerminatorKind::Call { args, .. } => args.iter().map(|a: &CallArg| &a.operand).collect(),
        TerminatorKind::Raise { value, .. } => vec![value],
        TerminatorKind::Panic { msg } => vec![msg],
        TerminatorKind::Spawn { args, .. } => args.iter().collect(),
        TerminatorKind::Await { task, .. } => vec![task],
    }
}

/// Yield bare `LocalId` fields carried directly by a terminator (not wrapped
/// in a `Place`/`Operand`) — `Spawn`'s task-group handle and destination,
/// `Await`'s destination.
fn bare_locals_of_terminator(kind: &TerminatorKind) -> Vec<LocalId> {
    match kind {
        TerminatorKind::Spawn {
            group_local, dest, ..
        } => vec![*group_local, *dest],
        TerminatorKind::Await { dest, .. } => vec![*dest],
        _ => Vec::new(),
    }
}

/// Yield the destination place of a terminator, when it has one.
fn destination_of_terminator(kind: &TerminatorKind) -> Option<&Place> {
    match kind {
        TerminatorKind::Call { destination, .. } => Some(destination),
        _ => None,
    }
}

/// SwitchTag adt+variant range + duplicate-arm, Call func target sanity.
fn check_terminator_adt_shape(
    program: &MirProgram,
    body_id: BodyId,
    body: &Body,
    block_id: BlockId,
    term: &Terminator,
    out: &mut Vec<ValidationError>,
) {
    match &term.kind {
        TerminatorKind::SwitchTag {
            adt: adt_id, arms, ..
        } => {
            check_switch_tag(program, body_id, block_id, *adt_id, arms, out);
        }
        TerminatorKind::Call {
            func,
            args,
            capabilities,
            ..
        } => {
            check_call_target(program, body_id, body, block_id, func, out);
            check_call_cap_pairings(body_id, body, block_id, args, capabilities, out);
        }
        TerminatorKind::Spawn { child, .. } => {
            check_spawn_target(program, body_id, block_id, *child, out);
        }
        _ => {}
    }
}

/// Call: every capability `value_arg` pairing must name a
/// capability-typed positional argument of the same call.
fn check_call_cap_pairings(
    body_id: BodyId,
    body: &Body,
    block_id: BlockId,
    args: &[CallArg],
    capabilities: &[ThreadedCapability],
    out: &mut Vec<ValidationError>,
) {
    for cap in capabilities {
        let Some(value_arg) = cap.value_arg else {
            continue;
        };
        let malformed = ValidationError::CallCapValueArgMalformed {
            body: body_id,
            block: block_id,
            value_arg,
        };
        let Some(arg) = args.get(value_arg as usize) else {
            out.push(malformed);
            continue;
        };
        let place = match &arg.operand {
            Operand::Copy(p) | Operand::Move(p) => p,
            _ => {
                out.push(malformed);
                continue;
            }
        };
        let cap_typed = place.projection.is_empty()
            && body
                .locals
                .get(place.local)
                .is_some_and(|d| matches!(d.ty.kind, MirTypeKind::Capability(_)));
        if !cap_typed {
            out.push(malformed);
        }
    }
}

/// SwitchTag: ADT must be Sum, every arm's variant must be in range, no
/// duplicate arm variants.
fn check_switch_tag(
    program: &MirProgram,
    body_id: BodyId,
    block_id: BlockId,
    adt_id: AdtId,
    arms: &[(VariantIdx, BlockId)],
    out: &mut Vec<ValidationError>,
) {
    let Some(adt) = program.adts.get(adt_id) else {
        return;
    };
    if adt.kind != AdtKind::Sum {
        out.push(ValidationError::SwitchTagAdtKindMismatch {
            body: body_id,
            block: block_id,
            adt: adt_id,
            found: adt.kind,
        });
    }
    check_switch_tag_arms(body_id, block_id, adt_id, adt, arms, out);
}

/// Per-arm range and duplicate check; split out to keep `check_switch_tag`
/// under the nesting budget.
fn check_switch_tag_arms(
    body_id: BodyId,
    block_id: BlockId,
    adt_id: AdtId,
    adt: &AdtDef,
    arms: &[(VariantIdx, BlockId)],
    out: &mut Vec<ValidationError>,
) {
    let mut seen: Vec<VariantIdx> = Vec::with_capacity(arms.len());
    for (variant, _) in arms {
        if variant.as_index() >= adt.variants.len() {
            out.push(ValidationError::SwitchTagVariantDangling {
                body: body_id,
                block: block_id,
                adt: adt_id,
                variant: *variant,
            });
            continue;
        }
        if seen.contains(variant) {
            out.push(ValidationError::SwitchTagDuplicateArm {
                body: body_id,
                block: block_id,
                variant: *variant,
            });
        } else {
            seen.push(*variant);
        }
    }
}

/// FuncRef::Body must reference a real body (not DUMMY, in range).
fn check_call_target(
    program: &MirProgram,
    body_id: BodyId,
    _body: &Body,
    block_id: BlockId,
    func: &FuncRef,
    out: &mut Vec<ValidationError>,
) {
    let FuncRef::Body(callee) = func else {
        return;
    };
    if *callee == BodyId::DUMMY {
        out.push(ValidationError::CallTargetIsDummy {
            body: body_id,
            block: block_id,
        });
        return;
    }
    if program.bodies.get(*callee).is_none() {
        out.push(ValidationError::CallExternBodyIdNonsense {
            body: body_id,
            block: block_id,
        });
    }
}

/// `Spawn::child` must reference a real body (not DUMMY, in range) — the
/// `Call`-target sibling check.
fn check_spawn_target(
    program: &MirProgram,
    body_id: BodyId,
    block_id: BlockId,
    child: BodyId,
    out: &mut Vec<ValidationError>,
) {
    if child == BodyId::DUMMY {
        out.push(ValidationError::SpawnTargetIsDummy {
            body: body_id,
            block: block_id,
        });
        return;
    }
    if program.bodies.get(child).is_none() {
        out.push(ValidationError::SpawnTargetBodyIdNonsense {
            body: body_id,
            block: block_id,
        });
    }
}
