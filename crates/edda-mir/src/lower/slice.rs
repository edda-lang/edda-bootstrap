//! Slice index + sub-range lowering.
//!
//! `receiver[idx]` lowers either to a single-element read
//! (`RvalueKind::ArrayIndex`) or — when `idx` is a `Range` expression — to a
//! sub-slice borrow (`RvalueKind::SliceSubrange`). Open-ended endpoints
//! (`..hi`, `lo..`, `..`) are synthesised at lower time so the rvalue's
//! operand shape stays uniform regardless of which endpoints the user
//! supplied.

use edda_span::Span;
use edda_types::{HirExpr, HirExprKind, TyKind};

use crate::constant::{Const, ConstValue};
use crate::operand::Operand;
use crate::place::Place;
use crate::rvalue::{Rvalue, RvalueKind};
use crate::ty::{MirPrim, MirType, MirTypeKind};

use super::cfg::push_assign;
use super::ctx::LoweringContext;
use super::expr::{lower_expr_to_operand, unsupported_and_unit};
use super::ty::lower_ty;

/// Lower `receiver[index]` — either a slice element read or a sub-slice
/// borrow.
pub(super) fn lower_index(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    receiver: &HirExpr,
    index: &HirExpr,
) -> Operand {
    let elem_ty_id = match ctx.ty_interner.kind(receiver.ty) {
        TyKind::Slice(elem) => *elem,
        _ => {
            return unsupported_and_unit(ctx, expr.span, "Index (non-slice receiver)");
        }
    };
    if let HirExprKind::Range { lo, hi, kind } = &index.kind {
        return lower_subrange(
            ctx,
            expr,
            receiver,
            elem_ty_id,
            lo.as_deref(),
            hi.as_deref(),
            *kind,
        );
    }
    let array = lower_expr_to_operand(ctx, receiver);
    let idx = lower_expr_to_operand(ctx, index);
    let element_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, elem_ty_id);
    let temp = match ctx.body.as_mut() {
        Some(body) => body.temp(element_ty.clone(), expr.span),
        None => return Operand::Unit,
    };
    let rvalue = Rvalue {
        span: expr.span,
        kind: RvalueKind::ArrayIndex { array, idx },
        ty: element_ty,
    };
    push_assign(ctx, expr.span, temp, rvalue);
    Operand::Move(Place::local(temp))
}

/// Lower `receiver[lo?..<hi?]` to `RvalueKind::SliceSubrange`.
fn lower_subrange(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    receiver: &HirExpr,
    elem_ty_id: edda_types::TyId,
    lo: Option<&HirExpr>,
    hi: Option<&HirExpr>,
    kind: edda_syntax::ast::RangeKind,
) -> Operand {
    use edda_syntax::ast::RangeKind;
    if !matches!(kind, RangeKind::HalfOpen) {
        return unsupported_and_unit(ctx, expr.span, "Index (closed range `..=`)");
    }
    let source = lower_expr_to_operand(ctx, receiver);
    let usize_ty = MirType::prim(MirPrim::Usize);
    let lo_op = match lo {
        Some(expr) => lower_expr_to_operand(ctx, expr),
        None => default_zero_usize(ctx, expr.span, &usize_ty),
    };
    let hi_op = match hi {
        Some(expr) => lower_expr_to_operand(ctx, expr),
        None => default_source_len(ctx, expr.span, &source, &usize_ty),
    };
    let elem_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, elem_ty_id);
    let slice_ty = MirType::new(MirTypeKind::Slice(Box::new(elem_ty)));
    let temp = match ctx.body.as_mut() {
        Some(body) => body.temp(slice_ty.clone(), expr.span),
        None => return Operand::Unit,
    };
    let rvalue = Rvalue {
        span: expr.span,
        kind: RvalueKind::SliceSubrange {
            source,
            lo: lo_op,
            hi: hi_op,
        },
        ty: slice_ty,
    };
    push_assign(ctx, expr.span, temp, rvalue);
    Operand::Move(Place::local(temp))
}

/// Synthesise the default `lo` operand for an open-low subrange: a
/// `0usize` constant stored in a fresh temp.
fn default_zero_usize(ctx: &mut LoweringContext<'_>, span: Span, usize_ty: &MirType) -> Operand {
    let zero_id = ctx.program.push_const(Const {
        ty: usize_ty.clone(),
        value: ConstValue::Uint(0),
    });
    let temp = match ctx.body.as_mut() {
        Some(body) => body.temp(usize_ty.clone(), span),
        None => return Operand::Unit,
    };
    let rvalue = Rvalue {
        span,
        kind: RvalueKind::Use(Operand::Const(zero_id)),
        ty: usize_ty.clone(),
    };
    push_assign(ctx, span, temp, rvalue);
    Operand::Move(Place::local(temp))
}

/// Synthesise the default `hi` operand for an open-high subrange:
/// `array_len(source)` stored in a fresh `usize`-typed temp.
fn default_source_len(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    source: &Operand,
    usize_ty: &MirType,
) -> Operand {
    let temp = match ctx.body.as_mut() {
        Some(body) => body.temp(usize_ty.clone(), span),
        None => return Operand::Unit,
    };
    let rvalue = Rvalue {
        span,
        kind: RvalueKind::ArrayLen {
            array: source.clone(),
        },
        ty: usize_ty.clone(),
    };
    push_assign(ctx, span, temp, rvalue);
    Operand::Move(Place::local(temp))
}
