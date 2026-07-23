//! Tests for the binary / unary operator inference helpers.

use super::*;
use crate::cx::TyCx;
use crate::lower::LowerCx;
use crate::lower::lower_expr;
use crate::test_support::Harness;
use edda_syntax::ast::{Expr, ExprKind, Literal};

fn lit_int(value: u128) -> Expr {
    Expr {
        span: Span::DUMMY,
        kind: ExprKind::Literal(Literal::Int {
            value,
            base: edda_syntax::IntBase::Dec,
        }),
    }
}

fn lit_bool(b: bool) -> Expr {
    Expr {
        span: Span::DUMMY,
        kind: ExprKind::Literal(Literal::Bool(b)),
    }
}

fn lower_and_synth(h: &mut Harness, ast: &Expr) -> TyId {
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut hir = lower_expr(ast, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    synth_expr(&mut env, &mut hir, &mut ic)
}

fn binary(op: BinOp, lhs: Expr, rhs: Expr) -> Expr {
    Expr {
        span: Span::DUMMY,
        kind: ExprKind::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        },
    }
}

fn unary(op: UnOp, expr: Expr) -> Expr {
    Expr {
        span: Span::DUMMY,
        kind: ExprKind::Unary {
            op,
            expr: Box::new(expr),
        },
    }
}

#[test]
fn integer_addition_synthesises_i64() {
    let mut h = Harness::new();
    let ty = lower_and_synth(&mut h, &binary(BinOp::Add, lit_int(1), lit_int(2)));
    assert_eq!(ty, h.ty_interner.prim(Primitive::I64));
    assert!(h.diags.is_empty());
}

#[test]
fn arithmetic_on_bool_emits_diagnostic() {
    let mut h = Harness::new();
    let ty = lower_and_synth(&mut h, &binary(BinOp::Add, lit_bool(true), lit_bool(false)));
    assert_eq!(ty, h.ty_interner.error());
    assert!(h.diags.error_count() >= 1);
    assert!(
        h.diags
            .iter()
            .any(|d| d.message.contains("requires numeric operands"))
    );
}

#[test]
fn comparison_returns_bool() {
    let mut h = Harness::new();
    for op in [
        BinOp::Eq,
        BinOp::Ne,
        BinOp::Lt,
        BinOp::Le,
        BinOp::Gt,
        BinOp::Ge,
    ] {
        let ty = lower_and_synth(&mut h, &binary(op, lit_int(1), lit_int(2)));
        assert_eq!(ty, h.ty_interner.prim(Primitive::Bool), "{op:?}");
    }
}

#[test]
fn ordering_on_bool_emits_diagnostic_but_yields_bool() {
    let mut h = Harness::new();
    let ty = lower_and_synth(&mut h, &binary(BinOp::Lt, lit_bool(true), lit_bool(false)));
    // Result type is still `bool` (only the operand mismatch is the bug).
    assert_eq!(ty, h.ty_interner.prim(Primitive::Bool));
    assert!(
        h.diags
            .iter()
            .any(|d| d.message.contains("ordering operator"))
    );
}

#[test]
fn equality_on_bool_allowed() {
    let mut h = Harness::new();
    let ty = lower_and_synth(&mut h, &binary(BinOp::Eq, lit_bool(true), lit_bool(false)));
    assert_eq!(ty, h.ty_interner.prim(Primitive::Bool));
    assert!(h.diags.is_empty(), "Eq does not require numeric");
}

#[test]
fn logical_and_or_require_bool() {
    let mut h = Harness::new();
    let ty = lower_and_synth(&mut h, &binary(BinOp::And, lit_bool(true), lit_bool(false)));
    assert_eq!(ty, h.ty_interner.prim(Primitive::Bool));
    assert!(h.diags.is_empty());
}

#[test]
fn logical_on_int_emits_diagnostic() {
    let mut h = Harness::new();
    lower_and_synth(&mut h, &binary(BinOp::And, lit_int(1), lit_int(2)));
    // Two diagnostics — one per operand (check_expr emits per-operand on mismatch).
    assert!(h.diags.error_count() >= 1);
}

#[test]
fn bitwise_on_int_synthesises_lhs_type() {
    let mut h = Harness::new();
    let ty = lower_and_synth(&mut h, &binary(BinOp::BitAnd, lit_int(0xFF), lit_int(0x0F)));
    assert_eq!(ty, h.ty_interner.prim(Primitive::I64));
    assert!(h.diags.is_empty());
}

#[test]
fn bitwise_on_bool_emits_diagnostic() {
    let mut h = Harness::new();
    let ty = lower_and_synth(
        &mut h,
        &binary(BinOp::BitAnd, lit_bool(true), lit_bool(false)),
    );
    assert_eq!(ty, h.ty_interner.error());
    assert!(
        h.diags
            .iter()
            .any(|d| d.message.contains("requires integer operands"))
    );
}

#[test]
fn shift_synthesises_lhs_type() {
    let mut h = Harness::new();
    let ty = lower_and_synth(&mut h, &binary(BinOp::Shl, lit_int(1), lit_int(4)));
    assert_eq!(ty, h.ty_interner.prim(Primitive::I64));
    assert!(h.diags.is_empty());
}

#[test]
fn unary_neg_preserves_numeric_type() {
    let mut h = Harness::new();
    let ty = lower_and_synth(&mut h, &unary(UnOp::Neg, lit_int(5)));
    assert_eq!(ty, h.ty_interner.prim(Primitive::I64));
    assert!(h.diags.is_empty());
}

