//! Refutable pattern tests used inside `match` arms.
//!
//! Each entry returns the success-continuation block; the mismatch path
//! falls through to `on_miss`. Sum-variant dispatch emits
//! `ExtractTag` + `SwitchTag`; product / struct dispatch walks fields with
//! `ExtractField` rvalues and recurses via [`lower_pattern_test`].

use edda_span::Span;
use edda_syntax::ast::{Ident, Literal, RangeKind};
use edda_types::{HirPat, HirPatKind, HirPath, HirStructPatField, HirVariantPatPayload, TyKind};

use crate::body::Mutability;
use crate::constant::{Const, ConstValue};
use crate::ids::{AdtId, BlockId, FieldIdx, LocalId, VariantIdx};
use crate::operand::Operand;
use crate::place::Place;
use crate::rvalue::{BinOp, Rvalue, RvalueKind};
use crate::statement::{Statement, StatementKind};
use crate::ty::{MirPrim, MirType};

use super::super::ctx::LoweringContext;
use super::super::ty::{lower_ty, ty_to_prim};
use super::{
    bind_subject_local, emit_extract_field, emit_extract_field_variant, emit_extract_tag,
    find_product_field_idx, find_variant_field_idx, find_variant_idx, is_product_adt,
    push_unsupported, resolve_adt, seal_switch_bool, seal_switch_tag, subject_to_local,
    unsupported_test,
};

/// Lower a refutable pattern test against `subject`. Emits the test
/// terminator and returns the success-continuation block id; the
/// mismatch path falls through to `on_miss`.
pub(in super::super) fn lower_pattern_test(
    ctx: &mut LoweringContext<'_>,
    pat: &HirPat,
    subject: Operand,
    on_match: BlockId,
    on_miss: BlockId,
) {
    match &pat.kind {
        HirPatKind::Wildcard | HirPatKind::Error => {
            super::super::cfg::goto(ctx, pat.span, on_match);
        }
        HirPatKind::Binding(ident) => {
            bind_subject_local(ctx, pat, subject, ident.name);
            super::super::cfg::goto(ctx, pat.span, on_match);
        }
        HirPatKind::Literal(lit) => {
            lower_literal_test(ctx, pat, lit, subject, on_match, on_miss);
        }
        HirPatKind::Tuple(elements) => {
            lower_tuple_test(ctx, pat, elements, subject, on_match, on_miss);
        }
        HirPatKind::Variant { path, payload } => {
            lower_variant_test(ctx, pat, path, payload, subject, on_match, on_miss);
        }
        HirPatKind::Struct { fields, .. } => {
            lower_struct_test(ctx, pat, fields, subject, on_match, on_miss);
        }
        HirPatKind::Guard { .. } => unsupported_test(ctx, pat.span, "Guard", on_miss),
        HirPatKind::Range { lo, hi, kind } => {
            lower_range_test(ctx, pat, lo, hi, *kind, subject, on_match, on_miss);
        }
        HirPatKind::AtBinding { name, inner } => {
            lower_at_binding_test(ctx, pat, name, inner, subject, on_match, on_miss);
        }
        HirPatKind::Slice {
            prefix,
            rest,
            suffix,
        } => {
            lower_slice_test(ctx, pat, prefix, rest, suffix, subject, on_match, on_miss);
        }
    }
}

