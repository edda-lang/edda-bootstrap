//! Tests for the HIR expression evaluator.

use edda_diag::Diagnostics;
use edda_intern::Interner;
use edda_span::Span;
use edda_syntax::IntBase;
use edda_syntax::ast::{AssignOp, BinOp, BindingMode, CastMode, Ident, Literal, RangeKind, UnOp};
use edda_target::{AbiVariant, Arch, FeatureSet, Os, TargetCfg, TargetTriple};
use edda_types::{
    HirBlock, HirCallArg, HirExpr, HirExprKind, HirPat, HirPatKind, HirPath, HirStmt, HirStmtKind,
    HirStructLitField, Primitive, TyId, TyInterner,
};

use crate::Value;
use crate::eval::{EvalCx, eval_expr};

fn x86_64_with_avx2() -> TargetCfg {
    let mut features = FeatureSet::new(Arch::X86_64);
    features.insert("avx2").unwrap();
    TargetCfg::with_features(
        TargetTriple::new(Arch::X86_64, Os::Linux, AbiVariant::Gnu),
        features,
    )
    .unwrap()
}

fn lit_int(ty: &TyInterner, value: u128, prim: Primitive) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(prim),
        kind: HirExprKind::Literal(Literal::Int {
            value,
            base: IntBase::Dec,
        }),
    }
}

fn lit_bool(ty: &TyInterner, b: bool) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::Bool),
        kind: HirExprKind::Literal(Literal::Bool(b)),
    }
}

fn path_to_type(interner: &Interner, ty: &TyInterner, name: &str) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::Type),
        kind: HirExprKind::Path(HirPath {
            span: Span::DUMMY,
            segments: Box::from([Ident {
                name: interner.intern(name),
                span: Span::DUMMY,
            }]),
        }),
    }
}

fn binary(
    _ty: &TyInterner,
    op: BinOp,
    lhs: HirExpr,
    rhs: HirExpr,
    result_ty: TyId,
) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: result_ty,
        kind: HirExprKind::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        },
    }
}

fn cast(ty: &TyInterner, inner: HirExpr, target: Primitive, mode: CastMode) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(target),
        kind: HirExprKind::Cast {
            expr: Box::new(inner),
            target_ty: ty.prim(target),
            mode,
        },
    }
}

#[test]
fn cast_through_eval_expr_repro() {
    // `(17 as i32) % (0 as i32 - 5)`.
    // Euclidean modulo yields a non-negative result: 17 mod 5 = 2.
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let lhs = cast(&ty, lit_int(&ty, 17, Primitive::I64), Primitive::I32, CastMode::Trap);
    let zero_i32 = cast(&ty, lit_int(&ty, 0, Primitive::I64), Primitive::I32, CastMode::Trap);
    let rhs = binary(
        &ty,
        BinOp::Sub,
        zero_i32,
        lit_int(&ty, 5, Primitive::I32),
        ty.prim(Primitive::I32),
    );
    let expr = binary(&ty, BinOp::Mod, lhs, rhs, ty.prim(Primitive::I32));
    let v = eval_expr(&expr, &mut cx).unwrap();
    match v {
        Value::Int(i) => {
            assert_eq!(i.width(), Primitive::I32);
            assert_eq!(i.as_i128(), Some(2));
        }
        other => panic!("expected Int, got {:?}", other),
    }
    assert!(diags.is_empty());
}

#[test]
fn cast_widening_preserves_value_through_eval() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let expr = cast(&ty, lit_int(&ty, 200, Primitive::I32), Primitive::I64, CastMode::Trap);
    let v = eval_expr(&expr, &mut cx).unwrap();
    match v {
        Value::Int(i) => {
            assert_eq!(i.width(), Primitive::I64);
            assert_eq!(i.as_i128(), Some(200));
        }
        _ => unreachable!(),
    }
    assert!(diags.is_empty());
}

#[test]
fn cast_trap_out_of_range_emits_panic() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    // 300 as i8 (trapping) — out of range.
    let expr = cast(&ty, lit_int(&ty, 300, Primitive::I32), Primitive::I8, CastMode::Trap);
    let result = eval_expr(&expr, &mut cx);
    assert!(result.is_none());
    assert_eq!(diags.error_count(), 1);
    assert!(diags.iter().next().unwrap().message.contains("out of range"));
}

#[test]
fn cast_wrapping_truncates_through_eval() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    // 300 as i8 wrapping → 44.
    let expr = cast(&ty, lit_int(&ty, 300, Primitive::I32), Primitive::I8, CastMode::Wrapping);
    let v = eval_expr(&expr, &mut cx).unwrap();
    match v {
        Value::Int(i) => assert_eq!(i.as_i128(), Some(44)),
        _ => unreachable!(),
    }
    assert!(diags.is_empty());
}

#[test]
fn cast_saturating_clamps_through_eval() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    // 300 as i8 saturating → 127.
    let expr = cast(&ty, lit_int(&ty, 300, Primitive::I32), Primitive::I8, CastMode::Saturating);
    let v = eval_expr(&expr, &mut cx).unwrap();
    match v {
        Value::Int(i) => assert_eq!(i.as_i128(), Some(127)),
        _ => unreachable!(),
    }
    assert!(diags.is_empty());
}

#[test]
fn literal_int_round_trip() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let expr = lit_int(&ty, 42, Primitive::I32);
    let v = eval_expr(&expr, &mut cx).unwrap();
    match v {
        Value::Int(i) => {
            assert_eq!(i.width(), Primitive::I32);
            assert_eq!(i.as_i128(), Some(42));
        }
        other => panic!("expected Int, got {:?}", other),
    }
    assert!(diags.is_empty());
}

#[test]
fn binary_arithmetic_walks_recursively() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    // (1 + 2) * 3 → 9
    let inner = binary(
        &ty,
        BinOp::Add,
        lit_int(&ty, 1, Primitive::I64),
        lit_int(&ty, 2, Primitive::I64),
        ty.prim(Primitive::I64),
    );
    let outer = binary(
        &ty,
        BinOp::Mul,
        inner,
        lit_int(&ty, 3, Primitive::I64),
        ty.prim(Primitive::I64),
    );
    let v = eval_expr(&outer, &mut cx).unwrap();
    match v {
        Value::Int(i) => assert_eq!(i.as_i128(), Some(9)),
        _ => unreachable!(),
    }
}

#[test]
fn comparison_returns_bool() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let e = binary(
        &ty,
        BinOp::Lt,
        lit_int(&ty, 3, Primitive::I32),
        lit_int(&ty, 5, Primitive::I32),
        ty.prim(Primitive::Bool),
    );
    let v = eval_expr(&e, &mut cx).unwrap();
    assert!(matches!(v, Value::Bool(true)));
}

#[test]
fn logical_short_circuits_on_false_and() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    // `false && (1 / 0 == 0)` — the RHS would div-by-zero but is
    // short-circuited.
    let div_by_zero = binary(
        &ty,
        BinOp::Eq,
        binary(
            &ty,
            BinOp::Div,
            lit_int(&ty, 1, Primitive::I32),
            lit_int(&ty, 0, Primitive::I32),
            ty.prim(Primitive::I32),
        ),
        lit_int(&ty, 0, Primitive::I32),
        ty.prim(Primitive::Bool),
    );
    let combined = binary(
        &ty,
        BinOp::And,
        lit_bool(&ty, false),
        div_by_zero,
        ty.prim(Primitive::Bool),
    );
    let v = eval_expr(&combined, &mut cx).unwrap();
    assert!(matches!(v, Value::Bool(false)));
    assert!(diags.is_empty()); // div-by-zero never reached
}

#[test]
fn logical_short_circuits_on_true_or() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let div_by_zero = binary(
        &ty,
        BinOp::Eq,
        binary(
            &ty,
            BinOp::Div,
            lit_int(&ty, 1, Primitive::I32),
            lit_int(&ty, 0, Primitive::I32),
            ty.prim(Primitive::I32),
        ),
        lit_int(&ty, 0, Primitive::I32),
        ty.prim(Primitive::Bool),
    );
    let combined = binary(
        &ty,
        BinOp::Or,
        lit_bool(&ty, true),
        div_by_zero,
        ty.prim(Primitive::Bool),
    );
    let v = eval_expr(&combined, &mut cx).unwrap();
    assert!(matches!(v, Value::Bool(true)));
    assert!(diags.is_empty());
}

