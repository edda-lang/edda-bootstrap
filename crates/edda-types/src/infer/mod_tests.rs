//! Tests for the bidirectional inference dispatchers and TyEnv lattice.

use super::*;
use crate::cx::TyCx;
use crate::lower::lower_expr;
use crate::test_support::{Harness, ident_for, path_for};
use edda_diag::DiagnosticClass;
use edda_span::Span;
use edda_syntax::ast::{
    BindingMode, Expr, ExprKind, Literal, Pat, PatKind, Stmt, StmtKind, Type, TypeKind,
};

fn lit_int(value: u128) -> Expr {
    Expr {
        span: Span::DUMMY,
        kind: ExprKind::Literal(Literal::Int {
            value,
            base: edda_syntax::IntBase::Dec,
        }),
    }
}

fn lower_and_synth(h: &mut Harness, expr: &Expr) -> (TyId, HirExpr) {
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut hir = lower_expr(expr, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let ty = synth_expr(
        &mut env,
        &mut hir,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    (ty, hir)
}

#[test]
fn integer_literal_synthesises_i64() {
    let mut h = Harness::new();
    let (ty, hir) = lower_and_synth(&mut h, &lit_int(42));
    assert_eq!(ty, h.ty_interner.prim(Primitive::I64));
    assert_eq!(hir.ty, h.ty_interner.prim(Primitive::I64));
    assert!(h.diags.is_empty());
}

#[test]
fn float_literal_synthesises_f64() {
    let mut h = Harness::new();
    let sym = h.interner.intern("1.5");
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Literal(Literal::Float(sym)),
    };
    let (ty, _) = lower_and_synth(&mut h, &e);
    assert_eq!(ty, h.ty_interner.prim(Primitive::F64));
}

#[test]
fn float_literal_checks_against_f32_target() {
    let mut h = Harness::new();
    let sym = h.interner.intern("1.5");
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Literal(Literal::Float(sym)),
    };
    let mut hir = lower_expr(&e, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let f32_id = h.ty_interner.prim(Primitive::F32);
    check_expr(
        &mut env,
        &mut hir,
        f32_id,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert_eq!(hir.ty, f32_id);
    assert!(h.diags.is_empty());
}

#[test]
fn float_literal_checks_against_f64_target() {
    let mut h = Harness::new();
    let sym = h.interner.intern("1.5");
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Literal(Literal::Float(sym)),
    };
    let mut hir = lower_expr(&e, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let f64_id = h.ty_interner.prim(Primitive::F64);
    check_expr(
        &mut env,
        &mut hir,
        f64_id,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert_eq!(hir.ty, f64_id);
    assert!(h.diags.is_empty());
}

#[test]
fn float_literal_against_non_float_target_emits_diagnostic() {
    let mut h = Harness::new();
    let sym = h.interner.intern("1.5");
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Literal(Literal::Float(sym)),
    };
    let mut hir = lower_expr(&e, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let i32_id = h.ty_interner.prim(Primitive::I32);
    check_expr(
        &mut env,
        &mut hir,
        i32_id,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert_eq!(hir.ty, h.ty_interner.error());
    assert_eq!(h.diags.error_count(), 1);
}

#[test]
fn bool_string_unit_literals_synth() {
    let mut h = Harness::new();
    let s_sym = h.interner.intern("hi");
    for (lit, expected) in [
        (Literal::Bool(true), Primitive::Bool),
        (Literal::Str(s_sym), Primitive::String),
        (Literal::Unit, Primitive::Unit),
    ] {
        let e = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Literal(lit),
        };
        let (ty, _) = lower_and_synth(&mut h, &e);
        assert_eq!(ty, h.ty_interner.prim(expected));
    }
}

#[test]
fn integer_literal_checks_against_in_range_target() {
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut hir = lower_expr(&lit_int(127), &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let i8_id = h.ty_interner.prim(Primitive::I8);
    check_expr(
        &mut env,
        &mut hir,
        i8_id,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert_eq!(hir.ty, i8_id);
    assert!(h.diags.is_empty());
}

#[test]
fn integer_literal_out_of_range_emits_diagnostic() {
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut hir = lower_expr(&lit_int(300), &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let i8_id = h.ty_interner.prim(Primitive::I8);
    check_expr(
        &mut env,
        &mut hir,
        i8_id,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert_eq!(hir.ty, h.ty_interner.error());
    assert_eq!(h.diags.error_count(), 1);
    let msg = &h.diags.iter().next().unwrap().message;
    assert!(msg.contains("300"));
    assert!(msg.contains("i8"));
}

#[test]
fn synth_check_mismatch_emits_diagnostic() {
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let bool_lit = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Literal(Literal::Bool(true)),
    };
    let mut hir = lower_expr(&bool_lit, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let i32_id = h.ty_interner.prim(Primitive::I32);
    check_expr(
        &mut env,
        &mut hir,
        i32_id,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert_eq!(hir.ty, h.ty_interner.error());
    assert_eq!(h.diags.error_count(), 1);
    assert!(
        h.diags
            .iter()
            .next()
            .unwrap()
            .message
            .contains("expected `i32`")
    );
}

#[test]
fn path_lookup_succeeds_for_bound_name() {
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut env = TyEnv::new();
    let x_sym = h.interner.intern("x");
    env.bind(x_sym, h.ty_interner.prim(Primitive::I32));

    let path_expr = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Path(path_for(&h.interner, &["x"])),
    };
    let mut hir = lower_expr(&path_expr, &cx, &mut h.diags, &h.lint_cfg);
    let ty = synth_expr(
        &mut env,
        &mut hir,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert_eq!(ty, h.ty_interner.prim(Primitive::I32));
    assert!(h.diags.is_empty());
}

#[test]
fn path_lookup_unknown_emits_diagnostic() {
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut env = TyEnv::new();
    let path_expr = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Path(path_for(&h.interner, &["missing"])),
    };
    let mut hir = lower_expr(&path_expr, &cx, &mut h.diags, &h.lint_cfg);
    let ty = synth_expr(
        &mut env,
        &mut hir,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert_eq!(ty, h.ty_interner.error());
    assert_eq!(h.diags.error_count(), 1);
    assert!(
        h.diags
            .iter()
            .next()
            .unwrap()
            .message
            .contains("cannot find binding `missing`")
    );
}

#[test]
fn qualified_path_defers_with_diagnostic() {
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut env = TyEnv::new();
    let path_expr = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Path(path_for(&h.interner, &["std", "fs"])),
    };
    let mut hir = lower_expr(&path_expr, &cx, &mut h.diags, &h.lint_cfg);
    let ty = synth_expr(
        &mut env,
        &mut hir,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert_eq!(ty, h.ty_interner.error());
    assert_eq!(h.diags.error_count(), 1);
    assert!(
        h.diags
            .iter()
            .next()
            .unwrap()
            .message
            .contains("qualified-path expressions")
    );
}

#[test]
fn empty_block_synthesises_unit() {
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let block_expr = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Block(edda_syntax::ast::Block {
            span: Span::DUMMY,
            stmts: vec![],
            trailing: None,
        }),
    };
    let mut hir = lower_expr(&block_expr, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let ty = synth_expr(
        &mut env,
        &mut hir,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert_eq!(ty, h.ty_interner.prim(Primitive::Unit));
}

#[test]
fn block_with_trailing_takes_trailing_type() {
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let block_expr = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Block(edda_syntax::ast::Block {
            span: Span::DUMMY,
            stmts: vec![],
            trailing: Some(Box::new(lit_int(7))),
        }),
    };
    let mut hir = lower_expr(&block_expr, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let ty = synth_expr(
        &mut env,
        &mut hir,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert_eq!(ty, h.ty_interner.prim(Primitive::I64));
}

#[test]
fn let_with_init_synthesises_and_binds() {
    // `let x = 42; x`
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let block = edda_syntax::ast::Block {
        span: Span::DUMMY,
        stmts: vec![Stmt { attributes: Vec::new(),
            span: Span::DUMMY,
            kind: StmtKind::Let {
                mutability: BindingMode::Immutable,
                pat: Pat {
                    span: Span::DUMMY,
                    kind: PatKind::Binding(ident_for(&h.interner, "x")),
                },
                ty: None,
                init: Some(lit_int(42)),
            },
        }],
        trailing: Some(Box::new(Expr {
            span: Span::DUMMY,
            kind: ExprKind::Path(path_for(&h.interner, &["x"])),
        })),
    };
    let expr = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Block(block),
    };
    let mut hir = lower_expr(&expr, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let ty = synth_expr(
        &mut env,
        &mut hir,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert_eq!(ty, h.ty_interner.prim(Primitive::I64));
    assert!(h.diags.is_empty());
}

#[test]
fn let_with_annotation_checks_init_against_annotation() {
    // `let x: i32 = 127; x`
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let block = edda_syntax::ast::Block {
        span: Span::DUMMY,
        stmts: vec![Stmt { attributes: Vec::new(),
            span: Span::DUMMY,
            kind: StmtKind::Let {
                mutability: BindingMode::Immutable,
                pat: Pat {
                    span: Span::DUMMY,
                    kind: PatKind::Binding(ident_for(&h.interner, "x")),
                },
                ty: Some(Type {
                    span: Span::DUMMY,
                    kind: TypeKind::Path(path_for(&h.interner, &["i32"])),
                }),
                init: Some(lit_int(127)),
            },
        }],
        trailing: Some(Box::new(Expr {
            span: Span::DUMMY,
            kind: ExprKind::Path(path_for(&h.interner, &["x"])),
        })),
    };
    let expr = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Block(block),
    };
    let mut hir = lower_expr(&expr, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let ty = synth_expr(
        &mut env,
        &mut hir,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert_eq!(ty, h.ty_interner.prim(Primitive::I32));
    assert!(h.diags.is_empty(), "127 fits in i32");
}

#[test]
fn let_with_annotation_emits_when_init_out_of_range() {
    // `let x: i8 = 200`
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let stmt = Stmt { attributes: Vec::new(),
        span: Span::DUMMY,
        kind: StmtKind::Let {
            mutability: BindingMode::Immutable,
            pat: Pat {
                span: Span::DUMMY,
                kind: PatKind::Binding(ident_for(&h.interner, "x")),
            },
            ty: Some(Type {
                span: Span::DUMMY,
                kind: TypeKind::Path(path_for(&h.interner, &["i8"])),
            }),
            init: Some(lit_int(200)),
        },
    };
    let block = edda_syntax::ast::Block {
        span: Span::DUMMY,
        stmts: vec![stmt],
        trailing: None,
    };
    let expr = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Block(block),
    };
    let mut hir = lower_expr(&expr, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    synth_expr(
        &mut env,
        &mut hir,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    // One diagnostic from the out-of-range integer check.
    assert_eq!(h.diags.error_count(), 1);
    assert_eq!(
        h.diags.iter().next().unwrap().class,
        DiagnosticClass::TypecheckError
    );
}

#[test]
fn scope_pop_on_block_exit() {
    // `let x = 1` inside an inner block should not be visible outside.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let inner_block = edda_syntax::ast::Block {
        span: Span::DUMMY,
        stmts: vec![Stmt { attributes: Vec::new(),
            span: Span::DUMMY,
            kind: StmtKind::Let {
                mutability: BindingMode::Immutable,
                pat: Pat {
                    span: Span::DUMMY,
                    kind: PatKind::Binding(ident_for(&h.interner, "x")),
                },
                ty: None,
                init: Some(lit_int(1)),
            },
        }],
        trailing: None,
    };
    let outer_block = edda_syntax::ast::Block {
        span: Span::DUMMY,
        stmts: vec![Stmt { attributes: Vec::new(),
            span: Span::DUMMY,
            kind: StmtKind::Expr(Expr {
                span: Span::DUMMY,
                kind: ExprKind::Block(inner_block),
            }),
        }],
        // Reference `x` from outside — should fail.
        trailing: Some(Box::new(Expr {
            span: Span::DUMMY,
            kind: ExprKind::Path(path_for(&h.interner, &["x"])),
        })),
    };
    let expr = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Block(outer_block),
    };
    let mut hir = lower_expr(&expr, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let ty = synth_expr(
        &mut env,
        &mut hir,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert_eq!(ty, h.ty_interner.error());
    assert_eq!(h.diags.error_count(), 1);
    assert!(
        h.diags
            .iter()
            .next()
            .unwrap()
            .message
            .contains("cannot find binding `x`")
    );
}

#[test]
fn coherence_region_body_is_typed() {
    // Regression: inference must descend
    // into a `scope(coherence)` body. Without the `Scope` arm the body's
    // expressions kept the `error()` sentinel HIR lowering seeds, which
    // then surfaced at MIR lowering as `BinOp on non-primitive operand`
    // for a `total = total + 1` on a primitive binding. Here the region's
    // trailing `total + 1` must synth to `i32`, not the error sentinel.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let region_body = edda_syntax::ast::Block {
        span: Span::DUMMY,
        stmts: vec![Stmt {
            attributes: Vec::new(),
            span: Span::DUMMY,
            kind: StmtKind::Let {
                mutability: BindingMode::Mutable,
                pat: Pat {
                    span: Span::DUMMY,
                    kind: PatKind::Binding(ident_for(&h.interner, "total")),
                },
                ty: Some(Type {
                    span: Span::DUMMY,
                    kind: TypeKind::Path(path_for(&h.interner, &["i32"])),
                }),
                init: Some(lit_int(0)),
            },
        }],
        trailing: Some(Box::new(Expr {
            span: Span::DUMMY,
            kind: ExprKind::Binary {
                op: edda_syntax::ast::BinOp::Add,
                lhs: Box::new(Expr {
                    span: Span::DUMMY,
                    kind: ExprKind::Path(path_for(&h.interner, &["total"])),
                }),
                rhs: Box::new(lit_int(1)),
            },
        })),
    };
    let expr = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Scope {
            kind: edda_syntax::ast::ScopeKind::Coherence,
            name: None,
            body: region_body,
        },
    };
    let mut hir = lower_expr(&expr, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let ty = synth_expr(
        &mut env,
        &mut hir,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert_eq!(ty, h.ty_interner.prim(Primitive::I32));
    assert_eq!(h.diags.error_count(), 0);
}

fn empty_scope_exec(unit: TyId) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: unit,
        kind: HirExprKind::Scope {
            kind: edda_syntax::ast::ScopeKind::Exec,
            name: None,
            body: HirBlock {
                span: Span::DUMMY,
                ty: unit,
                stmts: Box::from([]),
                trailing: None,
            },
        },
    }
}

#[test]
fn exec_scope_missing_executor_in_row_diagnoses() {
    // `scope(exec)` requires the
    // enclosing function's declared row to carry an `Executor`
    // capability entry. An empty declared row (no capabilities held
    // at all) must diagnose.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let mut expr = empty_scope_exec(unit);
    let mut env = TyEnv::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    synth_expr(&mut env, &mut expr, &mut ic);
    assert_eq!(
        h.diags
            .iter()
            .filter(|d| d.class == DiagnosticClass::ExecutorMissingInRow)
            .count(),
        1
    );
}

#[test]
fn exec_scope_with_executor_in_row_is_clean() {
    // Sibling of `exec_scope_missing_executor_in_row_diagnoses`: a
    // declared row carrying a capability entry bound (via `env`) to an
    // `Executor`-typed parameter must not diagnose.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let exec_sym = h.interner.intern("exec");
    let exec_ty = h.ty_interner.capability(crate::CapabilityType::Executor);
    let mut expr = empty_scope_exec(unit);
    let mut env = TyEnv::new();
    env.bind(exec_sym, exec_ty);
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    ic.declared_row = crate::effect::EffectRow::from_entries([crate::effect::EffectEntry::Capability(exec_sym)]);
    synth_expr(&mut env, &mut expr, &mut ic);
    assert_eq!(
        h.diags
            .iter()
            .filter(|d| d.class == DiagnosticClass::ExecutorMissingInRow)
            .count(),
        0
    );
}

#[test]
fn inner_shadows_outer() {
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut env = TyEnv::new();
    let x_sym = h.interner.intern("x");
    env.bind(x_sym, h.ty_interner.prim(Primitive::I32));

    env.enter_scope();
    env.bind(x_sym, h.ty_interner.prim(Primitive::Bool));

    let path_expr = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Path(path_for(&h.interner, &["x"])),
    };
    let mut hir = lower_expr(&path_expr, &cx, &mut h.diags, &h.lint_cfg);
    let ty = synth_expr(
        &mut env,
        &mut hir,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert_eq!(ty, h.ty_interner.prim(Primitive::Bool));

    env.exit_scope();
    let mut hir2 = lower_expr(&path_expr, &cx, &mut h.diags, &h.lint_cfg);
    let ty2 = synth_expr(
        &mut env,
        &mut hir2,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert_eq!(ty2, h.ty_interner.prim(Primitive::I32));
}

#[test]
fn unhandled_variants_remain_error_silently() {
    // `with { }` in expression position — the comptime `EffectRow`
    // literal is still unhandled in `synth_expr`. Unhandled variants
    // leave `ty = Error` silently. `Try` was the original placeholder
    // here (its rule has since landed), then `Await`
    // (transparent `Task(T)`
    // result typing landed), so neither fits any more.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::EffectRow(edda_syntax::ast::EffectRow {
            span: Span::DUMMY,
            members: Vec::new(),
        }),
    };
    let mut hir = lower_expr(&e, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let ty = synth_expr(
        &mut env,
        &mut hir,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert_eq!(ty, h.ty_interner.error());
    assert!(h.diags.is_empty());
}

#[test]
fn ty_env_depth_tracks_frames() {
    let mut env = TyEnv::new();
    assert_eq!(env.depth(), 1);
    env.enter_scope();
    assert_eq!(env.depth(), 2);
    env.enter_scope();
    assert_eq!(env.depth(), 3);
    env.exit_scope();
    assert_eq!(env.depth(), 2);
    env.exit_scope();
    assert_eq!(env.depth(), 1);
}

#[test]
#[should_panic(expected = "TyEnv::exit_scope underflow")]
fn exit_underflow_panics_in_debug() {
    let mut env = TyEnv::new();
    env.exit_scope();
}

#[test]
fn tuple_pattern_binds_elements_at_declared_types() {
    // `match (1, true) { (x, _) => x }` — x binds at i64.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let x_ident = ident_for(&h.interner, "x");
    let scrutinee = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Tuple(vec![
            lit_int(1),
            Expr {
                span: Span::DUMMY,
                kind: ExprKind::Literal(Literal::Bool(true)),
            },
        ]),
    };
    let arm = edda_syntax::ast::MatchArm {
        span: Span::DUMMY,
        pat: Pat {
            span: Span::DUMMY,
            kind: PatKind::Tuple(vec![
                Pat {
                    span: Span::DUMMY,
                    kind: PatKind::Binding(x_ident),
                },
                Pat {
                    span: Span::DUMMY,
                    kind: PatKind::Wildcard,
                },
            ]),
        },
        guard: None,
        body: Expr {
            span: Span::DUMMY,
            kind: ExprKind::Path(path_for(&h.interner, &["x"])),
        },
    };
    let match_expr = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Match {
            scrutinee: Box::new(scrutinee),
            arms: vec![arm],
        },
    };
    let mut hir = lower_expr(&match_expr, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let ty = synth_expr(
        &mut env,
        &mut hir,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert_eq!(ty, h.ty_interner.prim(Primitive::I64));
    assert!(h.diags.is_empty(), "tuple-pattern destructuring should type-check cleanly");
}

#[test]
fn tuple_pattern_arity_mismatch_emits_diagnostic() {
    // `match (1, true) { (x, _, _) => x }` — scrutinee is 2-tuple, pattern is 3-tuple.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let x_ident = ident_for(&h.interner, "x");
    let scrutinee = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Tuple(vec![
            lit_int(1),
            Expr {
                span: Span::DUMMY,
                kind: ExprKind::Literal(Literal::Bool(true)),
            },
        ]),
    };
    let arm = edda_syntax::ast::MatchArm {
        span: Span::DUMMY,
        pat: Pat {
            span: Span::DUMMY,
            kind: PatKind::Tuple(vec![
                Pat {
                    span: Span::DUMMY,
                    kind: PatKind::Binding(x_ident),
                },
                Pat {
                    span: Span::DUMMY,
                    kind: PatKind::Wildcard,
                },
                Pat {
                    span: Span::DUMMY,
                    kind: PatKind::Wildcard,
                },
            ]),
        },
        guard: None,
        body: Expr {
            span: Span::DUMMY,
            kind: ExprKind::Path(path_for(&h.interner, &["x"])),
        },
    };
    let match_expr = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Match {
            scrutinee: Box::new(scrutinee),
            arms: vec![arm],
        },
    };
    let mut hir = lower_expr(&match_expr, &cx, &mut h.diags, &h.lint_cfg);
    let mut env = TyEnv::new();
    let _ = synth_expr(
        &mut env,
        &mut hir,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    );
    assert!(
        h.diags
            .iter()
            .any(|d| d.message.contains("does not match scrutinee of type")),
    );
}

// -- Mode-tracker tests (§4) ----------------------------

/// `var x: i32; x` — reading an uninitialised binding is a §4 error.
#[test]
fn reading_uninit_var_diagnoses() {
    let mut h = Harness::new();
    let x_ident = ident_for(&h.interner, "x");
    let i32_ty = Type {
        span: Span::DUMMY,
        kind: TypeKind::Path(edda_syntax::ast::Path {
            segments: vec![ident_for(&h.interner, "i32")],
            span: Span::DUMMY,
        }),
    };
    let block = edda_syntax::ast::Block {
        span: Span::DUMMY,
        stmts: vec![Stmt { attributes: Vec::new(),
            span: Span::DUMMY,
            kind: StmtKind::Let {
                mutability: BindingMode::Mutable,
                pat: Pat {
                    span: Span::DUMMY,
                    kind: PatKind::Binding(x_ident),
                },
                ty: Some(i32_ty),
                init: None,
            },
        }],
        trailing: Some(Box::new(Expr {
            span: Span::DUMMY,
            kind: ExprKind::Path(edda_syntax::ast::Path {
                segments: vec![x_ident],
                span: Span::DUMMY,
            }),
        })),
    };
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Block(block),
    };
    let (_, _) = lower_and_synth(&mut h, &e);
    assert!(
        h.diags
            .iter()
            .any(|d| d.message.contains("uninitialised")),
        "expected uninit diagnostic, got: {:?}",
        h.diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

/// `var x: i32; x = 1; x` — reassignment transitions to Valid, then
/// reading is admissible without diagnostic.
#[test]
fn assignment_makes_uninit_var_readable() {
    let mut h = Harness::new();
    let x_ident = ident_for(&h.interner, "x");
    let i32_ty = Type {
        span: Span::DUMMY,
        kind: TypeKind::Path(edda_syntax::ast::Path {
            segments: vec![ident_for(&h.interner, "i32")],
            span: Span::DUMMY,
        }),
    };
    let block = edda_syntax::ast::Block {
        span: Span::DUMMY,
        stmts: vec![
            Stmt { attributes: Vec::new(),
                span: Span::DUMMY,
                kind: StmtKind::Let {
                    mutability: BindingMode::Mutable,
                    pat: Pat {
                        span: Span::DUMMY,
                        kind: PatKind::Binding(x_ident),
                    },
                    ty: Some(i32_ty),
                    init: None,
                },
            },
            Stmt { attributes: Vec::new(),
                span: Span::DUMMY,
                kind: StmtKind::Assign {
                    target: Expr {
                        span: Span::DUMMY,
                        kind: ExprKind::Path(edda_syntax::ast::Path {
                            segments: vec![x_ident],
                            span: Span::DUMMY,
                        }),
                    },
                    op: edda_syntax::ast::AssignOp::Plain,
                    rhs: lit_int(1),
                },
            },
        ],
        trailing: Some(Box::new(Expr {
            span: Span::DUMMY,
            kind: ExprKind::Path(edda_syntax::ast::Path {
                segments: vec![x_ident],
                span: Span::DUMMY,
            }),
        })),
    };
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Block(block),
    };
    let (_, _) = lower_and_synth(&mut h, &e);
    assert!(
        !h.diags
            .iter()
            .any(|d| d.message.contains("uninitialised")),
        "should not diagnose after assignment, got: {:?}",
        h.diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

/// `if c { x = 1 } else {}; x` — one branch initialises, the other
/// doesn't, so the §4 GLB merge keeps `x` Uninit at the join.
#[test]
fn branch_glb_keeps_uninit_when_not_all_branches_initialise() {
    let mut h = Harness::new();
    let x_ident = ident_for(&h.interner, "x");
    let i32_ty = Type {
        span: Span::DUMMY,
        kind: TypeKind::Path(edda_syntax::ast::Path {
            segments: vec![ident_for(&h.interner, "i32")],
            span: Span::DUMMY,
        }),
    };
    let true_lit = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Literal(Literal::Bool(true)),
    };
    let assign_stmt = Stmt { attributes: Vec::new(),
        span: Span::DUMMY,
        kind: StmtKind::Assign {
            target: Expr {
                span: Span::DUMMY,
                kind: ExprKind::Path(edda_syntax::ast::Path {
                    segments: vec![x_ident],
                    span: Span::DUMMY,
                }),
            },
            op: edda_syntax::ast::AssignOp::Plain,
            rhs: lit_int(1),
        },
    };
    let then_block = edda_syntax::ast::Block {
        span: Span::DUMMY,
        stmts: vec![assign_stmt],
        trailing: None,
    };
    let else_block = edda_syntax::ast::Block {
        span: Span::DUMMY,
        stmts: vec![],
        trailing: None,
    };
    let block = edda_syntax::ast::Block {
        span: Span::DUMMY,
        stmts: vec![
            Stmt { attributes: Vec::new(),
                span: Span::DUMMY,
                kind: StmtKind::Let {
                    mutability: BindingMode::Mutable,
                    pat: Pat {
                        span: Span::DUMMY,
                        kind: PatKind::Binding(x_ident),
                    },
                    ty: Some(i32_ty),
                    init: None,
                },
            },
            Stmt { attributes: Vec::new(),
                span: Span::DUMMY,
                kind: StmtKind::Expr(Expr {
                    span: Span::DUMMY,
                    kind: ExprKind::If {
                        cond: Box::new(true_lit),
                        then_block,
                        else_branch: Some(Box::new(Expr {
                            span: Span::DUMMY,
                            kind: ExprKind::Block(else_block),
                        })),
                    },
                }),
            },
        ],
        trailing: Some(Box::new(Expr {
            span: Span::DUMMY,
            kind: ExprKind::Path(edda_syntax::ast::Path {
                segments: vec![x_ident],
                span: Span::DUMMY,
            }),
        })),
    };
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Block(block),
    };
    let (_, _) = lower_and_synth(&mut h, &e);
    // Either branch initialised x; the other did not. GLB =
    // Uninit, so the final read should still diagnose.
    assert!(
        h.diags
            .iter()
            .any(|d| d.message.contains("uninitialised")),
        "GLB merge should keep x uninit; got: {:?}",
        h.diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}