/// Refutable tuple test: extract each field into a fresh temp and recurse
/// on its sub-pattern.
fn lower_tuple_test(
    ctx: &mut LoweringContext<'_>,
    pat: &HirPat,
    elements: &[HirPat],
    subject: Operand,
    on_match: BlockId,
    on_miss: BlockId,
) {
    if elements.is_empty() {
        super::super::cfg::goto(ctx, pat.span, on_match);
        return;
    }
    let Some(subject_local) = subject_to_local(ctx, pat, &subject) else {
        super::super::cfg::goto(ctx, pat.span, on_miss);
        return;
    };
    let last = elements.len() - 1;
    for (i, sub_pat) in elements.iter().enumerate() {
        let elem_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, sub_pat.ty);
        let Some(temp) = ctx
            .body
            .as_mut()
            .map(|body| body.temp(elem_ty.clone(), sub_pat.span))
        else {
            return;
        };
        emit_extract_field(ctx, sub_pat.span, subject_local, i as u32, temp, elem_ty);
        let next_on_match = if i == last {
            on_match
        } else {
            match super::super::cfg::alloc_block(ctx) {
                Some(bb) => bb,
                None => return,
            }
        };
        lower_pattern_test(
            ctx,
            sub_pat,
            Operand::Copy(Place::local(temp)),
            next_on_match,
            on_miss,
        );
        if i < last {
            ctx.current_bb = Some(next_on_match);
        }
    }
}

/// Emit a literal-equality test: `subject == lit ? on_match : on_miss`.
fn lower_literal_test(
    ctx: &mut LoweringContext<'_>,
    pat: &HirPat,
    lit: &Literal,
    subject: Operand,
    on_match: BlockId,
    on_miss: BlockId,
) {
    let Some(prim) = ty_to_prim(ctx.ty_interner, pat.ty) else {
        push_unsupported(ctx, pat.span, "Literal pattern on non-primitive");
        return;
    };
    let Some(rhs) = literal_to_operand(ctx, pat.span, lit, prim) else {
        push_unsupported(ctx, pat.span, "Literal pattern shape");
        return;
    };
    let bool_ty = MirType::prim(MirPrim::Bool);
    let Some(body_builder) = ctx.body.as_mut() else { return };
    let cond = body_builder.temp(bool_ty.clone(), pat.span);
    let Some(bb) = ctx.current_bb else { return };
    let stmt = Statement {
        span: pat.span,
        kind: StatementKind::Assign {
            place: Place::local(cond),
            rvalue: Rvalue {
                span: pat.span,
                kind: RvalueKind::BinOp {
                    op: BinOp::Eq,
                    lhs: subject,
                    rhs,
                    prim,
                },
                ty: bool_ty,
            },
        },
    };
    body_builder.body_mut().blocks[bb].stmts.push(stmt);
    seal_switch_bool(
        ctx,
        pat.span,
        bb,
        Operand::Copy(Place::local(cond)),
        on_match,
        on_miss,
    );
}

/// Lower a primitive [`Literal`] into a constant operand suitable for an
/// equality test. Returns `None` for shapes the lowering pass does not
/// support (currently `FString`, which has no MIR const form).
fn literal_to_operand(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    lit: &Literal,
    prim: MirPrim,
) -> Option<Operand> {
    let value = match lit {
        Literal::Int { value, .. } => ConstValue::Int(*value as i128),
        Literal::Bool(b) => ConstValue::Bool(*b),
        Literal::Str(sym) => ConstValue::Str(*sym),
        Literal::Unit => ConstValue::Unit,
        Literal::Float(sym) => {
            let raw = ctx.interner.resolve(*sym);
            let parsed = raw.parse::<f64>().ok()?;
            ConstValue::Float(parsed.to_bits())
        }
    };
    let c = Const {
        ty: MirType::prim(prim),
        value,
    };
    let _ = span;
    Some(Operand::Const(ctx.program.push_const(c)))
}