#[test]
fn if_then_branch() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let then_block = HirBlock {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::I32),
        stmts: Box::from([]),
        trailing: Some(Box::new(lit_int(&ty, 7, Primitive::I32))),
    };
    let else_block = HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::I32),
        kind: HirExprKind::Block(HirBlock {
            span: Span::DUMMY,
            ty: ty.prim(Primitive::I32),
            stmts: Box::from([]),
            trailing: Some(Box::new(lit_int(&ty, 11, Primitive::I32))),
        }),
    };
    let if_expr = HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::I32),
        kind: HirExprKind::If {
            cond: Box::new(lit_bool(&ty, true)),
            then_block,
            else_branch: Some(Box::new(else_block)),
        },
    };
    let v = eval_expr(&if_expr, &mut cx).unwrap();
    match v {
        Value::Int(i) => assert_eq!(i.as_i128(), Some(7)),
        _ => unreachable!(),
    }
}

#[test]
fn path_to_primitive_yields_type_value() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let expr = path_to_type(&interner, &ty, "u8");
    let v = eval_expr(&expr, &mut cx).unwrap();
    match v {
        Value::Type(id) => assert_eq!(id, ty.prim(Primitive::U8)),
        _ => unreachable!(),
    }
}

#[test]
fn call_size_of_through_hir() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    // `size_of(i32)` → 4
    let callee = HirExpr {
        span: Span::DUMMY,
        ty: ty.error(), // callee type is irrelevant for built-in lookup
        kind: HirExprKind::Path(HirPath {
            span: Span::DUMMY,
            segments: Box::from([Ident {
                name: interner.intern("size_of"),
                span: Span::DUMMY,
            }]),
        }),
    };
    let call = HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::Usize),
        kind: HirExprKind::Call {
            callee: Box::new(callee),
            args: Box::from([HirCallArg {
                span: Span::DUMMY,
                mode: None,
                name: None,
                expr: path_to_type(&interner, &ty, "i32"),
            }]),
        },
    };
    let v = eval_expr(&call, &mut cx).unwrap();
    match v {
        Value::Int(i) => {
            assert_eq!(i.width(), Primitive::Usize);
            assert_eq!(i.as_u128(), Some(4));
        }
        _ => unreachable!(),
    }
}

#[test]
fn panic_emits_diagnostic() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let msg = HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::String),
        kind: HirExprKind::Literal(Literal::Str(interner.intern("expected power of two"))),
    };
    let panic_expr = HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::Never),
        kind: HirExprKind::Panic(Box::new(msg)),
    };
    let result = eval_expr(&panic_expr, &mut cx);
    assert!(result.is_none());
    assert_eq!(diags.error_count(), 1);
    let d = diags.iter().next().unwrap();
    assert!(d.message.contains("expected power of two"));
}

#[test]
fn unary_negation_on_signed_int() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let expr = HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::I32),
        kind: HirExprKind::Unary {
            op: UnOp::Neg,
            expr: Box::new(lit_int(&ty, 5, Primitive::I32)),
        },
    };
    let v = eval_expr(&expr, &mut cx).unwrap();
    match v {
        Value::Int(i) => assert_eq!(i.as_i128(), Some(-5)),
        _ => unreachable!(),
    }
}

#[test]
fn comptime_block_pass_through() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let inner = lit_int(&ty, 100, Primitive::I32);
    let block = HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::I32),
        kind: HirExprKind::ComptimeBlock(HirBlock {
            span: Span::DUMMY,
            ty: ty.prim(Primitive::I32),
            stmts: Box::from([HirStmt {
                span: Span::DUMMY,
                kind: HirStmtKind::Expr(lit_int(&ty, 1, Primitive::I32)),
            }]),
            trailing: Some(Box::new(inner)),
        }),
    };
    let v = eval_expr(&block, &mut cx).unwrap();
    match v {
        Value::Int(i) => assert_eq!(i.as_i128(), Some(100)),
        _ => unreachable!(),
    }
}

#[test]
fn overflow_emits_panic_diagnostic() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    // i8::MAX + 1 overflows.
    let expr = binary(
        &ty,
        BinOp::Add,
        lit_int(&ty, 127, Primitive::I8),
        lit_int(&ty, 1, Primitive::I8),
        ty.prim(Primitive::I8),
    );
    let result = eval_expr(&expr, &mut cx);
    assert!(result.is_none());
    assert_eq!(diags.error_count(), 1);
}

#[test]
fn field_count_on_primitive_rejected() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let callee = HirExpr {
        span: Span::DUMMY,
        ty: ty.error(),
        kind: HirExprKind::Path(HirPath {
            span: Span::DUMMY,
            segments: Box::from([Ident {
                name: interner.intern("field_count"),
                span: Span::DUMMY,
            }]),
        }),
    };
    let call = HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::Usize),
        kind: HirExprKind::Call {
            callee: Box::new(callee),
            args: Box::from([HirCallArg {
                span: Span::DUMMY,
                mode: None,
                name: None,
                expr: path_to_type(&interner, &ty, "i32"),
            }]),
        },
    };
    let result = eval_expr(&call, &mut cx);
    assert!(result.is_none());
    assert_eq!(diags.error_count(), 1);
    let d = diags.iter().next().unwrap();
    assert!(d.message.contains("field_count"));
}

fn ident(interner: &Interner, name: &str) -> Ident {
    Ident {
        name: interner.intern(name),
        span: Span::DUMMY,
    }
}

fn path_expr(interner: &Interner, ty: &TyInterner, name: &str, prim: Primitive) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(prim),
        kind: HirExprKind::Path(HirPath {
            span: Span::DUMMY,
            segments: Box::from([ident(interner, name)]),
        }),
    }
}

fn let_stmt(
    interner: &Interner,
    ty: &TyInterner,
    mutability: BindingMode,
    name: &str,
    init: HirExpr,
) -> HirStmt {
    let pat_ty = init.ty;
    HirStmt {
        span: Span::DUMMY,
        kind: HirStmtKind::Let {
            mutability,
            pat: HirPat {
                span: Span::DUMMY,
                ty: pat_ty,
                kind: HirPatKind::Binding(ident(interner, name)),
            },
            ty: Some(ty.prim(Primitive::I64)),
            init: Some(init),
        },
    }
}

fn assign_stmt(interner: &Interner, ty: &TyInterner, name: &str, op: AssignOp, rhs: HirExpr) -> HirStmt {
    HirStmt {
        span: Span::DUMMY,
        kind: HirStmtKind::Assign {
            target: path_expr(interner, ty, name, Primitive::I64),
            op,
            rhs,
        },
    }
}

fn block_expr(ty: &TyInterner, stmts: Vec<HirStmt>, trailing: HirExpr, prim: Primitive) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(prim),
        kind: HirExprKind::Block(HirBlock {
            span: Span::DUMMY,
            ty: ty.prim(prim),
            stmts: stmts.into_boxed_slice(),
            trailing: Some(Box::new(trailing)),
        }),
    }
}

fn int_result(v: Option<Value>) -> i128 {
    match v.unwrap() {
        Value::Int(i) => i.as_i128().unwrap(),
        other => panic!("expected int, got {other:?}"),
    }
}

fn array_expr(ty: &TyInterner, elems: Vec<HirExpr>, elem_prim: Primitive) -> HirExpr {
    let elem_ty = ty.prim(elem_prim);
    HirExpr {
        span: Span::DUMMY,
        ty: ty.slice(elem_ty),
        kind: HirExprKind::Array(elems.into_boxed_slice()),
    }
}

fn index_expr(ty: &TyInterner, receiver: HirExpr, index: HirExpr, elem_prim: Primitive) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(elem_prim),
        kind: HirExprKind::Index {
            receiver: Box::new(receiver),
            index: Box::new(index),
        },
    }
}

