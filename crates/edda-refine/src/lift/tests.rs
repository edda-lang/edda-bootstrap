//! End-to-end tests for the AST → [`Predicate`](crate::Predicate) lifter.
//!
//! Each test builds a synthetic [`PredicateEnv`] impl ([`TestEnv`]) and
//! exercises the full [`lift_predicate`] dispatcher. The lifter is the
//! typechecker integration seam, so these tests double as the contract the
//! typechecker's `TyCx`-backed env must satisfy.

use std::collections::HashMap;

use smol_str::SmolStr;

use edda_intern::{Interner, Symbol};
use edda_span::{BytePos, Span};
use edda_syntax::ast::{
    self, BinOp, Expr, ExprKind, Ident, Literal, Path, UnOp,
};
use edda_syntax::IntBase;

use crate::error::LiftError;
use crate::predicate::{ArithOp, CmpOp, IntLit, IntLitValue, Predicate};
use crate::sort::{FieldRef, IntSort, IntWidth, RecordRef, Sort};

use super::{lift_predicate, PredicateEnv};

// Synthetic env used by every test — pre-populates path / field / type /
// ident lookups.
struct TestEnv {
    interner: Interner,
    paths: HashMap<Span, (SmolStr, Sort)>,
    expr_sorts: HashMap<Span, Sort>,
    fields: HashMap<(Sort, Symbol), FieldRef>,
    type_sorts: HashMap<Span, Sort>,
}

impl TestEnv {
    fn new() -> Self {
        TestEnv {
            interner: Interner::new(),
            paths: HashMap::new(),
            expr_sorts: HashMap::new(),
            fields: HashMap::new(),
            type_sorts: HashMap::new(),
        }
    }

    fn ident(&mut self, text: &str) -> Symbol {
        self.interner.intern(text)
    }

    fn with_path(mut self, span: Span, name: &str, sort: Sort) -> Self {
        self.paths.insert(span, (SmolStr::new(name), sort));
        self
    }

    fn with_expr_sort(mut self, span: Span, sort: Sort) -> Self {
        self.expr_sorts.insert(span, sort);
        self
    }

    fn with_field(mut self, base_sort: Sort, field_sym: Symbol, field_ref: FieldRef) -> Self {
        self.fields.insert((base_sort, field_sym), field_ref);
        self
    }
}

impl PredicateEnv for TestEnv {
    fn lookup_path(&self, span: Span) -> Option<(SmolStr, Sort)> {
        self.paths.get(&span).cloned()
    }

    fn expr_sort(&self, expr: &Expr) -> Option<Sort> {
        self.expr_sorts.get(&expr.span).cloned()
    }

    fn lookup_field(&self, base_sort: &Sort, field: &Ident) -> Option<FieldRef> {
        self.fields.get(&(base_sort.clone(), field.name)).cloned()
    }

    fn type_sort(&self, ty: &ast::Type) -> Option<Sort> {
        self.type_sorts.get(&ty.span).cloned()
    }

    fn ident_name(&self, ident: &Ident) -> SmolStr {
        SmolStr::new(self.interner.resolve(ident.name))
    }
}

// Hand-build a Span — Span::DUMMY collides for every fixture, so we
// synthesize fresh spans by raw lo/hi byte offsets against a sentinel
// FileId.
fn span_at(lo: u32, hi: u32) -> Span {
    // Use the dummy FileId — every test span shares it; we discriminate
    // on the (lo, hi) pair so HashMap lookups work.
    Span::new(Span::DUMMY.file, BytePos(lo), BytePos(hi))
}

fn int_lit_expr(value: u128, span: Span) -> Expr {
    Expr {
        span,
        kind: ExprKind::Literal(Literal::Int {
            value,
            base: IntBase::Dec,
        }),
    }
}

fn bool_lit_expr(value: bool, span: Span) -> Expr {
    Expr {
        span,
        kind: ExprKind::Literal(Literal::Bool(value)),
    }
}

fn path_expr(text: &str, env: &mut TestEnv, span: Span) -> Expr {
    let sym = env.ident(text);
    Expr {
        span,
        kind: ExprKind::Path(Path {
            segments: vec![Ident { name: sym, span }],
            span,
        }),
    }
}

