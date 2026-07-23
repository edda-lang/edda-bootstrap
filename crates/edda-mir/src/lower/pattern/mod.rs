//! Pattern lowering: shared helpers and dispatch.
//!
//! Two responsibilities live behind separate entry points because the rest
//! of the lowering pass dispatches into one or the other depending on
//! context — never both at the same call site:
//!
//! - [`install::install_bindings`] is irrefutable destructuring for the
//!   LHS of `let` / `for`. Re-exported as `install_bindings` for the
//!   pattern entry callers (`super::stmt`).
//! - [`test::lower_pattern_test`] is the refutable test used inside
//!   `match` arms. Re-exported as `lower_pattern_test`.
//!
//! Both responsibilities share a set of low-level helpers — subject
//! materialisation, field/tag extraction, ADT lookup — which live in this
//! file so they're addressable as `super::*` from both submodules.

mod install;
mod test;

pub(super) use install::install_bindings;
pub(super) use test::lower_pattern_test;

use edda_intern::Symbol;
use edda_span::Span;
use edda_types::{HirPat, TyKind};

use crate::adt::AdtKind;
use crate::body::Mutability;
use crate::error::{LoweringError, MirError};
use crate::ids::{AdtId, BlockId, FieldIdx, LocalId, VariantIdx};
use crate::operand::Operand;
use crate::place::Place;
use crate::rvalue::{Rvalue, RvalueKind};
use crate::statement::{Statement, StatementKind};
use crate::terminator::{Terminator, TerminatorKind};
use crate::ty::MirType;

use super::ctx::LoweringContext;
use super::ty::lower_ty;

/// Append an `Assign { dest = ExtractField(source, variant?, field) }`
/// statement to the active block. Shared by tuple-test, struct-test, and
/// variant-payload-test paths.
pub(super) fn emit_extract_field_variant(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    source: LocalId,
    variant: Option<VariantIdx>,
    field: FieldIdx,
    dest: LocalId,
    elem_ty: MirType,
) {
    let Some(bb) = ctx.current_bb else { return };
    let Some(body_builder) = ctx.body.as_mut() else { return };
    let stmt = Statement {
        span,
        kind: StatementKind::Assign {
            place: Place::local(dest),
            rvalue: Rvalue {
                span,
                kind: RvalueKind::ExtractField {
                    subject: Operand::Copy(Place::local(source)),
                    variant,
                    field,
                },
                ty: elem_ty,
            },
        },
    };
    body_builder.body_mut().blocks[bb].stmts.push(stmt);
}

/// Append an `Assign { dest = ExtractField(source.<idx>) }` statement to
/// the active block. Used for tuple field reads (`variant: None`).
fn emit_extract_field(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    source: LocalId,
    field_idx: u32,
    dest: LocalId,
    elem_ty: MirType,
) {
    emit_extract_field_variant(
        ctx,
        span,
        source,
        None,
        FieldIdx::from_raw(field_idx),
        dest,
        elem_ty,
    );
}

/// Emit `tag = ExtractTag(subject)` into a fresh temp and return the temp.
fn emit_extract_tag(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    adt_id: AdtId,
    subject_local: LocalId,
) -> Option<LocalId> {
    let tag_prim = ctx.program.program().adts.get(adt_id)?.tag_width?;
    let tag_ty = MirType::prim(tag_prim);
    let body_builder = ctx.body.as_mut()?;
    let tag_local = body_builder.temp(tag_ty.clone(), span);
    let bb = ctx.current_bb?;
    let body_builder = ctx.body.as_mut()?;
    let stmt = Statement {
        span,
        kind: StatementKind::Assign {
            place: Place::local(tag_local),
            rvalue: Rvalue {
                span,
                kind: RvalueKind::ExtractTag {
                    subject: Operand::Copy(Place::local(subject_local)),
                },
                ty: tag_ty,
            },
        },
    };
    body_builder.body_mut().blocks[bb].stmts.push(stmt);
    Some(tag_local)
}

/// Materialise the subject of a refutable test into a bare local. A
/// projection-free `Copy`/`Move` short-circuits to its underlying local;
/// projected places and constant/unit operands are copied into a fresh
/// temp so subsequent `ExtractField` rvalues can project from the root.
fn subject_to_local(
    ctx: &mut LoweringContext<'_>,
    pat: &HirPat,
    subject: &Operand,
) -> Option<LocalId> {
    if let Operand::Copy(p) | Operand::Move(p) = subject
        && p.projection.is_empty()
    {
        return Some(p.local);
    }
    let ty = lower_ty(ctx.ty_interner, &ctx.adt_map, pat.ty);
    let temp = ctx.body.as_mut()?.temp(ty.clone(), pat.span);
    let bb = ctx.current_bb?;
    let body_builder = ctx.body.as_mut()?;
    body_builder.body_mut().blocks[bb].stmts.push(Statement {
        span: pat.span,
        kind: StatementKind::Assign {
            place: Place::local(temp),
            rvalue: Rvalue {
                span: pat.span,
                kind: RvalueKind::Use(subject.clone()),
                ty,
            },
        },
    });
    Some(temp)
}