/// Refutable variant pattern test. Resolves `path`'s last segment to a
/// [`VariantIdx`] inside the scrutinee's sum ADT, switches on the tag, and
/// recursively tests the payload sub-patterns inside the variant-downcast
/// body block.
fn lower_variant_test(
    ctx: &mut LoweringContext<'_>,
    pat: &HirPat,
    path: &HirPath,
    payload: &HirVariantPatPayload,
    subject: Operand,
    on_match: BlockId,
    on_miss: BlockId,
) {
    let Some(adt_id) = resolve_adt(ctx, pat) else {
        unsupported_test(ctx, pat.span, "Variant", on_miss);
        return;
    };
    let Some(variant_idx) = find_variant_idx(ctx, adt_id, path) else {
        unsupported_test(ctx, pat.span, "Variant", on_miss);
        return;
    };
    let Some(subject_local) = subject_to_local(ctx, pat, &subject) else {
        super::super::cfg::goto(ctx, pat.span, on_miss);
        return;
    };
    let Some(tag_local) = emit_extract_tag(ctx, pat.span, adt_id, subject_local) else {
        return;
    };
    let Some(body_bb) = super::super::cfg::alloc_block(ctx) else {
        return;
    };
    seal_switch_tag(ctx, pat.span, tag_local, adt_id, variant_idx, body_bb, on_miss);
    ctx.current_bb = Some(body_bb);
    test_variant_payload(
        ctx,
        pat.span,
        adt_id,
        variant_idx,
        subject_local,
        payload,
        on_match,
        on_miss,
    );
}

/// Refutable struct destructuring inside a `match` arm.
fn lower_struct_test(
    ctx: &mut LoweringContext<'_>,
    pat: &HirPat,
    fields: &[HirStructPatField],
    subject: Operand,
    on_match: BlockId,
    on_miss: BlockId,
) {
    let Some(adt_id) = resolve_adt(ctx, pat) else {
        unsupported_test(ctx, pat.span, "Struct", on_miss);
        return;
    };
    if !is_product_adt(ctx, adt_id) {
        unsupported_test(ctx, pat.span, "Struct on sum ADT", on_miss);
        return;
    }
    let Some(subject_local) = subject_to_local(ctx, pat, &subject) else {
        super::super::cfg::goto(ctx, pat.span, on_miss);
        return;
    };
    if fields.is_empty() {
        super::super::cfg::goto(ctx, pat.span, on_match);
        return;
    }
    let last = fields.len() - 1;
    for (i, field_pat) in fields.iter().enumerate() {
        let Some(field_idx) = find_product_field_idx(ctx, adt_id, field_pat.name.name) else {
            push_unsupported(ctx, field_pat.span, "Struct field name not in ADT");
            super::super::cfg::goto(ctx, field_pat.span, on_miss);
            return;
        };
        let elem_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, field_pat.pat.ty);
        let Some(temp) = ctx
            .body
            .as_mut()
            .map(|body| body.temp(elem_ty.clone(), field_pat.span))
        else {
            return;
        };
        emit_extract_field_variant(
            ctx,
            field_pat.span,
            subject_local,
            None,
            field_idx,
            temp,
            elem_ty,
        );
        let next_on_match = if i == last {
            on_match
        } else {
            match super::super::cfg::alloc_block(ctx) {
                Some(bb) => bb,
                None => return,
            }
        };
        lower_pattern_test(
            ctx,
            &field_pat.pat,
            Operand::Copy(Place::local(temp)),
            next_on_match,
            on_miss,
        );
        if i < last {
            ctx.current_bb = Some(next_on_match);
        }
    }
}

/// Walk a variant's payload sub-patterns once the tag matched. Allocates
/// fresh temps for each field, emits `ExtractField` with the variant tag set
/// so the compile-side knows to downcast, and threads success of every
/// element to the next via fresh blocks.
fn test_variant_payload(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    adt_id: AdtId,
    variant_idx: VariantIdx,
    subject_local: LocalId,
    payload: &HirVariantPatPayload,
    on_match: BlockId,
    on_miss: BlockId,
) {
    let sub_patterns = collect_payload_subpatterns(payload, ctx, adt_id, variant_idx);
    let Some(sub_patterns) = sub_patterns else {
        super::super::cfg::goto(ctx, span, on_miss);
        return;
    };
    if sub_patterns.is_empty() {
        super::super::cfg::goto(ctx, span, on_match);
        return;
    }
    let last = sub_patterns.len() - 1;
    for (i, (field_idx, sub_pat)) in sub_patterns.into_iter().enumerate() {
        let elem_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, sub_pat.ty);
        let Some(temp) = ctx
            .body
            .as_mut()
            .map(|body| body.temp(elem_ty.clone(), sub_pat.span))
        else {
            return;
        };
        emit_extract_field_variant(
            ctx,
            sub_pat.span,
            subject_local,
            Some(variant_idx),
            field_idx,
            temp,
            elem_ty,
        );
        let next_on_match = if i == last {
            on_match
        } else {
            match super::super::cfg::alloc_block(ctx) {
                Some(bb) => bb,
                None => return,
            }
        };
        lower_pattern_test(
            ctx,
            sub_pat,
            Operand::Copy(Place::local(temp)),
            next_on_match,
            on_miss,
        );
        if i < last {
            ctx.current_bb = Some(next_on_match);
        }
    }
}

