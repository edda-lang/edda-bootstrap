//! Refinement-clause analysis — `requires` clauses → generator strategy.
//!
//! Recognises the currently-admitted integer-bound and bool-parameter
//! refinement forms and projects them onto a [`Strategy`]. Anything
//! outside the admitted fragment surfaces [`Strategy::Unanalyzable`].

use edda_intern::Symbol;
use edda_syntax::ast::{BinOp, Expr, ExprKind, Literal, RefinementClause, RefinementKind};

use crate::properties::strategy::Strategy;

// type or refinement combination the analyser does not yet
// admit — callers treat this as "skip"
/// Analyse a parameter's `requires` clauses and emit the matching
/// generator strategy. Integer parameters consume bounds from the
/// clauses; bool parameters always produce `BoolValue`; anything else
/// surfaces `Unanalyzable`.
///
/// `param_name` is the parameter's interned symbol — the analyser
/// matches single-segment Path expressions whose head equals this
/// symbol to identify which side of a comparison is the param.
/// `param_int_ty` carries the parameter's primitive integer type when
/// it is one; the produced range is clamped to that type's natural
/// `[MIN, MAX]` so the generator never emits values the call site
/// cannot accept. `is_bool_param` short-circuits to `BoolValue`.
/// `inline_refinement` carries the predicate from an inline
/// `param: T where pred` type refinement (`TypeKind::Refined`), when
/// present — it is folded into the same bound extraction as
/// `requires_clauses`.
pub fn analyse_param(
    param_name: Symbol,
    param_int_ty: Option<IntPrim>,
    is_bool_param: bool,
    requires_clauses: &[&RefinementClause],
    inline_refinement: Option<&Expr>,
) -> Strategy {
    if is_bool_param {
        return Strategy::BoolValue;
    }
    let Some(int_ty) = param_int_ty else {
        return Strategy::Unanalyzable;
    };
    let (ty_lo, ty_hi) = int_ty.range();
    let mut lo: i128 = ty_lo;
    let mut hi: i128 = ty_hi;
    let mut equality: Option<i128> = None;
    let mut any_clause_constrained = false;
    let mut apply_bounds = |bounds: Vec<Bound>| {
        for bound in bounds {
            any_clause_constrained = true;
            match bound {
                Bound::AtLeast(v) => lo = lo.max(v),
                Bound::AtMost(v) => hi = hi.min(v),
                Bound::Equal(v) => equality = Some(v),
            }
        }
    };
    for clause in requires_clauses {
        if clause.kind != RefinementKind::Requires {
            continue;
        }
        apply_bounds(extract_int_bounds(&clause.pred, param_name));
    }
    if let Some(pred) = inline_refinement {
        apply_bounds(extract_int_bounds(pred, param_name));
    }
    if let Some(v) = equality {
        // Equality must still fit in the declared type — otherwise the
        // user's `requires x == K` clashes with `x: T` and the runner
        // cannot honour the property anyway.
        if v < ty_lo || v > ty_hi {
            return Strategy::Unanalyzable;
        }
        return Strategy::Constant(v);
    }
    // Clamp to the parameter's natural type range. Even when the user
    // supplied bounds, the type narrows the abstract `i128` rendering
    // to the literal range the call site can actually accept. Without
    // this clamp, `x: i32` with `requires x >= 0` would generate up to
    // `i128::MAX` and panic at parse/typecheck.
    lo = lo.max(ty_lo);
    hi = hi.min(ty_hi);
    if !any_clause_constrained {
        // With no user-supplied bounds, narrow further to a tight
        // [-100, 100] (clipped to the type) so the runner generates
        // manageable interior values — boundary samples on a 64-bit
        // range are useless for any non-extreme function.
        lo = lo.max(-100);
        hi = hi.min(100);
    }
    if lo > hi {
        return Strategy::Unanalyzable;
    }
    Strategy::IntRange { lo, hi }
}

// match `crates/edda-types/src/prim.rs`. `usize`/`isize` are modelled
// as 64-bit per the v0.1 bootstrap targets (`docs/bootstrap/v0.1-scope.md`).
/// Edda integer primitive recognised by the property-test analyser.
/// Carries the bit-width-plus-signedness needed to clamp generated
/// inputs to the declared parameter type.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum IntPrim {
    I8,
    I16,
    I32,
    I64,
    I128,
    ISize,
    U8,
    U16,
    U32,
    U64,
    U128,
    USize,
}

