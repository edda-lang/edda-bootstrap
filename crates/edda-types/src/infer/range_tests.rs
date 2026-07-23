//! Tests for the implicit-spec invocation of
//! `std.core.range.Range(<T>)` from a range-literal use site.
//!
//! The synthesised value type is the error sentinel until codegen
//! materialises the generated `Range_<T>` nominal; these tests
//! verify the element-type inference rules and the
//! [`InferCx::implicit_specs`] registration.

use edda_diag::DiagnosticClass;
use edda_intern::Interner;
use edda_span::Span;
use edda_syntax::IntBase;
use edda_syntax::ast::{Ident, Literal, RangeKind};

use super::synth_expr;
use crate::cx::TyCx;
use crate::hir::{HirExpr, HirExprKind, HirPath};
use crate::implicit_spec::ImplicitSpec;
use crate::infer::{InferCx, TyEnv};
use crate::lower::LowerCx;
use crate::prim::Primitive;
use crate::ty::TyId;

fn int_lit(ty: TyId, value: u128) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty,
        kind: HirExprKind::Literal(Literal::Int {
            value,
            base: IntBase::Dec,
        }),
    }
}

fn path_one(interner: &Interner, name: &str, ty: TyId) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty,
        kind: HirExprKind::Path(HirPath {
            span: Span::DUMMY,
            segments: Box::from([Ident {
                name: interner.intern(name),
                span: Span::DUMMY,
            }]),
        }),
    }
}

fn range_expr(lo: HirExpr, hi: HirExpr, ty: TyId) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty,
        kind: HirExprKind::Range {
            lo: Some(Box::new(lo)),
            hi: Some(Box::new(hi)),
            kind: RangeKind::HalfOpen,
        },
    }
}

#[test]
fn range_both_int_literals_defaults_to_i64() {
    let mut h = crate::test_support::Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let mut env = TyEnv::new();
    let i64 = h.ty_interner.prim(Primitive::I64);

    let mut r = range_expr(int_lit(i64, 0), int_lit(i64, 10), h.ty_interner.error());
    let _ = synth_expr(&mut env, &mut r, &mut ic);

    assert_eq!(ic.implicit_specs.len(), 1);
    assert_eq!(ic.implicit_specs[0].kind, ImplicitSpec::Range);
    assert_eq!(ic.implicit_specs[0].type_arg, i64);
}

#[test]
fn range_lo_typed_hi_literal_uses_lo_type() {
    // `n..<10` where n: usize → element type usize.
    let mut h = crate::test_support::Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let mut env = TyEnv::new();
    let usize_ty = h.ty_interner.prim(Primitive::Usize);
    let n_sym = h.interner.intern("n");
    env.bind(n_sym, usize_ty);

    let mut r = range_expr(
        path_one(&h.interner, "n", usize_ty),
        int_lit(usize_ty, 10),
        h.ty_interner.error(),
    );
    let _ = synth_expr(&mut env, &mut r, &mut ic);

    assert_eq!(ic.implicit_specs.len(), 1);
    assert_eq!(ic.implicit_specs[0].type_arg, usize_ty);
}

#[test]
fn range_lo_literal_hi_typed_uses_hi_type() {
    // `0..<n` where n: usize → element type usize.
    let mut h = crate::test_support::Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let mut env = TyEnv::new();
    let usize_ty = h.ty_interner.prim(Primitive::Usize);
    let n_sym = h.interner.intern("n");
    env.bind(n_sym, usize_ty);

    let mut r = range_expr(
        int_lit(usize_ty, 0),
        path_one(&h.interner, "n", usize_ty),
        h.ty_interner.error(),
    );
    let _ = synth_expr(&mut env, &mut r, &mut ic);

    assert_eq!(ic.implicit_specs.len(), 1);
    assert_eq!(ic.implicit_specs[0].type_arg, usize_ty);
}

#[test]
fn range_both_typed_endpoints_must_agree() {
    let mut h = crate::test_support::Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let mut env = TyEnv::new();
    let i32_ty = h.ty_interner.prim(Primitive::I32);
    let lo_sym = h.interner.intern("lo");
    let hi_sym = h.interner.intern("hi");
    env.bind(lo_sym, i32_ty);
    env.bind(hi_sym, i32_ty);

    let mut r = range_expr(
        path_one(&h.interner, "lo", i32_ty),
        path_one(&h.interner, "hi", i32_ty),
        h.ty_interner.error(),
    );
    let _ = synth_expr(&mut env, &mut r, &mut ic);

    assert_eq!(ic.implicit_specs.len(), 1);
    assert_eq!(ic.implicit_specs[0].type_arg, i32_ty);
}