/// Pair each payload sub-pattern with the `FieldIdx` it should read.
fn collect_payload_subpatterns<'p>(
    payload: &'p HirVariantPatPayload,
    ctx: &mut LoweringContext<'_>,
    adt_id: AdtId,
    variant_idx: VariantIdx,
) -> Option<Vec<(FieldIdx, &'p HirPat)>> {
    match payload {
        HirVariantPatPayload::None => Some(Vec::new()),
        HirVariantPatPayload::Tuple(elems) => Some(
            elems
                .iter()
                .enumerate()
                .map(|(i, p)| (FieldIdx::from_raw(i as u32), p))
                .collect(),
        ),
        HirVariantPatPayload::Struct(struct_fields) => {
            let mut out = Vec::with_capacity(struct_fields.len());
            for field_pat in struct_fields.iter() {
                let Some(field_idx) =
                    find_variant_field_idx(ctx, adt_id, variant_idx, field_pat.name.name)
                else {
                    push_unsupported(ctx, field_pat.span, "Variant field name not in ADT");
                    return None;
                };
                out.push((field_idx, &field_pat.pat));
            }
            Some(out)
        }
    }
}

/// Refutable range test (`lo..<hi` / `lo..=hi`). Lowers to two bound
/// comparisons over `subject` joined by `SwitchBool`s: `subject >= lo`
/// then `subject < hi` (half-open) or `subject <= hi` (closed).
fn lower_range_test(
    ctx: &mut LoweringContext<'_>,
    pat: &HirPat,
    lo: &Literal,
    hi: &Literal,
    kind: RangeKind,
    subject: Operand,
    on_match: BlockId,
    on_miss: BlockId,
) {
    let Some(prim) = ty_to_prim(ctx.ty_interner, pat.ty) else {
        unsupported_test(ctx, pat.span, "Range pattern on non-primitive", on_miss);
        return;
    };
    let (Some(lo_op), Some(hi_op)) = (
        literal_to_operand(ctx, pat.span, lo, prim),
        literal_to_operand(ctx, pat.span, hi, prim),
    ) else {
        unsupported_test(ctx, pat.span, "Range pattern bound shape", on_miss);
        return;
    };
    let Some(ge) = emit_bool_cmp(ctx, pat.span, BinOp::Ge, subject.clone(), lo_op, prim) else {
        return;
    };
    let Some(hi_bb) = super::super::cfg::alloc_block(ctx) else {
        return;
    };
    let Some(lo_bb) = ctx.current_bb else { return };
    seal_switch_bool(
        ctx,
        pat.span,
        lo_bb,
        Operand::Copy(Place::local(ge)),
        hi_bb,
        on_miss,
    );
    ctx.current_bb = Some(hi_bb);
    let cmp_op = match kind {
        RangeKind::HalfOpen => BinOp::Lt,
        RangeKind::Closed => BinOp::Le,
    };
    let Some(hic) = emit_bool_cmp(ctx, pat.span, cmp_op, subject, hi_op, prim) else {
        return;
    };
    seal_switch_bool(
        ctx,
        pat.span,
        hi_bb,
        Operand::Copy(Place::local(hic)),
        on_match,
        on_miss,
    );
}