fn range_expr(ty: &TyInterner, lo: HirExpr, hi: HirExpr, kind: RangeKind) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: ty.error(),
        kind: HirExprKind::Range {
            lo: Some(Box::new(lo)),
            hi: Some(Box::new(hi)),
            kind,
        },
    }
}

fn binding_pat(interner: &Interner, ty: &TyInterner, name: &str, prim: Primitive) -> HirPat {
    HirPat {
        span: Span::DUMMY,
        ty: ty.prim(prim),
        kind: HirPatKind::Binding(ident(interner, name)),
    }
}

fn for_expr(ty: &TyInterner, pat: HirPat, iter: HirExpr, body: HirBlock) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::Unit),
        kind: HirExprKind::For {
            pat: Box::new(pat),
            iter: Box::new(iter),
            body,
            label: None,
        },
    }
}

fn index_assign_stmt(
    interner: &Interner,
    ty: &TyInterner,
    name: &str,
    idx: HirExpr,
    op: AssignOp,
    rhs: HirExpr,
) -> HirStmt {
    HirStmt {
        span: Span::DUMMY,
        kind: HirStmtKind::Assign {
            target: index_expr(ty, path_expr(interner, ty, name, Primitive::I64), idx, Primitive::I64),
            op,
            rhs,
        },
    }
}

#[test]
fn let_binding_resolves_through_path() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    // { let x = 41; x + 1 }
    let expr = block_expr(
        &ty,
        vec![let_stmt(
            &interner,
            &ty,
            BindingMode::Immutable,
            "x",
            lit_int(&ty, 41, Primitive::I64),
        )],
        binary(
            &ty,
            BinOp::Add,
            path_expr(&interner, &ty, "x", Primitive::I64),
            lit_int(&ty, 1, Primitive::I64),
            ty.prim(Primitive::I64),
        ),
        Primitive::I64,
    );
    assert_eq!(int_result(eval_expr(&expr, &mut cx)), 42);
    assert_eq!(cx.env.depth(), 0);
}

#[test]
fn var_assignment_updates_binding() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    // { var t = 1; t = 5; t += 2; t }  — the CRC-table accumulate shape.
    let expr = block_expr(
        &ty,
        vec![
            let_stmt(
                &interner,
                &ty,
                BindingMode::Mutable,
                "t",
                lit_int(&ty, 1, Primitive::I64),
            ),
            assign_stmt(&interner, &ty, "t", AssignOp::Plain, lit_int(&ty, 5, Primitive::I64)),
            assign_stmt(&interner, &ty, "t", AssignOp::Add, lit_int(&ty, 2, Primitive::I64)),
        ],
        path_expr(&interner, &ty, "t", Primitive::I64),
        Primitive::I64,
    );
    assert_eq!(int_result(eval_expr(&expr, &mut cx)), 7);
}

#[test]
fn inner_block_binding_does_not_leak() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    // { { let x = 1; x }; x } — the second `x` is out of scope.
    let inner = block_expr(
        &ty,
        vec![let_stmt(
            &interner,
            &ty,
            BindingMode::Immutable,
            "x",
            lit_int(&ty, 1, Primitive::I64),
        )],
        path_expr(&interner, &ty, "x", Primitive::I64),
        Primitive::I64,
    );
    let expr = block_expr(
        &ty,
        vec![HirStmt {
            span: Span::DUMMY,
            kind: HirStmtKind::Expr(inner),
        }],
        path_expr(&interner, &ty, "x", Primitive::I64),
        Primitive::I64,
    );
    let result = eval_expr(&expr, &mut cx);
    assert!(result.is_none());
    assert_eq!(diags.error_count(), 1);
    let d = diags.iter().next().unwrap();
    assert!(d.message.contains("unresolved identifier `x`"));
}

#[test]
fn assignment_to_unbound_name_panics() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let expr = block_expr(
        &ty,
        vec![assign_stmt(&interner, &ty, "t", AssignOp::Plain, lit_int(&ty, 5, Primitive::I64))],
        lit_int(&ty, 0, Primitive::I64),
        Primitive::I64,
    );
    let result = eval_expr(&expr, &mut cx);
    assert!(result.is_none());
    assert_eq!(diags.error_count(), 1);
    let d = diags.iter().next().unwrap();
    assert!(d.message.contains("not bound in this scope"));
}

#[test]
fn shadowed_binding_restores_after_inner_scope() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    // { let x = 1; { let x = 2; x }; x } — trailing `x` sees the outer 1.
    let inner = block_expr(
        &ty,
        vec![let_stmt(
            &interner,
            &ty,
            BindingMode::Immutable,
            "x",
            lit_int(&ty, 2, Primitive::I64),
        )],
        path_expr(&interner, &ty, "x", Primitive::I64),
        Primitive::I64,
    );
    let expr = block_expr(
        &ty,
        vec![
            let_stmt(
                &interner,
                &ty,
                BindingMode::Immutable,
                "x",
                lit_int(&ty, 1, Primitive::I64),
            ),
            HirStmt {
                span: Span::DUMMY,
                kind: HirStmtKind::Expr(inner),
            },
        ],
        path_expr(&interner, &ty, "x", Primitive::I64),
        Primitive::I64,
    );
    assert_eq!(int_result(eval_expr(&expr, &mut cx)), 1);
}

#[test]
fn wildcard_let_discards_value() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let expr = block_expr(
        &ty,
        vec![HirStmt {
            span: Span::DUMMY,
            kind: HirStmtKind::Let {
                mutability: BindingMode::Immutable,
                pat: HirPat {
                    span: Span::DUMMY,
                    ty: ty.prim(Primitive::I64),
                    kind: HirPatKind::Wildcard,
                },
                ty: None,
                init: Some(lit_int(&ty, 9, Primitive::I64)),
            },
        }],
        lit_int(&ty, 3, Primitive::I64),
        Primitive::I64,
    );
    assert_eq!(int_result(eval_expr(&expr, &mut cx)), 3);
    assert_eq!(cx.env.depth(), 0);
}

#[test]
fn array_literal_and_index_read() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    // [10, 20, 30][1] == 20
    let arr = array_expr(
        &ty,
        vec![
            lit_int(&ty, 10, Primitive::I64),
            lit_int(&ty, 20, Primitive::I64),
            lit_int(&ty, 30, Primitive::I64),
        ],
        Primitive::I64,
    );
    let expr = index_expr(&ty, arr, lit_int(&ty, 1, Primitive::Usize), Primitive::I64);
    assert_eq!(int_result(eval_expr(&expr, &mut cx)), 20);
    assert!(diags.is_empty());
}

#[test]
fn array_index_out_of_range_panics() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let arr = array_expr(&ty, vec![lit_int(&ty, 1, Primitive::I64)], Primitive::I64);
    let expr = index_expr(&ty, arr, lit_int(&ty, 5, Primitive::Usize), Primitive::I64);
    let result = eval_expr(&expr, &mut cx);
    assert!(result.is_none());
    assert_eq!(diags.error_count(), 1);
    let d = diags.iter().next().unwrap();
    assert!(d.message.contains("out of range"));
}

#[test]
fn index_assignment_updates_array_binding() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    // { var t = [0, 0, 0]; t[1] = 42; t[1] }
    let arr = array_expr(
        &ty,
        vec![
            lit_int(&ty, 0, Primitive::I64),
            lit_int(&ty, 0, Primitive::I64),
            lit_int(&ty, 0, Primitive::I64),
        ],
        Primitive::I64,
    );
    let expr = block_expr(
        &ty,
        vec![
            let_stmt(&interner, &ty, BindingMode::Mutable, "t", arr),
            index_assign_stmt(
                &interner,
                &ty,
                "t",
                lit_int(&ty, 1, Primitive::Usize),
                AssignOp::Plain,
                lit_int(&ty, 42, Primitive::I64),
            ),
        ],
        index_expr(
            &ty,
            path_expr(&interner, &ty, "t", Primitive::I64),
            lit_int(&ty, 1, Primitive::Usize),
            Primitive::I64,
        ),
        Primitive::I64,
    );
    assert_eq!(int_result(eval_expr(&expr, &mut cx)), 42);
    assert!(diags.is_empty());
}