impl IntPrim {
    // `i128` — `u128` is intentionally clipped at `i128::MAX` because
    // higher values are unrepresentable in the analyser's value model
    // ([`Value::Int(i128)`])
    /// Inclusive `[lo, hi]` natural range for this primitive, in
    /// `i128`. Used to clamp generated input ranges so the runner
    /// never emits values the call site cannot accept.
    fn range(self) -> (i128, i128) {
        match self {
            IntPrim::I8 => (i8::MIN as i128, i8::MAX as i128),
            IntPrim::I16 => (i16::MIN as i128, i16::MAX as i128),
            IntPrim::I32 => (i32::MIN as i128, i32::MAX as i128),
            IntPrim::I64 => (i64::MIN as i128, i64::MAX as i128),
            IntPrim::I128 => (i128::MIN, i128::MAX),
            IntPrim::ISize => (i64::MIN as i128, i64::MAX as i128),
            IntPrim::U8 => (0, u8::MAX as i128),
            IntPrim::U16 => (0, u16::MAX as i128),
            IntPrim::U32 => (0, u32::MAX as i128),
            IntPrim::U64 => (0, u64::MAX as i128),
            // `u128`'s upper bound exceeds `i128::MAX`; clip to
            // `i128::MAX` because the analyser's value model is `i128`.
            IntPrim::U128 => (0, i128::MAX),
            IntPrim::USize => (0, u64::MAX as i128),
        }
    }
}

enum Bound {
    AtLeast(i128),
    AtMost(i128),
    Equal(i128),
}

//   `param OP literal` or `literal OP param` where `param` is a
//   single-segment Path whose head matches `param_name` and the
//   other side is an integer literal; deeper expressions fall
//   through without contributing a bound
//   both bounds; `||` is not (disjunctive predicates are not yet
//   handled and fall through to the Z3 fallback)
/// Walk a refinement predicate looking for direct integer bounds on
/// `param_name`. Returns every bound the predicate establishes — a
/// single conjunction can yield multiple bounds.
fn extract_int_bounds(pred: &Expr, param_name: Symbol) -> Vec<Bound> {
    let mut out = Vec::new();
    collect_bounds(pred, param_name, &mut out);
    out
}