fn binop_expr(op: BinOp, lhs: Expr, rhs: Expr, span: Span) -> Expr {
    Expr {
        span,
        kind: ExprKind::Binary {
            op,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
        },
    }
}

fn i32_sort() -> IntSort {
    IntSort::sized(IntWidth::W32, true)
}

#[test]
fn lift_integer_literal_uses_env_inferred_sort() {
    let i32_s = i32_sort();
    let span = span_at(0, 1);
    let env = TestEnv::new().with_expr_sort(span, Sort::Int(i32_s));
    let e = int_lit_expr(7, span);
    match lift_predicate(&e, &env).unwrap() {
        Predicate::IntLit(IntLit {
            value: IntLitValue::Signed(7),
            sort,
        }) => assert_eq!(sort, i32_s),
        other => panic!("unexpected predicate: {other:?}"),
    }
}

#[test]
fn lift_bool_literal_is_trivial() {
    let env = TestEnv::new();
    let e = bool_lit_expr(true, span_at(0, 4));
    assert!(matches!(lift_predicate(&e, &env), Ok(Predicate::BoolLit(true))));
}

#[test]
fn lift_float_literal_routes_to_unsupported() {
    let env = TestEnv::new();
    let span = span_at(0, 3);
    let e = Expr {
        span,
        kind: ExprKind::Literal(Literal::Float(env.interner.intern("0.0"))),
    };
    let err = lift_predicate(&e, &env).unwrap_err();
    assert!(matches!(err, LiftError::Unsupported { .. }), "err: {err:?}");
}