#[test]
fn index_compound_assignment_reads_current_element() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    // { var t = [10, 20]; t[0] += 5; t[0] }
    let arr = array_expr(
        &ty,
        vec![lit_int(&ty, 10, Primitive::I64), lit_int(&ty, 20, Primitive::I64)],
        Primitive::I64,
    );
    let expr = block_expr(
        &ty,
        vec![
            let_stmt(&interner, &ty, BindingMode::Mutable, "t", arr),
            index_assign_stmt(
                &interner,
                &ty,
                "t",
                lit_int(&ty, 0, Primitive::Usize),
                AssignOp::Add,
                lit_int(&ty, 5, Primitive::I64),
            ),
        ],
        index_expr(
            &ty,
            path_expr(&interner, &ty, "t", Primitive::I64),
            lit_int(&ty, 0, Primitive::Usize),
            Primitive::I64,
        ),
        Primitive::I64,
    );
    assert_eq!(int_result(eval_expr(&expr, &mut cx)), 15);
}

#[test]
fn for_range_half_open_sums() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    // { var sum = 0; for i in 0..<5 { sum += i }; sum } == 0+1+2+3+4 == 10
    let body = HirBlock {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::Unit),
        stmts: Box::from([assign_stmt(
            &interner,
            &ty,
            "sum",
            AssignOp::Add,
            path_expr(&interner, &ty, "i", Primitive::I64),
        )]),
        trailing: None,
    };
    let for_loop = for_expr(
        &ty,
        binding_pat(&interner, &ty, "i", Primitive::I64),
        range_expr(
            &ty,
            lit_int(&ty, 0, Primitive::I64),
            lit_int(&ty, 5, Primitive::I64),
            RangeKind::HalfOpen,
        ),
        body,
    );
    let expr = block_expr(
        &ty,
        vec![
            let_stmt(&interner, &ty, BindingMode::Mutable, "sum", lit_int(&ty, 0, Primitive::I64)),
            HirStmt {
                span: Span::DUMMY,
                kind: HirStmtKind::Expr(for_loop),
            },
        ],
        path_expr(&interner, &ty, "sum", Primitive::I64),
        Primitive::I64,
    );
    assert_eq!(int_result(eval_expr(&expr, &mut cx)), 10);
    assert!(diags.is_empty());
}

#[test]
fn for_range_closed_at_width_boundary_does_not_overflow() {
    // A `Closed` range whose `hi` sits
    // at the width's maximum representable value (`u8` 255) must not
    // attempt a one-past-the-end increment after the last iteration.
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    // { var count = 0; for i in 250u8..=255u8 { count += 1 }; count } == 6
    let body = HirBlock {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::Unit),
        stmts: Box::from([assign_stmt(
            &interner,
            &ty,
            "count",
            AssignOp::Add,
            lit_int(&ty, 1, Primitive::I64),
        )]),
        trailing: None,
    };
    let for_loop = for_expr(
        &ty,
        binding_pat(&interner, &ty, "i", Primitive::U8),
        range_expr(
            &ty,
            lit_int(&ty, 250, Primitive::U8),
            lit_int(&ty, 255, Primitive::U8),
            RangeKind::Closed,
        ),
        body,
    );
    let expr = block_expr(
        &ty,
        vec![
            let_stmt(&interner, &ty, BindingMode::Mutable, "count", lit_int(&ty, 0, Primitive::I64)),
            HirStmt {
                span: Span::DUMMY,
                kind: HirStmtKind::Expr(for_loop),
            },
        ],
        path_expr(&interner, &ty, "count", Primitive::I64),
        Primitive::I64,
    );
    assert_eq!(int_result(eval_expr(&expr, &mut cx)), 6);
    assert!(diags.is_empty());
}

#[test]
fn for_over_array_sums() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    // { var sum = 0; for x in [1, 2, 3] { sum += x }; sum } == 6
    let arr = array_expr(
        &ty,
        vec![
            lit_int(&ty, 1, Primitive::I64),
            lit_int(&ty, 2, Primitive::I64),
            lit_int(&ty, 3, Primitive::I64),
        ],
        Primitive::I64,
    );
    let body = HirBlock {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::Unit),
        stmts: Box::from([assign_stmt(
            &interner,
            &ty,
            "sum",
            AssignOp::Add,
            path_expr(&interner, &ty, "x", Primitive::I64),
        )]),
        trailing: None,
    };
    let for_loop = for_expr(&ty, binding_pat(&interner, &ty, "x", Primitive::I64), arr, body);
    let expr = block_expr(
        &ty,
        vec![
            let_stmt(&interner, &ty, BindingMode::Mutable, "sum", lit_int(&ty, 0, Primitive::I64)),
            HirStmt {
                span: Span::DUMMY,
                kind: HirStmtKind::Expr(for_loop),
            },
        ],
        path_expr(&interner, &ty, "sum", Primitive::I64),
        Primitive::I64,
    );
    assert_eq!(int_result(eval_expr(&expr, &mut cx)), 6);
}

#[test]
fn for_over_non_iterable_reports_not_supported() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let body = HirBlock {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::Unit),
        stmts: Box::from([]),
        trailing: None,
    };
    let for_loop = for_expr(&ty, binding_pat(&interner, &ty, "x", Primitive::Bool), lit_bool(&ty, true), body);
    let result = eval_expr(&for_loop, &mut cx);
    assert!(result.is_none());
    assert_eq!(diags.error_count(), 1);
    let d = diags.iter().next().unwrap();
    assert!(d.message.contains("only ranges and arrays iterate"));
}

#[test]
fn for_range_exceeding_iteration_bound_panics() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let body = HirBlock {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::Unit),
        stmts: Box::from([]),
        trailing: None,
    };
    let for_loop = for_expr(
        &ty,
        binding_pat(&interner, &ty, "i", Primitive::I64),
        range_expr(
            &ty,
            lit_int(&ty, 0, Primitive::I64),
            lit_int(&ty, 100_001, Primitive::I64),
            RangeKind::HalfOpen,
        ),
        body,
    );
    let result = eval_expr(&for_loop, &mut cx);
    assert!(result.is_none());
    assert_eq!(diags.error_count(), 1);
    let d = diags.iter().next().unwrap();
    assert!(d.message.contains("exceeded iteration bound"));
}

use std::collections::HashMap;

use ahash::AHashMap;
use edda_resolve::{BindingId, ModuleId};
use edda_types::{EffectEntry, EffectRow, FnSig, Param, ParamMode, PureEffect, ReturnMode};

use crate::fndecl::{FnDeclInfo, FnDeclLookup};

struct TestFnDecls {
    decls: HashMap<BindingId, (edda_intern::Symbol, FnSig, HirBlock)>,
}

impl FnDeclLookup for TestFnDecls {
    fn lookup_fn_decl(&self, binding: BindingId) -> Option<FnDeclInfo<'_>> {
        self.decls
            .get(&binding)
            .map(|(name, sig, body)| FnDeclInfo { name: *name, sig, body })
    }
}

fn fn_sig(ty: &TyInterner, params: Vec<Param>, return_prim: Primitive, effects: EffectRow) -> FnSig {
    FnSig {
        params: params.into_boxed_slice(),
        return_ty: ty.prim(return_prim),
        return_mode: ReturnMode::ByValue,
        effects,
        graded_bounds: Box::from([]),
        refinement_stable: false,
    }
}

fn param(interner: &Interner, ty: &TyInterner, name: &str, prim: Primitive) -> Param {
    Param {
        span: Span::DUMMY,
        name: interner.intern(name),
        mode: ParamMode::Default,
        ty: ty.prim(prim),
    }
}

