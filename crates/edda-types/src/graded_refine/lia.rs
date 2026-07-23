//! Integer-sorted predicate helpers and the `for`-loop iteration-count
//! derivation for §5.4 graded-bound cost accumulation.
//!
//! All cost terms stay inside the locked LIA fragment per
//! `refinement-decidability.md` §4: literals fold eagerly, `max` lowers to
//! `if a >= b then a else b`, and multiplication requires one literal operand.

use edda_refine::{
    CmpOp, IntLit, IntLitValue, IntSort, Predicate, Sort, lift_predicate,
};
use edda_syntax::ast::{Expr, ExprKind, RangeKind};

use super::GradedCx;

/// Per-iteration count for a `for x in <iter>` loop, expressed as an
/// integer-sorted [`Predicate`].
///
/// - `Range { lo, hi, kind = HalfOpen }` → `hi - lo` (with `lo` defaulting
///   to `0` when omitted; an omitted `hi` is unbounded → bail).
/// - `Range { lo, hi, kind = Closed }`   → `hi - lo + 1`.
/// - Other iter shapes: lift through `lift_predicate`; if the result has
///   slice sort, lift to `Predicate::slice_len`. Otherwise bail.
pub(super) fn for_iter_count(iter: &Expr, cx: &GradedCx<'_>) -> Option<Predicate> {
    if let ExprKind::Range { lo, hi, kind } = &iter.kind {
        let hi = hi.as_ref()?;
        let hi_p = lift_predicate(hi, cx.env).ok()?;
        let lo_p = match lo {
            Some(l) => lift_predicate(l, cx.env).ok()?,
            None => zero(cx.sort),
        };
        let span = Predicate::sub(hi_p, lo_p);
        return Some(match kind {
            RangeKind::HalfOpen => span,
            RangeKind::Closed => add_costs(span, one(cx.sort)),
        });
    }
    let lifted = lift_predicate(iter, cx.env).ok()?;
    match lifted.sort() {
        Sort::Slice(_) => Some(Predicate::slice_len(lifted)),
        _ => None,
    }
}

/// Best-effort slice sort for a parameter whose element type is outside
/// the LIA fragment (`paths: [String]`, `bufs: [Buffer]`, ...). The
/// LIA-required `.len()` lift only cares that the receiver is `Sort::Slice(_)`;
/// the element sort never participates in arithmetic. Falling back to a
/// `usize`-element slice keeps the graded-bound lift working for the
/// canonical spec example (`alloc(bytes <= 4096 * paths.len())`) without
/// extending the predicate fragment.
pub(super) fn slice_placeholder_sort(
    ty: crate::ty::TyId,
    ty_interner: &crate::ty::TyInterner,
) -> Option<Sort> {
    match ty_interner.kind(ty) {
        crate::ty::TyKind::Slice(_) => Some(Sort::slice(Sort::usize())),
        _ => None,
    }
}

/// Integer literal `0` at the given sort.
pub(super) fn zero(sort: IntSort) -> Predicate {
    Predicate::IntLit(IntLit::signed(0, sort))
}

/// Integer literal `1` at the given sort.
pub(super) fn one(sort: IntSort) -> Predicate {
    Predicate::IntLit(IntLit::signed(1, sort))
}

/// `a + b` with zero-elision and constant folding so the accumulator's
/// intermediate trees don't pollute the LIA fragment. `0 + x ⇒ x`,
/// `IntLit(a) + IntLit(b) ⇒ IntLit(a+b)`, everything else stays as a
/// regular [`Predicate::Arith`] node. This keeps every cost term
/// emitted from the loop-lift step inside the "one operand must be a
/// literal" rule [`mul_predicate`] enforces.
pub(super) fn add_costs(a: Predicate, b: Predicate) -> Predicate {
    if let Predicate::IntLit(la) = &a
        && lit_is_zero(la)
    {
        return b;
    }
    if let Predicate::IntLit(lb) = &b
        && lit_is_zero(lb)
    {
        return a;
    }
    if let (Predicate::IntLit(la), Predicate::IntLit(lb)) = (&a, &b)
        && let (Some(av), Some(bv)) = (lit_as_i128(*la), lit_as_i128(*lb))
        && let Some(sum) = av.checked_add(bv)
    {
        return Predicate::IntLit(IntLit::signed(sum, la.sort()));
    }
    Predicate::add(a, b)
}

/// `true` when the literal's value is zero (signed or unsigned variant).
fn lit_is_zero(lit: &IntLit) -> bool {
    matches!(
        lit.value(),
        IntLitValue::Signed(0) | IntLitValue::Unsigned(0)
    )
}

/// `max(a, b)` encoded as `if a >= b then a else b`. Z3 LIA decides ite
/// trivially; this keeps cost expressions inside the locked LIA fragment
/// per `refinement-decidability.md` §4.
pub(super) fn predicate_max(a: Predicate, b: Predicate) -> Predicate {
    let cond = Predicate::cmp(CmpOp::Ge, a.clone(), b.clone());
    Predicate::if_then_else(cond, a, b)
}

/// `a * b` when one operand is an integer literal. Per the LIA
/// literal-constant rule, multiplication of two non-literal predicates
/// is undecidable; this returns `None` so the caller bails to silent
/// admission rather than emitting a noisy diagnostic on an out-of-LIA
/// shape.
///
/// Constant folding: `IntLit(c1) * IntLit(c2)` collapses to
/// `IntLit(c1*c2)` at the LHS's sort.
pub(super) fn mul_predicate(a: Predicate, b: Predicate) -> Option<Predicate> {
    if let (Predicate::IntLit(la), Predicate::IntLit(lb)) = (&a, &b) {
        return mul_lits(*la, *lb);
    }
    if let Predicate::IntLit(lit) = a {
        return Some(Predicate::mul_lit(lit, b));
    }
    if let Predicate::IntLit(lit) = b {
        return Some(Predicate::mul_lit(lit, a));
    }
    None
}

/// Constant-fold `c1 * c2` into one signed integer literal, keeping the
/// LHS sort. Returns `None` if the product overflows `i128`.
fn mul_lits(la: IntLit, lb: IntLit) -> Option<Predicate> {
    let a = lit_as_i128(la)?;
    let b = lit_as_i128(lb)?;
    let p = a.checked_mul(b)?;
    Some(Predicate::IntLit(IntLit::signed(p, la.sort())))
}

/// Project an `IntLit` to `i128` regardless of signedness tag. Returns
/// `None` for unsigned values too large to fit in `i128`.
fn lit_as_i128(lit: IntLit) -> Option<i128> {
    match lit.value() {
        IntLitValue::Signed(v) => Some(v),
        IntLitValue::Unsigned(v) => i128::try_from(v).ok(),
    }
}