#[test]
fn range_mismatched_typed_endpoints_diagnose() {
    let mut h = crate::test_support::Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let mut env = TyEnv::new();
    let i32_ty = h.ty_interner.prim(Primitive::I32);
    let i64_ty = h.ty_interner.prim(Primitive::I64);
    let lo_sym = h.interner.intern("lo");
    let hi_sym = h.interner.intern("hi");
    env.bind(lo_sym, i32_ty);
    env.bind(hi_sym, i64_ty);

    let mut r = range_expr(
        path_one(&h.interner, "lo", i32_ty),
        path_one(&h.interner, "hi", i64_ty),
        h.ty_interner.error(),
    );
    let _ = synth_expr(&mut env, &mut r, &mut ic);
    let spec_count = ic.implicit_specs.len();
    drop(ic);
    assert!(
        h.diags
            .iter()
            .any(|d| d.class == DiagnosticClass::TypecheckError
                && d.message.contains("incompatible types")),
        "got: {:?}",
        h.diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
    assert_eq!(spec_count, 0);
}

#[test]
fn range_non_numeric_endpoint_diagnoses() {
    // `b..<c` where b, c: bool — bool isn't a valid range element.
    let mut h = crate::test_support::Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let mut env = TyEnv::new();
    let bool_ty = h.ty_interner.prim(Primitive::Bool);
    let b_sym = h.interner.intern("b");
    let c_sym = h.interner.intern("c");
    env.bind(b_sym, bool_ty);
    env.bind(c_sym, bool_ty);

    let mut r = range_expr(
        path_one(&h.interner, "b", bool_ty),
        path_one(&h.interner, "c", bool_ty),
        h.ty_interner.error(),
    );
    let _ = synth_expr(&mut env, &mut r, &mut ic);

    assert!(
        h.diags
            .iter()
            .any(|d| d.message.contains("range endpoints must be numeric")),
        "got: {:?}",
        h.diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

#[test]
fn duplicate_ranges_dedup_to_single_spec_request() {
    // Two range expressions over the same element type → exactly
    // one entry in implicit_specs.
    let mut h = crate::test_support::Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let mut env = TyEnv::new();
    let i64 = h.ty_interner.prim(Primitive::I64);

    let mut r1 = range_expr(int_lit(i64, 0), int_lit(i64, 10), h.ty_interner.error());
    let mut r2 = range_expr(int_lit(i64, 0), int_lit(i64, 20), h.ty_interner.error());
    let _ = synth_expr(&mut env, &mut r1, &mut ic);
    let _ = synth_expr(&mut env, &mut r2, &mut ic);

    assert_eq!(ic.implicit_specs.len(), 1);
    assert_eq!(ic.implicit_specs[0].type_arg, i64);
}

#[test]
fn distinct_element_types_record_two_requests() {
    let mut h = crate::test_support::Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let mut env = TyEnv::new();
    let i64 = h.ty_interner.prim(Primitive::I64);
    let usize_ty = h.ty_interner.prim(Primitive::Usize);
    let n_sym = h.interner.intern("n");
    env.bind(n_sym, usize_ty);

    let mut r1 = range_expr(int_lit(i64, 0), int_lit(i64, 10), h.ty_interner.error());
    let mut r2 = range_expr(
        int_lit(usize_ty, 0),
        path_one(&h.interner, "n", usize_ty),
        h.ty_interner.error(),
    );
    let _ = synth_expr(&mut env, &mut r1, &mut ic);
    let _ = synth_expr(&mut env, &mut r2, &mut ic);

    assert_eq!(ic.implicit_specs.len(), 2);
    let types: Vec<TyId> = ic.implicit_specs.iter().map(|r| r.type_arg).collect();
    assert!(types.contains(&i64));
    assert!(types.contains(&usize_ty));
}

#[test]
fn implicit_spec_qualified_name_matches_locked_string() {
    assert_eq!(ImplicitSpec::Range.qualified_name(), "std.core.range.Range");
    assert_eq!(ImplicitSpec::Option.qualified_name(), "std.core.option.Option");
}