fn return_stmt_block(ty: &TyInterner, value: HirExpr, prim: Primitive) -> HirBlock {
    HirBlock {
        span: Span::DUMMY,
        ty: ty.prim(prim),
        stmts: Box::from([HirStmt {
            span: Span::DUMMY,
            kind: HirStmtKind::Expr(HirExpr {
                span: Span::DUMMY,
                ty: ty.prim(Primitive::Never),
                kind: HirExprKind::Return(Some(Box::new(value))),
            }),
        }]),
        trailing: None,
    }
}

fn call_at(
    span: Span,
    interner: &Interner,
    ty: &TyInterner,
    callee: &str,
    args: Vec<HirExpr>,
    prim: Primitive,
) -> HirExpr {
    HirExpr {
        span,
        ty: ty.prim(prim),
        kind: HirExprKind::Call {
            callee: Box::new(HirExpr {
                span: Span::DUMMY,
                ty: ty.error(),
                kind: HirExprKind::Path(HirPath {
                    span: Span::DUMMY,
                    segments: Box::from([ident(interner, callee)]),
                }),
            }),
            args: args
                .into_iter()
                .map(|expr| HirCallArg {
                    span: Span::DUMMY,
                    mode: None,
                    name: None,
                    expr,
                })
                .collect(),
        },
    }
}

#[test]
fn user_call_zero_arg_with_return() {
    // The same-file wrapper shape:
    //   function answer() -> i64 { return 41 }
    //   comptime { answer() + 1 }
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let binding = BindingId::new(ModuleId::new(0), 0);
    let decls = TestFnDecls {
        decls: HashMap::from([(
            binding,
            (
                interner.intern("answer"),
                fn_sig(&ty, vec![], Primitive::I64, EffectRow::empty()),
                return_stmt_block(&ty, lit_int(&ty, 41, Primitive::I64), Primitive::I64),
            ),
        )]),
    };
    let call_span = Span::DUMMY;
    let calls: AHashMap<Span, BindingId> = AHashMap::from_iter([(call_span, binding)]);
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags)
        .with_fn_calls(&calls)
        .with_fn_decls(&decls);
    let expr = binary(
        &ty,
        BinOp::Add,
        call_at(call_span, &interner, &ty, "answer", vec![], Primitive::I64),
        lit_int(&ty, 1, Primitive::I64),
        ty.prim(Primitive::I64),
    );
    assert_eq!(int_result(eval_expr(&expr, &mut cx)), 42);
    assert_eq!(cx.env.depth(), 0);
}

#[test]
fn user_call_binds_params_positionally() {
    //   function double(x: i64) -> i64 { return x + x }
    //   comptime { double(21) }
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let binding = BindingId::new(ModuleId::new(0), 0);
    let body = return_stmt_block(
        &ty,
        binary(
            &ty,
            BinOp::Add,
            path_expr(&interner, &ty, "x", Primitive::I64),
            path_expr(&interner, &ty, "x", Primitive::I64),
            ty.prim(Primitive::I64),
        ),
        Primitive::I64,
    );
    let decls = TestFnDecls {
        decls: HashMap::from([(
            binding,
            (
                interner.intern("double"),
                fn_sig(
                    &ty,
                    vec![param(&interner, &ty, "x", Primitive::I64)],
                    Primitive::I64,
                    EffectRow::empty(),
                ),
                body,
            ),
        )]),
    };
    let call_span = Span::DUMMY;
    let calls: AHashMap<Span, BindingId> = AHashMap::from_iter([(call_span, binding)]);
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags)
        .with_fn_calls(&calls)
        .with_fn_decls(&decls);
    let expr = call_at(
        call_span,
        &interner,
        &ty,
        "double",
        vec![lit_int(&ty, 21, Primitive::I64)],
        Primitive::I64,
    );
    assert_eq!(int_result(eval_expr(&expr, &mut cx)), 42);
}

#[test]
fn user_call_trailing_expression_body() {
    //   function three() -> i64 { 3 }   (trailing expr, no `return`)
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let binding = BindingId::new(ModuleId::new(0), 0);
    let body = HirBlock {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::I64),
        stmts: Box::from([]),
        trailing: Some(Box::new(lit_int(&ty, 3, Primitive::I64))),
    };
    let decls = TestFnDecls {
        decls: HashMap::from([(
            binding,
            (
                interner.intern("three"),
                fn_sig(&ty, vec![], Primitive::I64, EffectRow::empty()),
                body,
            ),
        )]),
    };
    let call_span = Span::DUMMY;
    let calls: AHashMap<Span, BindingId> = AHashMap::from_iter([(call_span, binding)]);
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags)
        .with_fn_calls(&calls)
        .with_fn_decls(&decls);
    let expr = call_at(call_span, &interner, &ty, "three", vec![], Primitive::I64);
    assert_eq!(int_result(eval_expr(&expr, &mut cx)), 3);
}

#[test]
fn user_call_impure_callee_rejected() {
    //   function diverging() -> i64 with {divergence} { return 1 }
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let binding = BindingId::new(ModuleId::new(0), 0);
    let decls = TestFnDecls {
        decls: HashMap::from([(
            binding,
            (
                interner.intern("diverging"),
                fn_sig(
                    &ty,
                    vec![],
                    Primitive::I64,
                    EffectRow::from_entries([EffectEntry::Pure(PureEffect::Divergence)]),
                ),
                return_stmt_block(&ty, lit_int(&ty, 1, Primitive::I64), Primitive::I64),
            ),
        )]),
    };
    let call_span = Span::DUMMY;
    let calls: AHashMap<Span, BindingId> = AHashMap::from_iter([(call_span, binding)]);
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags)
        .with_fn_calls(&calls)
        .with_fn_decls(&decls);
    let expr = call_at(call_span, &interner, &ty, "diverging", vec![], Primitive::I64);
    let result = eval_expr(&expr, &mut cx);
    assert!(result.is_none());
    assert_eq!(diags.error_count(), 1);
    let d = diags.iter().next().unwrap();
    assert!(d.message.contains("not comptime-pure"));
}

#[test]
fn user_call_panic_effect_admitted() {
    //   function may_panic() -> i64 with {panic} { return 7 }
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let binding = BindingId::new(ModuleId::new(0), 0);
    let decls = TestFnDecls {
        decls: HashMap::from([(
            binding,
            (
                interner.intern("may_panic"),
                fn_sig(
                    &ty,
                    vec![],
                    Primitive::I64,
                    EffectRow::from_entries([EffectEntry::Pure(PureEffect::Panic)]),
                ),
                return_stmt_block(&ty, lit_int(&ty, 7, Primitive::I64), Primitive::I64),
            ),
        )]),
    };
    let call_span = Span::DUMMY;
    let calls: AHashMap<Span, BindingId> = AHashMap::from_iter([(call_span, binding)]);
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags)
        .with_fn_calls(&calls)
        .with_fn_decls(&decls);
    let expr = call_at(call_span, &interner, &ty, "may_panic", vec![], Primitive::I64);
    assert_eq!(int_result(eval_expr(&expr, &mut cx)), 7);
}

#[test]
fn callee_body_cannot_see_caller_locals() {
    // { let secret = 9; leak() } where leak's body references `secret`
    // — the frame base must make that an unresolved identifier, not 9.
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let binding = BindingId::new(ModuleId::new(0), 0);
    let body = HirBlock {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::I64),
        stmts: Box::from([]),
        trailing: Some(Box::new(path_expr(&interner, &ty, "secret", Primitive::I64))),
    };
    let decls = TestFnDecls {
        decls: HashMap::from([(
            binding,
            (
                interner.intern("leak"),
                fn_sig(&ty, vec![], Primitive::I64, EffectRow::empty()),
                body,
            ),
        )]),
    };
    let call_span = Span::DUMMY;
    let calls: AHashMap<Span, BindingId> = AHashMap::from_iter([(call_span, binding)]);
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags)
        .with_fn_calls(&calls)
        .with_fn_decls(&decls);
    let expr = block_expr(
        &ty,
        vec![let_stmt(
            &interner,
            &ty,
            BindingMode::Immutable,
            "secret",
            lit_int(&ty, 9, Primitive::I64),
        )],
        call_at(call_span, &interner, &ty, "leak", vec![], Primitive::I64),
        Primitive::I64,
    );
    let result = eval_expr(&expr, &mut cx);
    assert!(result.is_none());
    assert_eq!(diags.error_count(), 1);
    let d = diags.iter().next().unwrap();
    assert!(d.message.contains("unresolved identifier `secret`"));
}