/// Refutable `@`-binding test (`name @ inner`). Binds the matched value to
/// `name` and recursively tests its shape against `inner`.
fn lower_at_binding_test(
    ctx: &mut LoweringContext<'_>,
    pat: &HirPat,
    name: &Ident,
    inner: &HirPat,
    subject: Operand,
    on_match: BlockId,
    on_miss: BlockId,
) {
    bind_subject_local(ctx, pat, subject.clone(), name.name);
    lower_pattern_test(ctx, inner, subject, on_match, on_miss);
}

/// Refutable slice test (`[head, ..tail]` / `[..init, last]` / `[]`).
/// Tests the length, then reads prefix elements from the low end, the
/// rest binding as a sub-slice, and suffix elements from the high end,
/// recursing on each element sub-pattern.
fn lower_slice_test(
    ctx: &mut LoweringContext<'_>,
    pat: &HirPat,
    prefix: &[HirPat],
    rest: &Option<Option<Ident>>,
    suffix: &[HirPat],
    subject: Operand,
    on_match: BlockId,
    on_miss: BlockId,
) {
    let TyKind::Slice(elem_ty_id) = ctx.ty_interner.kind(pat.ty) else {
        unsupported_test(ctx, pat.span, "Slice pattern on non-slice", on_miss);
        return;
    };
    let elem_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, *elem_ty_id);
    let slice_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, pat.ty);
    let usize_ty = MirType::prim(MirPrim::Usize);
    let Some(subject_local) = subject_to_local(ctx, pat, &subject) else {
        super::super::cfg::goto(ctx, pat.span, on_miss);
        return;
    };
    let Some(len_local) = ctx
        .body
        .as_mut()
        .map(|b| b.temp(usize_ty.clone(), pat.span))
    else {
        return;
    };
    super::super::cfg::push_assign(
        ctx,
        pat.span,
        len_local,
        Rvalue {
            span: pat.span,
            kind: RvalueKind::ArrayLen {
                array: Operand::Copy(Place::local(subject_local)),
            },
            ty: usize_ty.clone(),
        },
    );
    let min_len = (prefix.len() + suffix.len()) as u128;
    let min_id = ctx.program.push_const(Const {
        ty: usize_ty.clone(),
        value: ConstValue::Uint(min_len),
    });
    let cmp_op = if rest.is_some() {
        BinOp::Ge
    } else {
        BinOp::Eq
    };
    let Some(len_ok) = emit_bool_cmp(
        ctx,
        pat.span,
        cmp_op,
        Operand::Copy(Place::local(len_local)),
        Operand::Const(min_id),
        MirPrim::Usize,
    ) else {
        return;
    };
    let Some(body_bb) = super::super::cfg::alloc_block(ctx) else {
        return;
    };
    let Some(head_bb) = ctx.current_bb else { return };
    seal_switch_bool(
        ctx,
        pat.span,
        head_bb,
        Operand::Copy(Place::local(len_ok)),
        body_bb,
        on_miss,
    );
    ctx.current_bb = Some(body_bb);

    for (i, sub) in prefix.iter().enumerate() {
        let idx_id = ctx.program.push_const(Const {
            ty: usize_ty.clone(),
            value: ConstValue::Uint(i as u128),
        });
        if !slice_element_test(
            ctx,
            sub,
            subject_local,
            Operand::Const(idx_id),
            elem_ty.clone(),
            on_miss,
        ) {
            return;
        }
    }

    if let Some(Some(name)) = rest {
        let lo_id = ctx.program.push_const(Const {
            ty: usize_ty.clone(),
            value: ConstValue::Uint(prefix.len() as u128),
        });
        let suf_id = ctx.program.push_const(Const {
            ty: usize_ty.clone(),
            value: ConstValue::Uint(suffix.len() as u128),
        });
        let Some(hi_tmp) = ctx
            .body
            .as_mut()
            .map(|b| b.temp(usize_ty.clone(), name.span))
        else {
            return;
        };
        super::super::cfg::push_assign(
            ctx,
            name.span,
            hi_tmp,
            Rvalue {
                span: name.span,
                kind: RvalueKind::BinOp {
                    op: BinOp::Sub,
                    lhs: Operand::Copy(Place::local(len_local)),
                    rhs: Operand::Const(suf_id),
                    prim: MirPrim::Usize,
                },
                ty: usize_ty.clone(),
            },
        );
        let Some(rest_local) = ctx
            .body
            .as_mut()
            .map(|b| b.user_local(name.name, Mutability::Imm, slice_ty.clone(), name.span))
        else {
            return;
        };
        super::super::cfg::push_assign(
            ctx,
            name.span,
            rest_local,
            Rvalue {
                span: name.span,
                kind: RvalueKind::SliceSubrange {
                    source: Operand::Copy(Place::local(subject_local)),
                    lo: Operand::Const(lo_id),
                    hi: Operand::Copy(Place::local(hi_tmp)),
                },
                ty: slice_ty.clone(),
            },
        );
        ctx.bindings.insert(name.name, rest_local);
    }

    for (j, sub) in suffix.iter().enumerate() {
        let back = (suffix.len() - j) as u128;
        let back_id = ctx.program.push_const(Const {
            ty: usize_ty.clone(),
            value: ConstValue::Uint(back),
        });
        let Some(idx_tmp) = ctx
            .body
            .as_mut()
            .map(|b| b.temp(usize_ty.clone(), sub.span))
        else {
            return;
        };
        super::super::cfg::push_assign(
            ctx,
            sub.span,
            idx_tmp,
            Rvalue {
                span: sub.span,
                kind: RvalueKind::BinOp {
                    op: BinOp::Sub,
                    lhs: Operand::Copy(Place::local(len_local)),
                    rhs: Operand::Const(back_id),
                    prim: MirPrim::Usize,
                },
                ty: usize_ty.clone(),
            },
        );
        if !slice_element_test(
            ctx,
            sub,
            subject_local,
            Operand::Copy(Place::local(idx_tmp)),
            elem_ty.clone(),
            on_miss,
        ) {
            return;
        }
    }

    super::super::cfg::goto(ctx, pat.span, on_match);
}

