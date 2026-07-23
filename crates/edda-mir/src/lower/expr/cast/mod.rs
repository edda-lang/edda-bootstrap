//! Primitive-cast expression lowering for the typed-HIR -> MIR pass.
//!
//! `expr as T [mode]` numeric casts. The trapping / wrapping modes lower
//! to a single `RvalueKind::Cast`; the `checked` / `saturating` modes
//! synthesise a multi-block CFG that raises `err: Overflow` or clamps the
//! value respectively (those two CFG-synthesising paths live in [`modes`]).
//! Split out of `expr/mod.rs` to keep both files under the 500-line cap.

mod modes;

use edda_syntax::ast::CastMode;
use edda_types::HirExpr;

use crate::error::{LoweringError, MirError};
use crate::operand::Operand;
use crate::place::Place;
use crate::rvalue::{Rvalue, RvalueKind};
use crate::ty::MirPrim;

use super::super::cfg::push_assign;
use super::super::ctx::LoweringContext;
use super::super::ty::{lower_ty, prim_tag, ty_to_prim};
use super::lower_expr_to_operand;

/// Lower an `expr as T [mode]` primitive cast.
pub(super) fn lower_cast(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    inner: &HirExpr,
    target_ty: edda_types::TyId,
    mode: CastMode,
) -> Operand {
    let src = lower_expr_to_operand(ctx, inner);
    let src_prim = ty_to_prim(ctx.ty_interner, inner.ty);
    let dst_prim = ty_to_prim(ctx.ty_interner, target_ty);
    let Some((sp, dp)) = src_prim.zip(dst_prim) else {
        ctx.errors.push(MirError::from(LoweringError::UnsupportedCast {
            from: src_prim.map(prim_tag).unwrap_or("<non-primitive>"),
            to: dst_prim.map(prim_tag).unwrap_or("<non-primitive>"),
            span: expr.span,
        }));
        return Operand::Unit;
    };
    let result_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, target_ty);

    // Cast modes only carry distinct semantics for narrowing integer-to-
    // integer casts (where the source value may not fit). Float casts,
    // int→float / float→int, and widening / same-width int→int reduce
    // to a plain `RvalueKind::Cast` regardless of mode — the LLVM
    // backend's truncate/extend already produces the wrapping result
    // and overflow can't occur on these paths.
    let narrowing_int = is_narrowing_int_cast(sp, dp);
    let effective_mode = if narrowing_int { mode } else { CastMode::Trap };

    match effective_mode {
        CastMode::Trap | CastMode::Wrapping => {
            let temp = match ctx.body.as_mut() {
                Some(body) => body.temp(result_ty.clone(), expr.span),
                None => return Operand::Unit,
            };
            let rvalue = Rvalue {
                span: expr.span,
                kind: RvalueKind::Cast {
                    src,
                    src_prim: sp,
                    dst_prim: dp,
                },
                ty: result_ty,
            };
            push_assign(ctx, expr.span, temp, rvalue);
            Operand::Move(Place::local(temp))
        }
        CastMode::Checked => modes::lower_checked_cast(ctx, expr.span, src, sp, dp, result_ty),
        CastMode::Saturating => modes::lower_saturating_cast(ctx, expr.span, src, sp, dp, result_ty),
    }
}

//            `dp`'s bit width is strictly less than `sp`'s (or the same width
//            with a sign change that could lose information). Conservatively
//            includes signed↔unsigned same-width as narrowing because the
//            highest bit's meaning flips
/// Decide whether a primitive cast can lose information.
///
/// For integer→integer casts: narrowing covers strict width reduction
/// and same-width sign changes (where a high bit can become a sign
/// flip). For every other pair the cast is total, and cast modes
/// reduce to the trapping default.
fn is_narrowing_int_cast(sp: MirPrim, dp: MirPrim) -> bool {
    let Some((sw, ss)) = int_width_signed(sp) else {
        return false;
    };
    let Some((dw, ds)) = int_width_signed(dp) else {
        return false;
    };
    if dw < sw {
        return true;
    }
    if dw == sw && ss != ds {
        return true;
    }
    false
}

/// `(width_in_bits, is_signed)` for integer primitive `prim`, or `None`
/// for non-integer prims. `Isize`/`Usize` resolve to 64-bit per the
/// v0.1 target set (all six target triples are 64-bit address spaces).
pub(super) fn int_width_signed(prim: MirPrim) -> Option<(u8, bool)> {
    Some(match prim {
        MirPrim::I8 => (8, true),
        MirPrim::I16 => (16, true),
        MirPrim::I32 => (32, true),
        MirPrim::I64 | MirPrim::Isize => (64, true),
        MirPrim::I128 => (128, true),
        MirPrim::U8 => (8, false),
        MirPrim::U16 => (16, false),
        MirPrim::U32 => (32, false),
        MirPrim::U64 | MirPrim::Usize => (64, false),
        MirPrim::U128 => (128, false),
        _ => return None,
    })
}
