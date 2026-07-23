//! Tests for the AST → HIR structural lowering pass.

use super::*;
use crate::test_support::{Harness, ident_for, path_for, ty_path};
use crate::prim::Primitive;
use edda_span::Span;
use edda_syntax::ast;
use edda_syntax::ast::{
    BinOp, BindingMode, Expr, ExprKind, Literal, Pat, PatKind, Stmt, StmtKind,
};

fn lower(h: &mut Harness, e: &Expr) -> HirExpr {
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    lower_expr(e, &cx, &mut h.diags, &h.lint_cfg)
}

fn lower_p(h: &mut Harness, p: &Pat) -> HirPat {
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    lower_pat(p, &cx, &mut h.diags, &h.lint_cfg)
}

fn lower_s(h: &mut Harness, s: &Stmt) -> HirStmt {
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    lower_stmt(s, &cx, &mut h.diags, &h.lint_cfg)
}

fn lit_int_expr(value: u128) -> Expr {
    Expr {
        span: Span::DUMMY,
        kind: ExprKind::Literal(Literal::Int {
            value,
            base: edda_syntax::IntBase::Dec,
        }),
    }
}

fn unit_expr() -> Expr {
    Expr {
        span: Span::DUMMY,
        kind: ExprKind::Literal(Literal::Unit),
    }
}

#[test]
fn literal_lowers_to_hir_literal() {
    let mut h = Harness::new();
    let hir = lower(&mut h, &lit_int_expr(42));
    assert_eq!(hir.ty, h.ty_interner.error());
    assert!(matches!(
        hir.kind,
        HirExprKind::Literal(Literal::Int { value: 42, .. })
    ));
    assert!(h.diags.is_empty());
}

#[test]
fn path_lowers_preserving_segments() {
    let mut h = Harness::new();
    let ast_path = path_for(&h.interner, &["std", "fs", "read"]);
    let expr = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Path(ast_path),
    };
    let hir = lower(&mut h, &expr);
    match &hir.kind {
        HirExprKind::Path(p) => {
            assert_eq!(p.segments.len(), 3);
            assert_eq!(p.segments[0].name, h.interner.intern("std"));
            assert_eq!(p.segments[2].name, h.interner.intern("read"));
        }
        _ => panic!("expected Path"),
    }
    assert!(h.diags.is_empty());
}

#[test]
fn binary_recurses_into_operands() {
    let mut h = Harness::new();
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Binary {
            op: BinOp::Add,
            lhs: Box::new(lit_int_expr(1)),
            rhs: Box::new(lit_int_expr(2)),
        },
    };
    let hir = lower(&mut h, &e);
    match &hir.kind {
        HirExprKind::Binary { op, lhs, rhs } => {
            assert_eq!(*op, BinOp::Add);
            assert!(matches!(
                lhs.kind,
                HirExprKind::Literal(Literal::Int { value: 1, .. })
            ));
            assert!(matches!(
                rhs.kind,
                HirExprKind::Literal(Literal::Int { value: 2, .. })
            ));
        }
        _ => panic!("expected Binary"),
    }
}

#[test]
fn cast_target_lowers_via_lower_type() {
    let mut h = Harness::new();
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Cast {
            expr: Box::new(lit_int_expr(0)),
            ty: Box::new(ty_path(&h.interner, "u8")),
            mode: edda_syntax::ast::CastMode::Trap,
        },
    };
    let hir = lower(&mut h, &e);
    match &hir.kind {
        HirExprKind::Cast { target_ty, .. } => {
            assert_eq!(*target_ty, h.ty_interner.prim(Primitive::U8));
        }
        _ => panic!("expected Cast"),
    }
    assert!(h.diags.is_empty());
}

#[test]
fn cast_to_unknown_user_type_cascades_diagnostic() {
    let mut h = Harness::new();
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Cast {
            expr: Box::new(lit_int_expr(0)),
            ty: Box::new(ty_path(&h.interner, "MyType")),
            mode: edda_syntax::ast::CastMode::Trap,
        },
    };
    let hir = lower(&mut h, &e);
    match &hir.kind {
        HirExprKind::Cast { target_ty, .. } => {
            assert_eq!(*target_ty, h.ty_interner.error());
        }
        _ => panic!("expected Cast"),
    }
    assert_eq!(h.diags.error_count(), 1);
}

#[test]
fn block_with_stmts_and_trailing() {
    let mut h = Harness::new();
    let block = ast::Block {
        span: Span::DUMMY,
        stmts: vec![Stmt { attributes: Vec::new(),
            span: Span::DUMMY,
            kind: StmtKind::Expr(unit_expr()),
        }],
        trailing: Some(Box::new(lit_int_expr(7))),
    };
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Block(block),
    };
    let hir = lower(&mut h, &e);
    match &hir.kind {
        HirExprKind::Block(b) => {
            assert_eq!(b.stmts.len(), 1);
            assert!(b.trailing.is_some());
            assert_eq!(b.ty, h.ty_interner.error()); // bidirectional inference fills this
        }
        _ => panic!("expected Block"),
    }
    assert!(h.diags.is_empty());
}