/// Allocate a fresh user-local for `name` and copy the subject operand into
/// it.
fn bind_subject_local(
    ctx: &mut LoweringContext<'_>,
    pat: &HirPat,
    subject: Operand,
    name: Symbol,
) {
    let ty = lower_ty(ctx.ty_interner, &ctx.adt_map, pat.ty);
    let Some(body_builder) = ctx.body.as_mut() else { return };
    let local = body_builder.user_local(name, Mutability::Imm, ty.clone(), pat.span);
    let Some(bb) = ctx.current_bb else { return };
    let stmt = Statement {
        span: pat.span,
        kind: StatementKind::Assign {
            place: Place::local(local),
            rvalue: Rvalue {
                span: pat.span,
                kind: RvalueKind::Use(subject),
                ty,
            },
        },
    };
    body_builder.body_mut().blocks[bb].stmts.push(stmt);
    ctx.bindings.insert(name, local);
}

/// Helper: seal block `bb` with a `SwitchBool` terminator branching to
/// `on_true` / `on_false`. Clears `current_bb` only if `bb` is the current
/// block.
fn seal_switch_bool(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    bb: BlockId,
    cond: Operand,
    on_true: BlockId,
    on_false: BlockId,
) {
    let Some(body_builder) = ctx.body.as_mut() else { return };
    let terminator = Terminator {
        span,
        kind: TerminatorKind::SwitchBool {
            cond,
            true_bb: on_true,
            false_bb: on_false,
        },
    };
    body_builder.body_mut().blocks[bb].terminator = terminator;
    if ctx.current_bb == Some(bb) {
        ctx.current_bb = None;
    }
}

/// Seal the current block with `SwitchTag { arms: [(variant_idx, body_bb)], otherwise: on_miss }`.
fn seal_switch_tag(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    tag_local: LocalId,
    adt_id: AdtId,
    variant_idx: VariantIdx,
    body_bb: BlockId,
    on_miss: BlockId,
) {
    let Some(bb) = ctx.current_bb else { return };
    let Some(body_builder) = ctx.body.as_mut() else { return };
    let terminator = Terminator {
        span,
        kind: TerminatorKind::SwitchTag {
            subject: Operand::Copy(Place::local(tag_local)),
            adt: adt_id,
            arms: vec![(variant_idx, body_bb)],
            otherwise: on_miss,
        },
    };
    body_builder.body_mut().blocks[bb].terminator = terminator;
    ctx.current_bb = None;
}

/// Resolve the [`AdtId`] for `pat.ty`. Returns `None` for non-nominal types
/// or nominal types whose binding is not in `ctx.adt_map`.
fn resolve_adt(ctx: &LoweringContext<'_>, pat: &HirPat) -> Option<AdtId> {
    match ctx.ty_interner.kind(pat.ty) {
        TyKind::Nominal(binding) => ctx.adt_map.get(binding).copied(),
        _ => None,
    }
}

/// Look up the variant index inside `adt_id` matching the path's last
/// segment name.
fn find_variant_idx(
    ctx: &LoweringContext<'_>,
    adt_id: AdtId,
    path: &edda_types::HirPath,
) -> Option<VariantIdx> {
    let last = path.segments.last()?;
    let adt = ctx.program.program().adts.get(adt_id)?;
    adt.variants
        .iter()
        .position(|v| v.name == last.name)
        .map(|i| VariantIdx::from_raw(i as u32))
}

/// Look up the field index for a named field inside `adt_id`'s single
/// product variant. Returns `None` when the field name is absent.
fn find_product_field_idx(
    ctx: &LoweringContext<'_>,
    adt_id: AdtId,
    name: Symbol,
) -> Option<FieldIdx> {
    let adt = ctx.program.program().adts.get(adt_id)?;
    let variant = adt.variants.first()?;
    variant
        .fields
        .iter()
        .position(|f| f.name == name)
        .map(|i| FieldIdx::from_raw(i as u32))
}

/// Look up the field index for a named field inside `(adt_id, variant_idx)`.
/// Returns `None` when the field name is absent.
fn find_variant_field_idx(
    ctx: &LoweringContext<'_>,
    adt_id: AdtId,
    variant_idx: VariantIdx,
    name: Symbol,
) -> Option<FieldIdx> {
    let adt = ctx.program.program().adts.get(adt_id)?;
    let variant = adt.variants.get(variant_idx.as_index())?;
    variant
        .fields
        .iter()
        .position(|f| f.name == name)
        .map(|i| FieldIdx::from_raw(i as u32))
}

/// True when `adt_id` is registered as a [`AdtKind::Product`].
fn is_product_adt(ctx: &LoweringContext<'_>, adt_id: AdtId) -> bool {
    ctx.program
        .program()
        .adts
        .get(adt_id)
        .map(|adt| adt.kind == AdtKind::Product)
        .unwrap_or(false)
}

/// Push an `UnsupportedPattern` error against `span` with `kind` as the tag.
fn push_unsupported(ctx: &mut LoweringContext<'_>, span: Span, kind: &'static str) {
    ctx.errors.push(MirError::from(LoweringError::UnsupportedPattern { kind, span }));
}

/// `lower_pattern_test` fallback: emit the unsupported diagnostic then fall
/// through to `on_miss` so callers see consistent control flow.
fn unsupported_test(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    kind: &'static str,
    on_miss: BlockId,
) {
    push_unsupported(ctx, span, kind);
    super::cfg::goto(ctx, span, on_miss);
}