/// Extract one slice element into a fresh temp and recursively test it
/// against `sub`. On a sub-pattern miss control falls through to
/// `on_miss`; on a match `current_bb` is advanced to the success block so
/// the caller can emit the next element test.
fn slice_element_test(
    ctx: &mut LoweringContext<'_>,
    sub: &HirPat,
    subject_local: LocalId,
    idx: Operand,
    elem_ty: MirType,
    on_miss: BlockId,
) -> bool {
    let Some(temp) = ctx.body.as_mut().map(|b| b.temp(elem_ty.clone(), sub.span)) else {
        return false;
    };
    super::super::cfg::push_assign(
        ctx,
        sub.span,
        temp,
        Rvalue {
            span: sub.span,
            kind: RvalueKind::ArrayIndex {
                array: Operand::Copy(Place::local(subject_local)),
                idx,
            },
            ty: elem_ty,
        },
    );
    let Some(next_bb) = super::super::cfg::alloc_block(ctx) else {
        return false;
    };
    lower_pattern_test(ctx, sub, Operand::Copy(Place::local(temp)), next_bb, on_miss);
    ctx.current_bb = Some(next_bb);
    true
}

/// Emit a `bool`-typed comparison `lhs <op> rhs` into a fresh temp and
/// return the temp local.
fn emit_bool_cmp(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    op: BinOp,
    lhs: Operand,
    rhs: Operand,
    prim: MirPrim,
) -> Option<LocalId> {
    let bool_ty = MirType::prim(MirPrim::Bool);
    let cond = ctx.body.as_mut()?.temp(bool_ty.clone(), span);
    let bb = ctx.current_bb?;
    let body_builder = ctx.body.as_mut()?;
    body_builder.body_mut().blocks[bb].stmts.push(Statement {
        span,
        kind: StatementKind::Assign {
            place: Place::local(cond),
            rvalue: Rvalue {
                span,
                kind: RvalueKind::BinOp { op, lhs, rhs, prim },
                ty: bool_ty,
            },
        },
    });
    Some(cond)
}