#[test]
fn let_stmt_with_annotation_lowers_via_lower_type() {
    let mut h = Harness::new();
    let stmt = Stmt { attributes: Vec::new(),
        span: Span::DUMMY,
        kind: StmtKind::Let {
            mutability: BindingMode::Immutable,
            pat: Pat {
                span: Span::DUMMY,
                kind: PatKind::Binding(ident_for(&h.interner, "x")),
            },
            ty: Some(ty_path(&h.interner, "i64")),
            init: Some(lit_int_expr(99)),
        },
    };
    let hir = lower_s(&mut h, &stmt);
    match &hir.kind {
        HirStmtKind::Let {
            mutability,
            pat,
            ty,
            init,
        } => {
            assert_eq!(*mutability, BindingMode::Immutable);
            assert!(matches!(pat.kind, HirPatKind::Binding(_)));
            assert_eq!(*ty, Some(h.ty_interner.prim(Primitive::I64)));
            assert!(init.is_some());
        }
        _ => panic!("expected Let"),
    }
    assert!(h.diags.is_empty());
}

#[test]
fn match_arm_with_guard_lowers() {
    let mut h = Harness::new();
    let arm = ast::MatchArm {
        span: Span::DUMMY,
        pat: Pat {
            span: Span::DUMMY,
            kind: PatKind::Wildcard,
        },
        guard: Some(unit_expr()),
        body: lit_int_expr(1),
    };
    let scrut = unit_expr();
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Match {
            scrutinee: Box::new(scrut),
            arms: vec![arm],
        },
    };
    let hir = lower(&mut h, &e);
    match &hir.kind {
        HirExprKind::Match { arms, .. } => {
            assert_eq!(arms.len(), 1);
            assert!(arms[0].guard.is_some());
            assert!(matches!(arms[0].pat.kind, HirPatKind::Wildcard));
        }
        _ => panic!("expected Match"),
    }
}

#[test]
fn tuple_pat_lowers_each_element() {
    let mut h = Harness::new();
    let p = Pat {
        span: Span::DUMMY,
        kind: PatKind::Tuple(vec![
            Pat {
                span: Span::DUMMY,
                kind: PatKind::Wildcard,
            },
            Pat {
                span: Span::DUMMY,
                kind: PatKind::Binding(ident_for(&h.interner, "y")),
            },
        ]),
    };
    let hir = lower_p(&mut h, &p);
    match &hir.kind {
        HirPatKind::Tuple(elems) => {
            assert_eq!(elems.len(), 2);
            assert!(matches!(elems[0].kind, HirPatKind::Wildcard));
            assert!(matches!(elems[1].kind, HirPatKind::Binding(_)));
        }
        _ => panic!("expected Tuple"),
    }
}

#[test]
fn variant_pat_with_tuple_payload() {
    let mut h = Harness::new();
    let inner_pat = Pat {
        span: Span::DUMMY,
        kind: PatKind::Wildcard,
    };
    let p = Pat {
        span: Span::DUMMY,
        kind: PatKind::Variant {
            path: path_for(&h.interner, &["Color", "rgb"]),
            payload: ast::VariantPatPayload::Tuple(vec![inner_pat]),
        },
    };
    let hir = lower_p(&mut h, &p);
    match &hir.kind {
        HirPatKind::Variant { path, payload } => {
            assert_eq!(path.segments.len(), 2);
            assert!(matches!(payload, HirVariantPatPayload::Tuple(elems) if elems.len() == 1));
        }
        _ => panic!("expected Variant"),
    }
}

#[test]
fn struct_pat_preserves_rest_flag() {
    let mut h = Harness::new();
    let p = Pat {
        span: Span::DUMMY,
        kind: PatKind::Struct {
            path: path_for(&h.interner, &["Point"]),
            fields: vec![ast::StructPatField {
                span: Span::DUMMY,
                name: ident_for(&h.interner, "x"),
                pat: Pat {
                    span: Span::DUMMY,
                    kind: PatKind::Wildcard,
                },
            }],
            rest: true,
        },
    };
    let hir = lower_p(&mut h, &p);
    match &hir.kind {
        HirPatKind::Struct { fields, rest, .. } => {
            assert_eq!(fields.len(), 1);
            assert!(*rest);
        }
        _ => panic!("expected Struct"),
    }
}

#[test]
fn loop_with_label_preserves_label() {
    let mut h = Harness::new();
    let label = ident_for(&h.interner, "outer");
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Loop {
            body: ast::Block {
                span: Span::DUMMY,
                stmts: vec![],
                trailing: None,
            },
            label: Some(label),
            decreases: None,
        },
    };
    let hir = lower(&mut h, &e);
    match &hir.kind {
        HirExprKind::Loop { label: l, .. } => assert_eq!(*l, Some(label)),
        _ => panic!("expected Loop"),
    }
}

