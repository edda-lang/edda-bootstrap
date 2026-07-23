//! Tests for the control-flow synth helpers.

use super::super::{InferCx, TyEnv, check_expr, synth_expr};
use edda_span::Span;
use crate::cx::TyCx;
use crate::prim::Primitive;
use crate::ty::TyId;
use crate::lower::LowerCx;
use crate::lower::lower_expr;
use crate::test_support::{Harness, ident_for};
use edda_syntax::ast::{BinOp, Expr, ExprKind, Literal, MatchArm, Pat, PatKind};

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

fn lit_unit() -> Expr {
    Expr {
        span: Span::DUMMY,
        kind: ExprKind::Literal(Literal::Unit),
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

fn if_expr(cond: Expr, then: Expr, else_branch: Option<Expr>) -> Expr {
    Expr {
        span: Span::DUMMY,
        kind: ExprKind::If {
            cond: Box::new(cond),
            then_block: edda_syntax::ast::Block {
                span: Span::DUMMY,
                stmts: vec![],
                trailing: Some(Box::new(then)),
            },
            else_branch: else_branch.map(Box::new),
        },
    }
}

#[test]
fn if_with_matching_branches_synthesises_branch_type() {
    let mut h = Harness::new();
    let e = if_expr(lit_bool(true), lit_int(1), Some(lit_int(2)));
    let ty = lower_and_synth(&mut h, &e);
    assert_eq!(ty, h.ty_interner.prim(Primitive::I64));
    assert!(h.diags.is_empty());
}

#[test]
fn if_without_else_synthesises_unit_when_then_is_unit() {
    let mut h = Harness::new();
    let e = if_expr(lit_bool(true), lit_unit(), None);
    let ty = lower_and_synth(&mut h, &e);
    assert_eq!(ty, h.ty_interner.prim(Primitive::Unit));
    assert!(h.diags.is_empty());
}

#[test]
fn if_without_else_diagnoses_non_unit_then_branch() {
    let mut h = Harness::new();
    let e = if_expr(lit_bool(true), lit_int(1), None);
    let ty = lower_and_synth(&mut h, &e);
    // Then = i64, else = (); branches disagree.
    assert_eq!(ty, h.ty_interner.error());
    assert!(
        h.diags
            .iter()
            .any(|d| d.message.contains("incompatible types"))
    );
}

#[test]
fn if_cond_non_bool_emits_check_error() {
    let mut h = Harness::new();
    let e = if_expr(lit_int(1), lit_int(2), Some(lit_int(3)));
    let ty = lower_and_synth(&mut h, &e);
    // Branches agree on i64, so synth still yields i64.
    assert_eq!(ty, h.ty_interner.prim(Primitive::I64));
    // The cond mismatch produced a diagnostic.
    assert!(
        h.diags
            .iter()
            .any(|d| d.message.contains("expected `bool`"))
    );
}

#[test]
fn if_branch_diverges_absorbs_into_other() {
    // `if cond { 1 } else { panic "boom" }` → i64 (else is never)
    let mut h = Harness::new();
    let panic_str = h.interner.intern("boom");
    let panic_expr = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Panic(Box::new(Expr {
            span: Span::DUMMY,
            kind: ExprKind::Literal(Literal::Str(panic_str)),
        })),
    };
    let e = if_expr(lit_bool(true), lit_int(1), Some(panic_expr));
    let ty = lower_and_synth(&mut h, &e);
    assert_eq!(ty, h.ty_interner.prim(Primitive::I64));
    assert!(h.diags.is_empty());
}

#[test]
fn match_single_arm_synthesises_body_type() {
    let mut h = Harness::new();
    let arm = MatchArm {
        span: Span::DUMMY,
        pat: Pat {
            span: Span::DUMMY,
            kind: PatKind::Wildcard,
        },
        guard: None,
        body: lit_int(7),
    };
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Match {
            scrutinee: Box::new(lit_int(0)),
            arms: vec![arm],
        },
    };
    let ty = lower_and_synth(&mut h, &e);
    assert_eq!(ty, h.ty_interner.prim(Primitive::I64));
    assert!(h.diags.is_empty());
}