#[test]
fn return_outside_function_body_is_diagnosed() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let expr = HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::Never),
        kind: HirExprKind::Return(Some(Box::new(lit_int(&ty, 1, Primitive::I64)))),
    };
    let result = eval_expr(&expr, &mut cx);
    assert!(result.is_none());
    assert_eq!(diags.error_count(), 1);
    let d = diags.iter().next().unwrap();
    assert!(d.message.contains("outside a function-body context"));
}

fn qualified_call_at(
    span: Span,
    interner: &Interner,
    ty: &TyInterner,
    segments: &[&str],
    args: Vec<HirExpr>,
    prim: Primitive,
) -> HirExpr {
    HirExpr {
        span,
        ty: ty.prim(prim),
        kind: HirExprKind::Call {
            callee: Box::new(HirExpr {
                span: Span::DUMMY,
                ty: ty.error(),
                kind: HirExprKind::Path(HirPath {
                    span: Span::DUMMY,
                    segments: segments.iter().map(|s| ident(interner, s)).collect(),
                }),
            }),
            args: args
                .into_iter()
                .map(|expr| HirCallArg {
                    span: Span::DUMMY,
                    mode: None,
                    name: None,
                    expr,
                })
                .collect(),
        },
    }
}

#[test]
fn qualified_callee_resolves_cross_module() {
    // The direct qualified repro shape, spread over two
    // "modules" of one package:
    //   comptime { theme.contrast_ok(light.palette()) }
    // where palette() -> i64 { return 6 } and
    // contrast_ok(r: i64) -> bool { return r > 4 }.
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let palette_binding = BindingId::new(ModuleId::new(1), 0);
    let contrast_binding = BindingId::new(ModuleId::new(2), 0);
    let palette_body = return_stmt_block(&ty, lit_int(&ty, 6, Primitive::I64), Primitive::I64);
    let contrast_body = return_stmt_block(
        &ty,
        binary(
            &ty,
            BinOp::Gt,
            path_expr(&interner, &ty, "r", Primitive::I64),
            lit_int(&ty, 4, Primitive::I64),
            ty.prim(Primitive::Bool),
        ),
        Primitive::Bool,
    );
    let decls = TestFnDecls {
        decls: HashMap::from([
            (
                palette_binding,
                (
                    interner.intern("palette"),
                    fn_sig(&ty, vec![], Primitive::I64, EffectRow::empty()),
                    palette_body,
                ),
            ),
            (
                contrast_binding,
                (
                    interner.intern("contrast_ok"),
                    fn_sig(
                        &ty,
                        vec![param(&interner, &ty, "r", Primitive::I64)],
                        Primitive::Bool,
                        EffectRow::empty(),
                    ),
                    contrast_body,
                ),
            ),
        ]),
    };
    let file = Span::DUMMY.file;
    let outer_span = Span::new(file, edda_span::BytePos(0), edda_span::BytePos(40));
    let inner_span = Span::new(file, edda_span::BytePos(20), edda_span::BytePos(35));
    let calls: AHashMap<Span, BindingId> = AHashMap::from_iter([
        (outer_span, contrast_binding),
        (inner_span, palette_binding),
    ]);
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags)
        .with_fn_calls(&calls)
        .with_fn_decls(&decls);
    let inner = qualified_call_at(
        inner_span,
        &interner,
        &ty,
        &["light", "palette"],
        vec![],
        Primitive::I64,
    );
    let expr = qualified_call_at(
        outer_span,
        &interner,
        &ty,
        &["theme", "contrast_ok"],
        vec![inner],
        Primitive::Bool,
    );
    match eval_expr(&expr, &mut cx).unwrap() {
        Value::Bool(b) => assert!(b),
        other => panic!("expected bool, got {other:?}"),
    }
    assert_eq!(diags.error_count(), 0);
}

#[test]
fn qualified_value_path_still_rejected() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let expr = HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::I64),
        kind: HirExprKind::Path(HirPath {
            span: Span::DUMMY,
            segments: Box::from([ident(&interner, "theme"), ident(&interner, "shade")]),
        }),
    };
    let result = eval_expr(&expr, &mut cx);
    assert!(result.is_none());
    assert_eq!(diags.error_count(), 1);
    let d = diags.iter().next().unwrap();
    assert!(d.message.contains("value position"));
}

#[test]
fn user_call_without_threaded_lookup_reports_not_supported() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let expr = call_at(Span::DUMMY, &interner, &ty, "mystery", vec![], Primitive::I64);
    let result = eval_expr(&expr, &mut cx);
    assert!(result.is_none());
    assert_eq!(diags.error_count(), 1);
    let d = diags.iter().next().unwrap();
    assert!(d.message.contains("comptime call to non-built-in"));
}

fn lit_float(interner: &Interner, ty: &TyInterner, raw: &str, prim: Primitive) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(prim),
        kind: HirExprKind::Literal(Literal::Float(interner.intern(raw))),
    }
}

fn f64_result(v: Option<Value>) -> f64 {
    match v.unwrap() {
        Value::Float(crate::FloatValue::F64(f)) => f,
        other => panic!("expected f64, got {other:?}"),
    }
}

#[test]
fn float_literal_round_trips_at_f64() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let expr = lit_float(&interner, &ty, "2.75", Primitive::F64);
    assert_eq!(f64_result(eval_expr(&expr, &mut cx)), 2.75);
}

#[test]
fn float_literal_round_trips_at_f32() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let expr = lit_float(&interner, &ty, "0.5", Primitive::F32);
    match eval_expr(&expr, &mut cx).unwrap() {
        Value::Float(crate::FloatValue::F32(f)) => assert_eq!(f, 0.5f32),
        other => panic!("expected f32, got {other:?}"),
    }
}

#[test]
fn float_arithmetic_walks_recursively() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    // 1.5 + 2.25 = 3.75
    let expr = binary(
        &ty,
        BinOp::Add,
        lit_float(&interner, &ty, "1.5", Primitive::F64),
        lit_float(&interner, &ty, "2.25", Primitive::F64),
        ty.prim(Primitive::F64),
    );
    assert_eq!(f64_result(eval_expr(&expr, &mut cx)), 3.75);
}

#[test]
fn float_comparison_returns_bool() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let expr = binary(
        &ty,
        BinOp::Lt,
        lit_float(&interner, &ty, "0.5", Primitive::F64),
        lit_float(&interner, &ty, "1.0", Primitive::F64),
        ty.prim(Primitive::Bool),
    );
    match eval_expr(&expr, &mut cx).unwrap() {
        Value::Bool(b) => assert!(b),
        other => panic!("expected bool, got {other:?}"),
    }
}

#[test]
fn float_math_through_user_call() {
    // The WCAG-contrast consumer shape: f64 luminance arithmetic
    // inside a comptime-called predicate.
    //   function contrast(l1: f64, l2: f64) -> bool { return l1 / l2 > 4.5 }
    //   comptime { contrast(10.0, 2.0) }
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let binding = BindingId::new(ModuleId::new(0), 0);
    let body = return_stmt_block(
        &ty,
        binary(
            &ty,
            BinOp::Gt,
            binary(
                &ty,
                BinOp::Div,
                path_expr(&interner, &ty, "l1", Primitive::F64),
                path_expr(&interner, &ty, "l2", Primitive::F64),
                ty.prim(Primitive::F64),
            ),
            lit_float(&interner, &ty, "4.5", Primitive::F64),
            ty.prim(Primitive::Bool),
        ),
        Primitive::Bool,
    );
    let decls = TestFnDecls {
        decls: HashMap::from([(
            binding,
            (
                interner.intern("contrast"),
                fn_sig(
                    &ty,
                    vec![
                        param(&interner, &ty, "l1", Primitive::F64),
                        param(&interner, &ty, "l2", Primitive::F64),
                    ],
                    Primitive::Bool,
                    EffectRow::empty(),
                ),
                body,
            ),
        )]),
    };
    let call_span = Span::DUMMY;
    let calls: AHashMap<Span, BindingId> = AHashMap::from_iter([(call_span, binding)]);
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags)
        .with_fn_calls(&calls)
        .with_fn_decls(&decls);
    let expr = call_at(
        call_span,
        &interner,
        &ty,
        "contrast",
        vec![
            lit_float(&interner, &ty, "10.0", Primitive::F64),
            lit_float(&interner, &ty, "2.0", Primitive::F64),
        ],
        Primitive::Bool,
    );
    match eval_expr(&expr, &mut cx).unwrap() {
        Value::Bool(b) => assert!(b),
        other => panic!("expected bool, got {other:?}"),
    }
}