#[test]
fn break_with_value_and_label() {
    let mut h = Harness::new();
    let label = ident_for(&h.interner, "outer");
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Break {
            label: Some(label),
            value: Some(Box::new(lit_int_expr(5))),
        },
    };
    let hir = lower(&mut h, &e);
    match &hir.kind {
        HirExprKind::Break { label: l, value } => {
            assert_eq!(*l, Some(label));
            assert!(value.is_some());
        }
        _ => panic!("expected Break"),
    }
}

#[test]
fn error_variant_propagates_silently() {
    let mut h = Harness::new();
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Error,
    };
    let hir = lower(&mut h, &e);
    assert!(matches!(hir.kind, HirExprKind::Error));
    assert_eq!(hir.ty, h.ty_interner.error());
    assert!(h.diags.is_empty());
}

#[test]
fn pat_error_propagates_silently() {
    let mut h = Harness::new();
    let p = Pat {
        span: Span::DUMMY,
        kind: PatKind::Error,
    };
    let hir = lower_p(&mut h, &p);
    assert!(matches!(hir.kind, HirPatKind::Error));
    assert!(h.diags.is_empty());
}

#[test]
fn try_await_raise_panic_lower_through_box() {
    let mut h = Harness::new();
    let inner = lit_int_expr(0);
    for ctor in [
        ExprKind::Try(Box::new(inner.clone())),
        ExprKind::Await(Box::new(inner.clone())),
        ExprKind::Raise(Box::new(inner.clone())),
        ExprKind::Panic(Box::new(inner.clone())),
        ExprKind::Comptime(Box::new(inner.clone())),
    ] {
        let e = Expr {
            span: Span::DUMMY,
            kind: ctor,
        };
        let hir = lower(&mut h, &e);
        // Just verify the lowering doesn't lose the inner expression.
        let inner_kind = match &hir.kind {
            HirExprKind::Try(b)
            | HirExprKind::Await(b)
            | HirExprKind::Raise(b)
            | HirExprKind::Panic(b)
            | HirExprKind::Comptime(b) => &b.kind,
            other => panic!("unexpected kind: {other:?}"),
        };
        assert!(matches!(
            inner_kind,
            HirExprKind::Literal(Literal::Int { value: 0, .. })
        ));
    }
    assert!(h.diags.is_empty());
}

#[test]
fn tuple_expr_lowers_each_element() {
    let mut h = Harness::new();
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Tuple(vec![lit_int_expr(1), lit_int_expr(2), lit_int_expr(3)]),
    };
    let hir = lower(&mut h, &e);
    match &hir.kind {
        HirExprKind::Tuple(elems) => assert_eq!(elems.len(), 3),
        _ => panic!("expected Tuple"),
    }
}

#[test]
fn ty_field_is_error_until_inference_runs() {
    let mut h = Harness::new();
    // Verify the contract: every value-producing node has
    // ty = error() from the structural lowering.
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Tuple(vec![lit_int_expr(1), lit_int_expr(2)]),
    };
    let hir = lower(&mut h, &e);
    let err = h.ty_interner.error();
    assert_eq!(hir.ty, err);
    match &hir.kind {
        HirExprKind::Tuple(elems) => {
            for elem in elems {
                assert_eq!(elem.ty, err);
            }
        }
        _ => panic!("expected Tuple"),
    }
}

#[test]
fn nested_expression_recursion_preserves_shape() {
    let mut h = Harness::new();
    // `if cond { 1 + 2 } else { 3 }` — exercises If + Binary + Block + Literal.
    let cond = unit_expr();
    let then_block = ast::Block {
        span: Span::DUMMY,
        stmts: vec![],
        trailing: Some(Box::new(Expr {
            span: Span::DUMMY,
            kind: ExprKind::Binary {
                op: BinOp::Add,
                lhs: Box::new(lit_int_expr(1)),
                rhs: Box::new(lit_int_expr(2)),
            },
        })),
    };
    let else_branch = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Block(ast::Block {
            span: Span::DUMMY,
            stmts: vec![],
            trailing: Some(Box::new(lit_int_expr(3))),
        }),
    };
    let e = Expr {
        span: Span::DUMMY,
        kind: ExprKind::If {
            cond: Box::new(cond),
            then_block,
            else_branch: Some(Box::new(else_branch)),
        },
    };
    let hir = lower(&mut h, &e);
    match &hir.kind {
        HirExprKind::If {
            cond,
            then_block,
            else_branch,
        } => {
            assert!(matches!(
                cond.kind,
                HirExprKind::Literal(Literal::Unit)
            ));
            assert!(matches!(
                then_block.trailing.as_ref().unwrap().kind,
                HirExprKind::Binary { .. }
            ));
            assert!(else_branch.is_some());
        }
        _ => panic!("expected If"),
    }
    assert!(h.diags.is_empty());
}