#[test]
fn match_arms_must_agree_on_body_type() {
    let mut h = Harness::new();
    let arms = vec![
        MatchArm {
            span: Span::DUMMY,
            pat: Pat {
                span: Span::DUMMY,
                kind: PatKind::Wildcard,
            },
            guard: None,
            body: lit_int(1),
        },
        MatchArm {
            span: Span::DUMMY,
            pat: Pat {
                span: Span::DUMMY,
                kind: PatKind::Wildcard,
            },
            guard: None,
            body: lit_bool(true),
        },
    ];
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Match {
            scrutinee: Box::new(lit_int(0)),
            arms,
        },
    };
    let ty = lower_and_synth(&mut h, &e);
    assert_eq!(ty, h.ty_interner.error());
    assert!(
        h.diags
            .iter()
            .any(|d| d.message.contains("incompatible types"))
    );
}

#[test]
fn match_binding_pattern_brings_name_into_scope() {
    // `match 42 { x => x }` — `x` binds to scrutinee's type (i64).
    let mut h = Harness::new();
    let x_ident = ident_for(&h.interner, "x");
    let body = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Path(edda_syntax::ast::Path {
            segments: vec![x_ident],
            span: Span::DUMMY,
        }),
    };
    let arm = MatchArm {
        span: Span::DUMMY,
        pat: Pat {
            span: Span::DUMMY,
            kind: PatKind::Binding(x_ident),
        },
        guard: None,
        body,
    };
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Match {
            scrutinee: Box::new(lit_int(42)),
            arms: vec![arm],
        },
    };
    let ty = lower_and_synth(&mut h, &e);
    assert_eq!(ty, h.ty_interner.prim(Primitive::I64));
    assert!(h.diags.is_empty(), "binding lookup must succeed");
}

#[test]
fn match_guard_must_be_bool() {
    let mut h = Harness::new();
    let arm = MatchArm {
        span: Span::DUMMY,
        pat: Pat {
            span: Span::DUMMY,
            kind: PatKind::Wildcard,
        },
        guard: Some(lit_int(1)), // not bool — should diagnose
        body: lit_int(2),
    };
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Match {
            scrutinee: Box::new(lit_int(0)),
            arms: vec![arm],
        },
    };
    let _ = lower_and_synth(&mut h, &e);
    assert!(
        h.diags
            .iter()
            .any(|d| d.message.contains("expected `bool`"))
    );
}

#[test]
fn loop_synthesises_never() {
    let mut h = Harness::new();
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Loop {
            body: edda_syntax::ast::Block {
                span: Span::DUMMY,
                stmts: vec![],
                trailing: None,
            },
            label: None,
            decreases: None,
        },
    };
    let ty = lower_and_synth(&mut h, &e);
    assert_eq!(ty, h.ty_interner.prim(Primitive::Never));
    assert!(h.diags.is_empty());
}

#[test]
fn return_synthesises_never() {
    let mut h = Harness::new();
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Return(Some(Box::new(lit_int(1)))),
    };
    let ty = lower_and_synth(&mut h, &e);
    assert_eq!(ty, h.ty_interner.prim(Primitive::Never));
}

#[test]
fn raise_synthesises_never() {
    let mut h = Harness::new();
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Raise(Box::new(lit_int(1))),
    };
    let ty = lower_and_synth(&mut h, &e);
    assert_eq!(ty, h.ty_interner.prim(Primitive::Never));
}

#[test]
fn panic_synthesises_never() {
    let mut h = Harness::new();
    let sym = h.interner.intern("oops");
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Panic(Box::new(Expr {
            span: Span::DUMMY,
            kind: ExprKind::Literal(Literal::Str(sym)),
        })),
    };
    let ty = lower_and_synth(&mut h, &e);
    assert_eq!(ty, h.ty_interner.prim(Primitive::Never));
}

#[test]
fn break_continue_synthesise_never() {
    let mut h = Harness::new();
    let brk = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Break {
            label: None,
            value: Some(Box::new(lit_int(1))),
        },
    };
    let ty = lower_and_synth(&mut h, &brk);
    assert_eq!(ty, h.ty_interner.prim(Primitive::Never));

    let cont = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Continue { label: None },
    };
    let ty = lower_and_synth(&mut h, &cont);
    assert_eq!(ty, h.ty_interner.prim(Primitive::Never));
}