#[test]
fn float_literal_with_non_float_type_is_diagnosed() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let expr = lit_float(&interner, &ty, "1.5", Primitive::I64);
    let result = eval_expr(&expr, &mut cx);
    assert!(result.is_none());
    assert_eq!(diags.error_count(), 1);
    let d = diags.iter().next().unwrap();
    assert!(d.message.contains("non-float type"));
}

#[test]
fn return_inside_for_loop_unwinds_to_call_frame() {
    // A `return` inside a `for` body
    // must unwind through the loop via `cx.pending_return`, not report
    // a spurious failure or keep iterating.
    //   function first(xs: [i64]) -> i64 {
    //       for x in xs { return x }
    //       return 0
    //   }
    //   comptime { first() }  (xs is embedded as a literal for the test)
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let binding = BindingId::new(ModuleId::new(0), 0);
    let for_body = HirBlock {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::Unit),
        stmts: Box::from([HirStmt {
            span: Span::DUMMY,
            kind: HirStmtKind::Expr(HirExpr {
                span: Span::DUMMY,
                ty: ty.prim(Primitive::Never),
                kind: HirExprKind::Return(Some(Box::new(path_expr(&interner, &ty, "x", Primitive::I64)))),
            }),
        }]),
        trailing: None,
    };
    let for_loop = for_expr(
        &ty,
        binding_pat(&interner, &ty, "x", Primitive::I64),
        array_expr(
            &ty,
            vec![
                lit_int(&ty, 7, Primitive::I64),
                lit_int(&ty, 8, Primitive::I64),
                lit_int(&ty, 9, Primitive::I64),
            ],
            Primitive::I64,
        ),
        for_body,
    );
    let fn_body = HirBlock {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::I64),
        stmts: Box::from([HirStmt {
            span: Span::DUMMY,
            kind: HirStmtKind::Expr(for_loop),
        }]),
        trailing: Some(Box::new(lit_int(&ty, 0, Primitive::I64))),
    };
    let decls = TestFnDecls {
        decls: HashMap::from([(
            binding,
            (
                interner.intern("first"),
                fn_sig(&ty, vec![], Primitive::I64, EffectRow::empty()),
                fn_body,
            ),
        )]),
    };
    let call_span = Span::DUMMY;
    let calls: AHashMap<Span, BindingId> = AHashMap::from_iter([(call_span, binding)]);
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags)
        .with_fn_calls(&calls)
        .with_fn_decls(&decls);
    let expr = call_at(call_span, &interner, &ty, "first", vec![], Primitive::I64);
    assert_eq!(int_result(eval_expr(&expr, &mut cx)), 7);
    assert_eq!(cx.env.depth(), 0);
}

fn struct_field(interner: &Interner, name: &str, value: HirExpr) -> HirStructLitField {
    HirStructLitField {
        span: Span::DUMMY,
        name: Ident {
            name: interner.intern(name),
            span: Span::DUMMY,
        },
        mode: None,
        value,
    }
}

#[test]
fn struct_literal_constructs_record() {
    // A `Path { field: e, ... }` struct
    // literal reduces to a `Value::Record` with each field initialiser
    // comptime-evaluated, in source order.
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let expr = HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::Unit),
        kind: HirExprKind::StructLit {
            path: HirPath {
                span: Span::DUMMY,
                segments: Box::from([Ident {
                    name: interner.intern("Point"),
                    span: Span::DUMMY,
                }]),
            },
            fields: Box::from([
                struct_field(&interner, "x", lit_int(&ty, 3, Primitive::I64)),
                struct_field(&interner, "y", lit_int(&ty, 4, Primitive::I64)),
            ]),
        },
    };
    let v = eval_expr(&expr, &mut cx).unwrap();
    match v {
        Value::Record(entries) => {
            assert_eq!(entries.len(), 2);
            assert_eq!(interner.resolve(entries[0].0), "x");
            assert_eq!(interner.resolve(entries[1].0), "y");
            match (&entries[0].1, &entries[1].1) {
                (Value::Int(a), Value::Int(b)) => {
                    assert_eq!(a.as_i128(), Some(3));
                    assert_eq!(b.as_i128(), Some(4));
                }
                other => panic!("expected Int fields, got {other:?}"),
            }
        }
        other => panic!("expected Record, got {other:?}"),
    }
    assert!(diags.is_empty());
}

#[test]
fn field_read_projects_record_entry() {
    // A `receiver.field` access over a
    // `Value::Record` (constructed by a struct literal) projects the
    // named entry back out, in declared field order.
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let record = HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::Unit),
        kind: HirExprKind::StructLit {
            path: HirPath {
                span: Span::DUMMY,
                segments: Box::from([Ident {
                    name: interner.intern("Point"),
                    span: Span::DUMMY,
                }]),
            },
            fields: Box::from([
                struct_field(&interner, "x", lit_int(&ty, 3, Primitive::I64)),
                struct_field(&interner, "y", lit_int(&ty, 4, Primitive::I64)),
            ]),
        },
    };
    let field = HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::I64),
        kind: HirExprKind::Field {
            receiver: Box::new(record),
            name: Ident {
                name: interner.intern("y"),
                span: Span::DUMMY,
            },
        },
    };
    assert_eq!(int_result(eval_expr(&field, &mut cx)), 4);
    assert!(diags.is_empty());
}

// ---- comptime loop / break / continue ----

fn expr_stmt(e: HirExpr) -> HirStmt {
    HirStmt { span: Span::DUMMY, kind: HirStmtKind::Expr(e) }
}

fn plain_block(
    ty: &TyInterner,
    stmts: Vec<HirStmt>,
    trailing: Option<HirExpr>,
    prim: Primitive,
) -> HirBlock {
    HirBlock {
        span: Span::DUMMY,
        ty: ty.prim(prim),
        stmts: stmts.into_boxed_slice(),
        trailing: trailing.map(Box::new),
    }
}

fn loop_expr(ty: &TyInterner, body: HirBlock, prim: Primitive) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(prim),
        kind: HirExprKind::Loop { body, label: None, decreases: None },
    }
}

fn break_expr(ty: &TyInterner, value: Option<HirExpr>) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::Never),
        kind: HirExprKind::Break { label: None, value: value.map(Box::new) },
    }
}

fn continue_expr(ty: &TyInterner) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::Never),
        kind: HirExprKind::Continue { label: None },
    }
}

fn if_no_else(ty: &TyInterner, cond: HirExpr, then_block: HirBlock) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::Unit),
        kind: HirExprKind::If {
            cond: Box::new(cond),
            then_block,
            else_branch: None,
        },
    }
}

#[test]
fn comptime_loop_break_yields_value() {
    // loop { break 7 }  ==> 7
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let body = plain_block(
        &ty,
        vec![],
        Some(break_expr(&ty, Some(lit_int(&ty, 7, Primitive::I64)))),
        Primitive::I64,
    );
    let expr = loop_expr(&ty, body, Primitive::I64);
    assert_eq!(int_result(eval_expr(&expr, &mut cx)), 7);
    assert_eq!(cx.env.depth(), 0);
    assert!(diags.is_empty());
}