fn collect_bounds(expr: &Expr, param: Symbol, out: &mut Vec<Bound>) {
    match &expr.kind {
        ExprKind::Binary { op, lhs, rhs } => {
            match op {
                BinOp::And => {
                    collect_bounds(lhs, param, out);
                    collect_bounds(rhs, param, out);
                }
                BinOp::Eq | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    if let Some(bound) = comparison_to_bound(*op, lhs, rhs, param) {
                        out.push(bound);
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }
}

//   param sits on the RHS the comparison is flipped (`5 <= x`
//   becomes `x >= 5` i.e. `AtLeast(5)`)
fn comparison_to_bound(op: BinOp, lhs: &Expr, rhs: &Expr, param: Symbol) -> Option<Bound> {
    let (param_side_left, other) = if is_param_path(lhs, param) {
        (true, rhs)
    } else if is_param_path(rhs, param) {
        (false, lhs)
    } else {
        return None;
    };
    let value = literal_int(other)?;
    let effective_op = if param_side_left {
        op
    } else {
        flip_comparison(op)
    };
    Some(match effective_op {
        BinOp::Eq => Bound::Equal(value),
        BinOp::Lt => Bound::AtMost(value.saturating_sub(1)),
        BinOp::Le => Bound::AtMost(value),
        BinOp::Gt => Bound::AtLeast(value.saturating_add(1)),
        BinOp::Ge => Bound::AtLeast(value),
        _ => return None,
    })
}

fn flip_comparison(op: BinOp) -> BinOp {
    match op {
        BinOp::Lt => BinOp::Gt,
        BinOp::Le => BinOp::Ge,
        BinOp::Gt => BinOp::Lt,
        BinOp::Ge => BinOp::Le,
        BinOp::Eq => BinOp::Eq,
        other => other,
    }
}

fn is_param_path(expr: &Expr, param: Symbol) -> bool {
    if let ExprKind::Path(path) = &expr.kind
        && path.segments.len() == 1
        && path.segments[0].name == param
    {
        return true;
    }
    false
}

fn literal_int(expr: &Expr) -> Option<i128> {
    match &expr.kind {
        ExprKind::Literal(Literal::Int { value, .. }) => i128::try_from(*value).ok(),
        ExprKind::Unary {
            op: edda_syntax::ast::UnOp::Neg,
            expr: inner,
        } => literal_int(inner).map(|v| v.checked_neg().unwrap_or(i128::MIN)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::properties::strategy::generate_values;
    use crate::properties::value::Value;
    use edda_intern::Interner;
    use edda_span::Span;
    use edda_syntax::IntBase;
    use edda_syntax::ast::{Ident, Path};

    fn lit(value: u128) -> Expr {
        Expr {
            span: Span::DUMMY,
            kind: ExprKind::Literal(Literal::Int {
                value,
                base: IntBase::Dec,
            }),
        }
    }

    fn neg_lit(value: u128) -> Expr {
        Expr {
            span: Span::DUMMY,
            kind: ExprKind::Unary {
                op: edda_syntax::ast::UnOp::Neg,
                expr: Box::new(lit(value)),
            },
        }
    }

    fn path(interner: &Interner, name: &str) -> Expr {
        let sym = interner.intern(name);
        Expr {
            span: Span::DUMMY,
            kind: ExprKind::Path(Path {
                segments: vec![Ident {
                    name: sym,
                    span: Span::DUMMY,
                }],
                span: Span::DUMMY,
            }),
        }
    }

    fn binop(op: BinOp, lhs: Expr, rhs: Expr) -> Expr {
        Expr {
            span: Span::DUMMY,
            kind: ExprKind::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
        }
    }

    fn clause(kind: RefinementKind, pred: Expr) -> RefinementClause {
        RefinementClause {
            span: Span::DUMMY,
            kind,
            pred,
        }
    }

    #[test]
    fn boolean_param_yields_two_values() {
        let interner = Interner::new();
        let sym = interner.intern("flag");
        let strategy = analyse_param(sym, None, true, &[], None);
        assert_eq!(strategy, Strategy::BoolValue);
        let values = generate_values(&strategy, 10);
        assert_eq!(values.len(), 2);
        assert!(values.contains(&Value::Bool(false)));
        assert!(values.contains(&Value::Bool(true)));
    }

    #[test]
    fn non_integer_non_bool_is_unanalyzable() {
        let interner = Interner::new();
        let sym = interner.intern("xs");
        let strategy = analyse_param(sym, None, false, &[], None);
        assert_eq!(strategy, Strategy::Unanalyzable);
        assert!(generate_values(&strategy, 100).is_empty());
    }

    #[test]
    fn unconstrained_int_defaults_to_small_range() {
        let interner = Interner::new();
        let sym = interner.intern("n");
        let strategy = analyse_param(sym, Some(IntPrim::I64), false, &[], None);
        assert_eq!(strategy, Strategy::IntRange { lo: -100, hi: 100 });
    }

    #[test]
    fn positive_int_requires_yields_bounded_range_at_least_one() {
        // The third success criterion's literal shape:
        // `requires n > 0` for an i64 param.
        let interner = Interner::new();
        let sym = interner.intern("n");
        let pred = binop(BinOp::Gt, path(&interner, "n"), lit(0));
        let c = clause(RefinementKind::Requires, pred);
        let strategy = analyse_param(sym, Some(IntPrim::I64), false, &[&c], None);
        match strategy {
            Strategy::IntRange { lo, hi } => {
                assert_eq!(lo, 1, "n > 0 means lo == 1 (inclusive lower bound)");
                assert!(hi >= 1);
            }
            other => panic!("expected IntRange, got {other:?}"),
        }
    }

    #[test]
    fn equality_requires_pins_to_constant() {
        let interner = Interner::new();
        let sym = interner.intern("n");
        let pred = binop(BinOp::Eq, path(&interner, "n"), lit(42));
        let c = clause(RefinementKind::Requires, pred);
        let strategy = analyse_param(sym, Some(IntPrim::I64), false, &[&c], None);
        assert_eq!(strategy, Strategy::Constant(42));
        let values = generate_values(&strategy, 5);
        assert!(values.iter().all(|v| matches!(v, Value::Int(42))));
    }

    #[test]
    fn conjunction_of_bounds_combines_to_range() {
        // `requires n >= 0 && n <= 10` → IntRange { lo: 0, hi: 10 }.
        let interner = Interner::new();
        let sym = interner.intern("n");
        let lower = binop(BinOp::Ge, path(&interner, "n"), lit(0));
        let upper = binop(BinOp::Le, path(&interner, "n"), lit(10));
        let conj = binop(BinOp::And, lower, upper);
        let c = clause(RefinementKind::Requires, conj);
        let strategy = analyse_param(sym, Some(IntPrim::I64), false, &[&c], None);
        assert_eq!(strategy, Strategy::IntRange { lo: 0, hi: 10 });
    }

    #[test]
    fn negative_literal_in_lower_bound_parses() {
        let interner = Interner::new();
        let sym = interner.intern("n");
        // `n >= -5`
        let pred = binop(BinOp::Ge, path(&interner, "n"), neg_lit(5));
        let c = clause(RefinementKind::Requires, pred);
        let strategy = analyse_param(sym, Some(IntPrim::I64), false, &[&c], None);
        match strategy {
            Strategy::IntRange { lo, hi } => {
                assert_eq!(lo, -5);
                assert!(hi >= -5);
            }
            other => panic!("expected IntRange, got {other:?}"),
        }
    }

    #[test]
    fn flipped_comparison_with_param_on_rhs_works() {
        // `5 <= n` should yield `lo == 5`.
        let interner = Interner::new();
        let sym = interner.intern("n");
        let pred = binop(BinOp::Le, lit(5), path(&interner, "n"));
        let c = clause(RefinementKind::Requires, pred);
        let strategy = analyse_param(sym, Some(IntPrim::I64), false, &[&c], None);
        match strategy {
            Strategy::IntRange { lo, hi } => {
                assert_eq!(lo, 5);
                assert!(hi >= 5);
            }
            other => panic!("expected IntRange, got {other:?}"),
        }
    }

    #[test]
    fn i32_param_with_lower_bound_clamps_upper_to_i32_max() {
        // The B23 repro shape — `x: i32` with `requires x >= 0`.
        // Without clamping the upper would default to `i128::MAX`
        // and the generated call site overflows the param type.
        let interner = Interner::new();
        let sym = interner.intern("x");
        let pred = binop(BinOp::Ge, path(&interner, "x"), lit(0));
        let c = clause(RefinementKind::Requires, pred);
        let strategy = analyse_param(sym, Some(IntPrim::I32), false, &[&c], None);
        match strategy {
            Strategy::IntRange { lo, hi } => {
                assert_eq!(lo, 0);
                assert_eq!(
                    hi,
                    i32::MAX as i128,
                    "upper bound must clamp to i32::MAX, not i128::MAX"
                );
            }
            other => panic!("expected IntRange, got {other:?}"),
        }
    }

    #[test]
    fn u8_param_unconstrained_clamps_to_unsigned_byte() {
        let interner = Interner::new();
        let sym = interner.intern("b");
        let strategy = analyse_param(sym, Some(IntPrim::U8), false, &[], None);
        // Default narrow [-100, 100] intersected with [0, 255] = [0, 100].
        assert_eq!(strategy, Strategy::IntRange { lo: 0, hi: 100 });
    }

    #[test]
    fn inline_where_refinement_yields_bounded_range() {
        // `kind_tag_in: u8 where kind_tag_in < 5` carries
        // its bound on the parameter's own type (`TypeKind::Refined`),
        // not in `fn_decl.refinements`. The caller now feeds that
        // predicate through as `inline_refinement` instead of leaving
        // the param `Unanalyzable`.
        let interner = Interner::new();
        let sym = interner.intern("kind_tag_in");
        let pred = binop(BinOp::Lt, path(&interner, "kind_tag_in"), lit(5));
        let strategy = analyse_param(sym, Some(IntPrim::U8), false, &[], Some(&pred));
        assert_eq!(strategy, Strategy::IntRange { lo: 0, hi: 4 });
    }
}