#[test]
fn lift_path_uses_env_lookup() {
    let span = span_at(0, 3);
    let env = TestEnv::new().with_path(span, "den", Sort::Int(i32_sort()));
    let mut env = env;
    let e = path_expr("den", &mut env, span);
    match lift_predicate(&e, &env).unwrap() {
        Predicate::Var(v) => {
            assert_eq!(v.name.as_str(), "den");
            assert_eq!(v.sort, Sort::Int(i32_sort()));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn lift_addition_recurses_into_both_sides() {
    let i32_s = i32_sort();
    let lhs_span = span_at(0, 1);
    let rhs_span = span_at(4, 5);
    let bin_span = span_at(0, 5);
    let mut env = TestEnv::new()
        .with_path(lhs_span, "x", Sort::Int(i32_s))
        .with_expr_sort(rhs_span, Sort::Int(i32_s));
    let lhs = path_expr("x", &mut env, lhs_span);
    let rhs = int_lit_expr(1, rhs_span);
    let e = binop_expr(BinOp::Add, lhs, rhs, bin_span);
    let p = lift_predicate(&e, &env).unwrap();
    assert!(matches!(p, Predicate::Arith { op: ArithOp::Add, .. }));
}

#[test]
fn lift_non_literal_multiplication_is_rejected() {
    let i32_s = i32_sort();
    let lhs_span = span_at(0, 1);
    let rhs_span = span_at(4, 5);
    let bin_span = span_at(0, 5);
    let mut env = TestEnv::new()
        .with_path(lhs_span, "x", Sort::Int(i32_s))
        .with_path(rhs_span, "y", Sort::Int(i32_s));
    let lhs = path_expr("x", &mut env, lhs_span);
    let rhs = path_expr("y", &mut env, rhs_span);
    let e = binop_expr(BinOp::Mul, lhs, rhs, bin_span);
    match lift_predicate(&e, &env).unwrap_err() {
        LiftError::Unsupported { what, .. } => {
            assert!(what.contains("non-linear"), "{what}");
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn lift_literal_times_var_uses_mul_lit() {
    let i32_s = i32_sort();
    let lit_span = span_at(0, 1);
    let var_span = span_at(4, 5);
    let bin_span = span_at(0, 5);
    let mut env = TestEnv::new()
        .with_expr_sort(lit_span, Sort::Int(i32_s))
        .with_path(var_span, "x", Sort::Int(i32_s));
    let lhs = int_lit_expr(2, lit_span);
    let rhs = path_expr("x", &mut env, var_span);
    let e = binop_expr(BinOp::Mul, lhs, rhs, bin_span);
    let p = lift_predicate(&e, &env).unwrap();
    assert!(matches!(p, Predicate::MulLit { .. }));
}

#[test]
fn lift_non_literal_modulo_is_rejected() {
    let i32_s = i32_sort();
    let lhs_span = span_at(0, 1);
    let rhs_span = span_at(4, 5);
    let bin_span = span_at(0, 5);
    let mut env = TestEnv::new()
        .with_path(lhs_span, "x", Sort::Int(i32_s))
        .with_path(rhs_span, "y", Sort::Int(i32_s));
    let lhs = path_expr("x", &mut env, lhs_span);
    let rhs = path_expr("y", &mut env, rhs_span);
    let e = binop_expr(BinOp::Mod, lhs, rhs, bin_span);
    let err = lift_predicate(&e, &env).unwrap_err();
    assert!(matches!(err, LiftError::Unsupported { .. }));
}

#[test]
fn lift_var_modulo_literal_uses_mod_lit() {
    let i32_s = i32_sort();
    let var_span = span_at(0, 1);
    let lit_span = span_at(4, 5);
    let bin_span = span_at(0, 5);
    let mut env = TestEnv::new()
        .with_path(var_span, "x", Sort::Int(i32_s))
        .with_expr_sort(lit_span, Sort::Int(i32_s));
    let lhs = path_expr("x", &mut env, var_span);
    let rhs = int_lit_expr(2, lit_span);
    let e = binop_expr(BinOp::Mod, lhs, rhs, bin_span);
    let p = lift_predicate(&e, &env).unwrap();
    assert!(matches!(p, Predicate::ModLit { .. }));
}

#[test]
fn lift_comparison_yields_cmp_predicate() {
    let i32_s = i32_sort();
    let lhs_span = span_at(0, 1);
    let rhs_span = span_at(4, 5);
    let bin_span = span_at(0, 5);
    let mut env = TestEnv::new()
        .with_path(lhs_span, "x", Sort::Int(i32_s))
        .with_expr_sort(rhs_span, Sort::Int(i32_s));
    let lhs = path_expr("x", &mut env, lhs_span);
    let rhs = int_lit_expr(0, rhs_span);
    let e = binop_expr(BinOp::Ne, lhs, rhs, bin_span);
    match lift_predicate(&e, &env).unwrap() {
        Predicate::Cmp {
            op: CmpOp::Ne, ..
        } => {}
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn lift_bitwise_op_is_rejected() {
    let i32_s = i32_sort();
    let lhs_span = span_at(0, 1);
    let rhs_span = span_at(4, 5);
    let bin_span = span_at(0, 5);
    let mut env = TestEnv::new()
        .with_path(lhs_span, "x", Sort::Int(i32_s))
        .with_path(rhs_span, "y", Sort::Int(i32_s));
    let lhs = path_expr("x", &mut env, lhs_span);
    let rhs = path_expr("y", &mut env, rhs_span);
    let e = binop_expr(BinOp::BitAnd, lhs, rhs, bin_span);
    let err = lift_predicate(&e, &env).unwrap_err();
    assert!(matches!(err, LiftError::NotAdmittedInPredicate { .. }));
}

#[test]
fn lift_slice_len_method_call_uses_slice_len_predicate() {
    let i64_s = IntSort::sized(IntWidth::W64, true);
    let slice_sort = Sort::slice(Sort::Int(i64_s));
    let receiver_span = span_at(0, 2);
    let name_span = span_at(3, 6);
    let call_span = span_at(0, 8);
    let mut env = TestEnv::new().with_path(receiver_span, "xs", slice_sort.clone());
    let receiver = path_expr("xs", &mut env, receiver_span);
    let len_sym = env.ident("len");
    let e = Expr {
        span: call_span,
        kind: ExprKind::MethodCall {
            receiver: Box::new(receiver),
            name: Ident {
                name: len_sym,
                span: name_span,
            },
            args: Vec::new(),
        },
    };
    match lift_predicate(&e, &env).unwrap() {
        Predicate::SliceLen { .. } => {}
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn lift_method_call_other_than_len_is_rejected() {
    let i64_s = IntSort::sized(IntWidth::W64, true);
    let slice_sort = Sort::slice(Sort::Int(i64_s));
    let recv_span = span_at(0, 2);
    let name_span = span_at(3, 8);
    let call_span = span_at(0, 10);
    let mut env = TestEnv::new().with_path(recv_span, "xs", slice_sort);
    let receiver = path_expr("xs", &mut env, recv_span);
    let other_sym = env.ident("first");
    let e = Expr {
        span: call_span,
        kind: ExprKind::MethodCall {
            receiver: Box::new(receiver),
            name: Ident {
                name: other_sym,
                span: name_span,
            },
            args: Vec::new(),
        },
    };
    assert!(matches!(
        lift_predicate(&e, &env).unwrap_err(),
        LiftError::NotAdmittedInPredicate { .. }
    ));
}

#[test]
fn lift_field_access_uses_env_lookup_field() {
    let record_sort = Sort::Record(RecordRef::new("Point"));
    let recv_span = span_at(0, 1);
    let name_span = span_at(2, 3);
    let field_span = span_at(0, 3);
    let mut env = TestEnv::new().with_path(recv_span, "p", record_sort.clone());
    let x_sym = env.ident("x");
    let env = env.with_field(
        record_sort.clone(),
        x_sym,
        FieldRef::new(RecordRef::new("Point"), "x", Sort::Int(i32_sort())),
    );
    let mut env = env;
    let receiver = path_expr("p", &mut env, recv_span);
    let e = Expr {
        span: field_span,
        kind: ExprKind::Field {
            receiver: Box::new(receiver),
            name: Ident {
                name: x_sym,
                span: name_span,
            },
        },
    };
    let p = lift_predicate(&e, &env).unwrap();
    assert!(matches!(p, Predicate::FieldProj { .. }));
}

#[test]
fn lift_call_is_rejected_as_not_admitted_in_phase_1() {
    let env = TestEnv::new();
    let span = span_at(0, 3);
    let e = Expr {
        span,
        kind: ExprKind::Call {
            callee: Box::new(Expr {
                span: span_at(0, 1),
                kind: ExprKind::Path(Path {
                    segments: Vec::new(),
                    span,
                }),
            }),
            args: Vec::new(),
        },
    };
    match lift_predicate(&e, &env).unwrap_err() {
        LiftError::NotAdmittedInPredicate { form, .. } => assert_eq!(form, "user-function call"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn lift_unary_not_yields_not_predicate() {
    let env = TestEnv::new();
    let inner_span = span_at(0, 4);
    let span = span_at(0, 5);
    let e = Expr {
        span,
        kind: ExprKind::Unary {
            op: UnOp::Not,
            expr: Box::new(bool_lit_expr(true, inner_span)),
        },
    };
    let p = lift_predicate(&e, &env).unwrap();
    assert!(matches!(p, Predicate::Not(_)));
}

#[test]
fn lift_bit_not_is_rejected() {
    let i32_s = i32_sort();
    let inner_span = span_at(0, 1);
    let span = span_at(0, 2);
    let mut env = TestEnv::new().with_path(inner_span, "x", Sort::Int(i32_s));
    let inner = path_expr("x", &mut env, inner_span);
    let e = Expr {
        span,
        kind: ExprKind::Unary {
            op: UnOp::BitNot,
            expr: Box::new(inner),
        },
    };
    assert!(matches!(
        lift_predicate(&e, &env).unwrap_err(),
        LiftError::NotAdmittedInPredicate { .. }
    ));
}

#[test]
fn lift_path_with_no_env_entry_is_unresolved() {
    let env = TestEnv::new();
    let mut env = env;
    let span = span_at(0, 3);
    let e = path_expr("missing", &mut env, span);
    assert!(matches!(
        lift_predicate(&e, &env).unwrap_err(),
        LiftError::UnresolvedPath { .. }
    ));
}