#[test]
fn unary_not_synthesises_bool() {
    let mut h = Harness::new();
    let ty = lower_and_synth(&mut h, &unary(UnOp::Not, lit_bool(true)));
    assert_eq!(ty, h.ty_interner.prim(Primitive::Bool));
    assert!(h.diags.is_empty());
}

#[test]
fn unary_bitnot_preserves_integer_type() {
    let mut h = Harness::new();
    let ty = lower_and_synth(&mut h, &unary(UnOp::BitNot, lit_int(0xFF)));
    assert_eq!(ty, h.ty_interner.prim(Primitive::I64));
    assert!(h.diags.is_empty());
}

#[test]
fn unary_neg_on_bool_emits_diagnostic() {
    let mut h = Harness::new();
    let ty = lower_and_synth(&mut h, &unary(UnOp::Neg, lit_bool(true)));
    assert_eq!(ty, h.ty_interner.error());
    assert!(h.diags.iter().any(|d| d.message.contains("unary `-`")));
}

#[test]
fn unary_not_on_int_emits_check_mismatch() {
    let mut h = Harness::new();
    // `!42` checks the int against `bool` and fails.
    let ty = lower_and_synth(&mut h, &unary(UnOp::Not, lit_int(42)));
    // The Not branch returns bool regardless (the operand is the
    // failing site).
    assert_eq!(ty, h.ty_interner.prim(Primitive::Bool));
    assert!(h.diags.error_count() >= 1);
}

#[test]
fn nested_arith_round_trips() {
    // `(1 + 2) * 3` → i64
    let mut h = Harness::new();
    let inner = binary(BinOp::Add, lit_int(1), lit_int(2));
    let outer = binary(BinOp::Mul, inner, lit_int(3));
    let ty = lower_and_synth(&mut h, &outer);
    assert_eq!(ty, h.ty_interner.prim(Primitive::I64));
    assert!(h.diags.is_empty());
}

#[test]
fn cascade_error_does_not_double_diagnose() {
    // `(true + false) + 1` — the inner Add emits one diagnostic,
    // the outer cascade should not re-emit.
    let mut h = Harness::new();
    let inner = binary(BinOp::Add, lit_bool(true), lit_bool(false));
    let outer = binary(BinOp::Add, inner, lit_int(1));
    let ty = lower_and_synth(&mut h, &outer);
    assert_eq!(ty, h.ty_interner.error());
    // Exactly one diagnostic — from the inner failure.
    assert_eq!(h.diags.error_count(), 1);
}

/// Regression for the "non-lowerable-local of type Never"
/// emitter error. `1 + 2 * 3 ⇐ i32` must narrow every literal to
/// `i32` and the binary expression's `ty` must be `i32` — without
/// this, the MIR emitter rejects the body-temp as having type
/// `Never` because [`super::check_expr`] used to fall through to
/// `_ => ic.ty_error()` for every non-Literal/Path/Block form.
#[test]
fn nested_arith_checks_against_i32_narrows_every_literal() {
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    // (1 + 2 * 3) ⇐ i32
    let inner = binary(BinOp::Mul, lit_int(2), lit_int(3));
    let outer = binary(BinOp::Add, lit_int(1), inner);
    let mut hir = lower_expr(&outer, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let i32_id = h.ty_interner.prim(Primitive::I32);
    super::check_expr(
        &mut env,
        &mut hir,
        i32_id,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert!(h.diags.is_empty(), "no diagnostics: {:?}", h.diags.iter().map(|d| &d.message).collect::<Vec<_>>());
    assert_eq!(hir.ty, i32_id);
    // Verify the leaf-literal narrowing reached every leaf — the
    // MIR emitter inspects every operand's `ty` field for the
    // `non-lowerable-local` guard.
    fn assert_ty_recursive(e: &HirExpr, expected: TyId) {
        match &e.kind {
            crate::hir::HirExprKind::Binary { lhs, rhs, .. } => {
                assert_eq!(e.ty, expected, "outer binary ty mismatch");
                assert_ty_recursive(lhs, expected);
                assert_ty_recursive(rhs, expected);
            }
            crate::hir::HirExprKind::Literal(_) => {
                assert_eq!(e.ty, expected, "literal ty mismatch: {:?}", e);
            }
            other => panic!("unexpected shape in nested arith: {other:?}"),
        }
    }
    assert_ty_recursive(&hir, i32_id);
}

/// Regression: `check_binary` for bitwise / shift operators
/// propagates the expected integer type into both operands.
#[test]
fn nested_bitwise_checks_against_u32_narrows() {
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    // (1 | 2) ⇐ u32
    let outer = binary(BinOp::BitOr, lit_int(1), lit_int(2));
    let mut hir = lower_expr(&outer, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let u32_id = h.ty_interner.prim(Primitive::U32);
    super::check_expr(
        &mut env,
        &mut hir,
        u32_id,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert!(h.diags.is_empty());
    assert_eq!(hir.ty, u32_id);
}

/// Regression: `check_unary` for `Neg` propagates the expected
/// numeric type into the operand.
#[test]
fn unary_neg_checks_against_i32_narrows_operand() {
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let neg = unary(UnOp::Neg, lit_int(7));
    let mut hir = lower_expr(&neg, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let i32_id = h.ty_interner.prim(Primitive::I32);
    super::check_expr(
        &mut env,
        &mut hir,
        i32_id,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert!(h.diags.is_empty());
    assert_eq!(hir.ty, i32_id);
}