#[test]
fn comptime_loop_counts_to_bound_via_break() {
    // var i = 0
    // loop { if i >= 5 { break }  i = i + 1 }
    // i   ==> 5
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let guard = if_no_else(
        &ty,
        binary(
            &ty,
            BinOp::Ge,
            path_expr(&interner, &ty, "i", Primitive::I64),
            lit_int(&ty, 5, Primitive::I64),
            ty.prim(Primitive::Bool),
        ),
        plain_block(&ty, vec![expr_stmt(break_expr(&ty, None))], None, Primitive::Unit),
    );
    let incr = assign_stmt(
        &interner,
        &ty,
        "i",
        AssignOp::Plain,
        binary(
            &ty,
            BinOp::Add,
            path_expr(&interner, &ty, "i", Primitive::I64),
            lit_int(&ty, 1, Primitive::I64),
            ty.prim(Primitive::I64),
        ),
    );
    let loop_body = plain_block(&ty, vec![expr_stmt(guard), incr], None, Primitive::Unit);
    let outer = block_expr(
        &ty,
        vec![
            let_stmt(&interner, &ty, BindingMode::Mutable, "i", lit_int(&ty, 0, Primitive::I64)),
            expr_stmt(loop_expr(&ty, loop_body, Primitive::Unit)),
        ],
        path_expr(&interner, &ty, "i", Primitive::I64),
        Primitive::I64,
    );
    assert_eq!(int_result(eval_expr(&outer, &mut cx)), 5);
    assert_eq!(cx.env.depth(), 0);
    assert!(diags.is_empty());
}

#[test]
fn comptime_loop_continue_skips_rest_of_body() {
    // var sum = 0
    // var i = 0
    // loop {
    //     if i >= 5 { break }
    //     i = i + 1
    //     if i == 3 { continue }
    //     sum = sum + i
    // }
    // sum   ==> 1 + 2 + 4 + 5 = 12
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let break_if = if_no_else(
        &ty,
        binary(
            &ty,
            BinOp::Ge,
            path_expr(&interner, &ty, "i", Primitive::I64),
            lit_int(&ty, 5, Primitive::I64),
            ty.prim(Primitive::Bool),
        ),
        plain_block(&ty, vec![expr_stmt(break_expr(&ty, None))], None, Primitive::Unit),
    );
    let incr = assign_stmt(
        &interner,
        &ty,
        "i",
        AssignOp::Plain,
        binary(
            &ty,
            BinOp::Add,
            path_expr(&interner, &ty, "i", Primitive::I64),
            lit_int(&ty, 1, Primitive::I64),
            ty.prim(Primitive::I64),
        ),
    );
    let continue_if = if_no_else(
        &ty,
        binary(
            &ty,
            BinOp::Eq,
            path_expr(&interner, &ty, "i", Primitive::I64),
            lit_int(&ty, 3, Primitive::I64),
            ty.prim(Primitive::Bool),
        ),
        plain_block(&ty, vec![expr_stmt(continue_expr(&ty))], None, Primitive::Unit),
    );
    let accumulate = assign_stmt(
        &interner,
        &ty,
        "sum",
        AssignOp::Plain,
        binary(
            &ty,
            BinOp::Add,
            path_expr(&interner, &ty, "sum", Primitive::I64),
            path_expr(&interner, &ty, "i", Primitive::I64),
            ty.prim(Primitive::I64),
        ),
    );
    let loop_body = plain_block(
        &ty,
        vec![expr_stmt(break_if), incr, expr_stmt(continue_if), accumulate],
        None,
        Primitive::Unit,
    );
    let outer = block_expr(
        &ty,
        vec![
            let_stmt(&interner, &ty, BindingMode::Mutable, "sum", lit_int(&ty, 0, Primitive::I64)),
            let_stmt(&interner, &ty, BindingMode::Mutable, "i", lit_int(&ty, 0, Primitive::I64)),
            expr_stmt(loop_expr(&ty, loop_body, Primitive::Unit)),
        ],
        path_expr(&interner, &ty, "sum", Primitive::I64),
        Primitive::I64,
    );
    assert_eq!(int_result(eval_expr(&outer, &mut cx)), 12);
    assert_eq!(cx.env.depth(), 0);
    assert!(diags.is_empty());
}

#[test]
fn comptime_for_with_break_stops_early() {
    // var acc = 0
    // for i in 0..<10 { if i == 3 { break }  acc = acc + i }
    // acc   ==> 0 + 1 + 2 = 3
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let break_if = if_no_else(
        &ty,
        binary(
            &ty,
            BinOp::Eq,
            path_expr(&interner, &ty, "i", Primitive::I64),
            lit_int(&ty, 3, Primitive::I64),
            ty.prim(Primitive::Bool),
        ),
        plain_block(&ty, vec![expr_stmt(break_expr(&ty, None))], None, Primitive::Unit),
    );
    let accumulate = assign_stmt(
        &interner,
        &ty,
        "acc",
        AssignOp::Plain,
        binary(
            &ty,
            BinOp::Add,
            path_expr(&interner, &ty, "acc", Primitive::I64),
            path_expr(&interner, &ty, "i", Primitive::I64),
            ty.prim(Primitive::I64),
        ),
    );
    let for_body = plain_block(&ty, vec![expr_stmt(break_if), accumulate], None, Primitive::Unit);
    let for_loop = for_expr(
        &ty,
        binding_pat(&interner, &ty, "i", Primitive::I64),
        range_expr(
            &ty,
            lit_int(&ty, 0, Primitive::I64),
            lit_int(&ty, 10, Primitive::I64),
            RangeKind::HalfOpen,
        ),
        for_body,
    );
    let outer = block_expr(
        &ty,
        vec![
            let_stmt(&interner, &ty, BindingMode::Mutable, "acc", lit_int(&ty, 0, Primitive::I64)),
            expr_stmt(for_loop),
        ],
        path_expr(&interner, &ty, "acc", Primitive::I64),
        Primitive::I64,
    );
    assert_eq!(int_result(eval_expr(&outer, &mut cx)), 3);
    assert_eq!(cx.env.depth(), 0);
    assert!(diags.is_empty());
}

#[test]
fn comptime_break_outside_loop_is_diagnostic() {
    // break   (no enclosing loop) ==> comptime panic, no value
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags);
    let expr = break_expr(&ty, None);
    assert!(eval_expr(&expr, &mut cx).is_none());
    assert_eq!(diags.error_count(), 1);
    let d = diags.iter().next().unwrap();
    assert!(d.message.contains("outside a loop"));
}

#[test]
fn comptime_return_unwinds_through_loop_to_call_frame() {
    // function f() -> i64 { loop { return 9 } }
    // comptime { f() }  ==> 9
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let binding = BindingId::new(ModuleId::new(0), 0);
    let loop_body = plain_block(
        &ty,
        vec![expr_stmt(HirExpr {
            span: Span::DUMMY,
            ty: ty.prim(Primitive::Never),
            kind: HirExprKind::Return(Some(Box::new(lit_int(&ty, 9, Primitive::I64)))),
        })],
        None,
        Primitive::Unit,
    );
    let fn_body = HirBlock {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::I64),
        stmts: Box::from([expr_stmt(loop_expr(&ty, loop_body, Primitive::Unit))]),
        trailing: None,
    };
    let decls = TestFnDecls {
        decls: HashMap::from([(
            binding,
            (
                interner.intern("f"),
                fn_sig(&ty, vec![], Primitive::I64, EffectRow::empty()),
                fn_body,
            ),
        )]),
    };
    let call_span = Span::DUMMY;
    let calls: AHashMap<Span, BindingId> = AHashMap::from_iter([(call_span, binding)]);
    let mut diags = Diagnostics::new();
    let mut cx = EvalCx::new(&ty, &target, &interner, &mut diags)
        .with_fn_calls(&calls)
        .with_fn_decls(&decls);
    let expr = call_at(call_span, &interner, &ty, "f", vec![], Primitive::I64);
    assert_eq!(int_result(eval_expr(&expr, &mut cx)), 9);
    assert_eq!(cx.env.depth(), 0);
    assert!(diags.is_empty());
}
