//! Comptime `Type`-valued argument evaluation for inbound comptime
//! type generics (`field_type_at` as a
//! value at template call sites).
//!
//! The codex `04-specs-comptime.md` §4.5 canonical deserialize form
//! passes a comptime type expression positionally:
//!
//! ```text
//! out.(i) = decode_field(field_type_at(T, i), bytes, offset_of(T, i))
//! ```
//!
//! By the time the mono rewrite walks a caller body, the D-22
//! expansion has already unrolled the enclosing `comptime for`, so the
//! index argument is an integer literal and the receiver is a concrete
//! type path. This module evaluates that argument shape to the
//! `(leaf, span)` bound-type reference the substitution walker resolves
//! through the span-keyed `Resolutions` map — the same reference form
//! `member_ty_spans` was added to the [`ShapeIndex`] for.

use edda_intern::Interner;
use edda_syntax::ast::{Expr, ExprKind, Literal};

use super::{member_bound_ty, BoundTy};
use crate::comptime_expand::ShapeIndex;

/// Evaluate a call argument as a comptime `Type` value, yielding the
/// [`BoundTy`] reference of the concrete type it names.
pub(super) fn comptime_type_of_expr(
    e: &Expr,
    shapes: &ShapeIndex,
    interner: &Interner,
) -> Option<BoundTy> {
    match &e.kind {
        ExprKind::Path(p) => p.segments.last().map(|s| BoundTy::Named(s.name, p.span)),
        // Tuple-composite sentinel `(A, B, …)` / `()` — spliced by the
        // substitution walker in place of a structural bound generic.
        // Its elements are plain type
        // paths; an empty tuple is the unit composite.
        ExprKind::Tuple(elems) => tuple_sentinel_bound(elems),
        // Slice-composite sentinel `[E]` — spliced by the substitution
        // walker in place of a slice bound generic.
        // One plain type-path element.
        ExprKind::Array(elems) => slice_sentinel_bound(elems),
        ExprKind::Call { callee, args } => {
            let ExprKind::Path(cp) = &callee.kind else {
                return None;
            };
            let name = cp.segments.last()?.name;
            if interner.resolve(name) != "field_type_at" || args.len() != 2 {
                return None;
            }
            let recv = comptime_type_of_expr(&args[0].expr, shapes, interner)?;
            let index = const_index(&args[1].expr)?;
            match recv {
                // `field_type_at(NamedSum, k)` → variant `k`'s payload
                // composite (unit / tuple), the D-22 convention.
                BoundTy::Named(recv_leaf, _) => {
                    member_bound_ty(shapes.get(&recv_leaf)?.member_tys.get(index)?)
                }
                // `field_type_at((A, B, …), k)` → element `k` (a plain
                // named reference); the walk inside a tuple-U specialization.
                BoundTy::Tuple(refs) => {
                    let (leaf, span) = refs.get(index)?;
                    Some(BoundTy::Named(*leaf, *span))
                }
                // `field_type_at((), k)` is out of range for any `k`; a
                // slice `[E]` has no positional comptime fields either.
                BoundTy::Unit | BoundTy::Slice(..) => None,
            }
        }
        _ => None,
    }
}

/// Interpret a tuple-composite sentinel expression's elements as a
/// [`BoundTy`]: an empty tuple is [`BoundTy::Unit`]; a non-empty tuple
/// of plain type paths is [`BoundTy::Tuple`]. Any non-path element makes
/// it undecidable.
fn tuple_sentinel_bound(elems: &[Expr]) -> Option<BoundTy> {
    if elems.is_empty() {
        return Some(BoundTy::Unit);
    }
    let mut refs = Vec::with_capacity(elems.len());
    for el in elems {
        let ExprKind::Path(p) = &el.kind else {
            return None;
        };
        let seg = p.segments.last()?;
        refs.push((seg.name, p.span));
    }
    Some(BoundTy::Tuple(refs))
}

/// Interpret a slice-composite sentinel `[E]` as a [`BoundTy::Slice`]:
/// exactly one plain type-path element. Any other shape is undecidable.
fn slice_sentinel_bound(elems: &[Expr]) -> Option<BoundTy> {
    let [el] = elems else {
        return None;
    };
    let ExprKind::Path(p) = &el.kind else {
        return None;
    };
    let seg = p.segments.last()?;
    Some(BoundTy::Slice(seg.name, p.span))
}

/// Constant `usize` index: a non-negative integer literal (the shape
/// the D-22 unroll leaves behind for the loop variable).
fn const_index(e: &Expr) -> Option<usize> {
    match &e.kind {
        ExprKind::Literal(Literal::Int { value, .. }) => usize::try_from(*value).ok(),
        _ => None,
    }
}