#[test]
fn nested_if_in_arithmetic() {
    // `1 + if cond { 2 } else { 3 }` — outer Add synth, branches join to i64.
    let mut h = Harness::new();
    let inner_if = if_expr(lit_bool(true), lit_int(2), Some(lit_int(3)));
    let outer = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Binary {
            op: BinOp::Add,
            lhs: Box::new(lit_int(1)),
            rhs: Box::new(inner_if),
        },
    };
    let ty = lower_and_synth(&mut h, &outer);
    assert_eq!(ty, h.ty_interner.prim(Primitive::I64));
    assert!(h.diags.is_empty());
}

/// Regression: `if c { 7 } else { 0 } ⇐ i32` narrows both branch
/// trailing literals to `i32`. Without the `check_if` arm
/// this hit the `_ => ic.ty_error()` fallback and the MIR emitter
/// rejected the body-result temp as `Never`.
#[test]
fn if_checks_against_i32_narrows_both_branches() {
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let e = if_expr(lit_bool(true), lit_int(7), Some(lit_int(0)));
    let mut hir = lower_expr(&e, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let i32_id = h.ty_interner.prim(Primitive::I32);
    check_expr(
        &mut env,
        &mut hir,
        i32_id,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert!(h.diags.is_empty(), "expected no diagnostics: {:?}", h.diags.iter().map(|d| &d.message).collect::<Vec<_>>());
    assert_eq!(hir.ty, i32_id);
}

/// Regression: `never` is admissible at any expected type. A
/// `return` expression synthesises `never`; in checking position it
/// must absorb into the expected type rather than emitting a
/// type-mismatch diagnostic. The synth path already encodes this via
/// `join_types`; the check path now agrees via
/// `check_synth_against_expected`'s `never`-arm.
#[test]
fn never_absorbs_into_any_expected_type() {
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    // `return 0` synthesises `never`; check it against i32.
    let ret = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Return(Some(Box::new(lit_int(0)))),
    };
    let mut hir = lower_expr(&ret, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let i32_id = h.ty_interner.prim(Primitive::I32);
    check_expr(
        &mut env,
        &mut hir,
        i32_id,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    // No mismatch diagnostic — `never ⇐ i32` is admissible. The
    // payload-typing path may still emit (no fn_return_ty here so
    // synth_return falls back to synth_expr), so we don't assert on
    // diag emptiness — just on the absence of the structural
    // "expected i32, found never" error.
    let has_never_mismatch = h.diags.iter().any(|d| d.message.contains("found `never`"));
    assert!(!has_never_mismatch, "never must absorb; diags: {:?}", h.diags.iter().map(|d| &d.message).collect::<Vec<_>>());
    assert_eq!(hir.ty, i32_id);
}

/// Regression: a `Call` expression in checking position must
/// recurse via synth+compare rather than dropping to the
/// `_ => Error` fallback. We can't construct a full Call here
/// without a TyCx for the function signature, so this test stays at
/// the "synth + bridge" granularity via a literal in checking
/// position — the canonical path that used to hit `_ => Error`.
#[test]
fn check_expr_fallback_uses_synth_then_compare() {
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    // Tuple is a checking-form-with-elements that previously fell
    // into `_ => Error`; now `check_tuple` destructures expected.
    let tup = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Tuple(vec![lit_int(1), lit_int(2)]),
    };
    let mut hir = lower_expr(&tup, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let i32_id = h.ty_interner.prim(Primitive::I32);
    let expected = h
        .ty_interner
        .tuple(vec![i32_id, i32_id]);
    check_expr(
        &mut env,
        &mut hir,
        expected,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert!(h.diags.is_empty(), "no diagnostics: {:?}", h.diags.iter().map(|d| &d.message).collect::<Vec<_>>());
    assert_eq!(hir.ty, expected);
    // Verify each element narrowed.
    if let crate::hir::HirExprKind::Tuple(elems) = &hir.kind {
        assert_eq!(elems[0].ty, i32_id);
        assert_eq!(elems[1].ty, i32_id);
    } else {
        panic!("expected tuple");
    }
}
