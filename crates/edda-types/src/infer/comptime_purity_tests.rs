//! Tests for the comptime-purity check
//! (`inference-rules.md §1a.6`).
//!
//! These tests verify the row-side approximation of P-CompTimePure
//! at every call site inside a `comptime <expr>` or `comptime { … }`
//! body — the callee's declared row must be a subset of
//! `{panic, yield: T}`. The check fires only when
//! [`InferCx::in_comptime_context`] is `true`; non-comptime call
//! sites are unaffected.

use edda_diag::DiagnosticClass;
use edda_intern::Interner;
use edda_span::Span;

use crate::cx::TyCx;
use crate::effect::{EffectEntry, EffectRow, PureEffect};
use crate::hir::{HirExpr, HirExprKind};
use crate::infer::{InferCx, TyEnv};
use crate::lower::LowerCx;
use crate::prim::Primitive;

#[test]
fn comptime_purity_is_clean_outside_comptime_context() {
    // Even when a callee has a non-pure row, the check only fires
    // inside a comptime body. This test verifies the gate is off by
    // default. The full flow requires a `ResolvedPackage` to look up
    // the callee, which is beyond a unit-test fixture — instead we
    // verify by direct flag introspection: `in_comptime_context`
    // defaults to `false`.
    let mut h = crate::test_support::Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    assert!(!ic.in_comptime_context);
}

#[test]
fn comptime_arm_toggles_flag() {
    // `comptime <expr>` toggles `in_comptime_context = true` while
    // walking the inner expression, then restores it to the previous
    // value. The inner expression here is a pure literal — no
    // call-site purity check fires, but the flag toggle is observable
    // by hooking into a synthetic visitor. We use a `Comptime(Error)`
    // form to confirm the dispatch wired through.
    let mut h = crate::test_support::Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let mut env = TyEnv::new();
    let mut expr = HirExpr {
        span: Span::DUMMY,
        ty: h.ty_interner.error(),
        kind: HirExprKind::Comptime(Box::new(HirExpr {
            span: Span::DUMMY,
            ty: h.ty_interner.error(),
            kind: HirExprKind::Error,
        })),
    };
    let _ = super::synth_expr(&mut env, &mut expr, &mut ic);
    // After returning, the flag should be back to false.
    assert!(!ic.in_comptime_context);
}

#[test]
fn comptime_block_toggles_flag() {
    let mut h = crate::test_support::Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let mut env = TyEnv::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let mut expr = HirExpr {
        span: Span::DUMMY,
        ty: unit,
        kind: HirExprKind::ComptimeBlock(crate::hir::HirBlock {
            span: Span::DUMMY,
            ty: unit,
            stmts: Box::from([]),
            trailing: None,
        }),
    };
    let _ = super::synth_expr(&mut env, &mut expr, &mut ic);
    assert!(!ic.in_comptime_context);
}

#[test]
fn pure_row_is_admissible_in_comptime() {
    // Callee row = {panic} — admissible. No diagnostic.
    let mut h = crate::test_support::Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    ic.in_comptime_context = true;
    let pure_row =
        EffectRow::from_entries([EffectEntry::Pure(PureEffect::Panic)]);
    let callee_sym = h.interner.intern("safe_helper");

    super::call::check_comptime_purity(
        &pure_row,
        callee_sym,
        Span::DUMMY,
        &mut ic,
    );
    assert!(
        h.diags
            .iter()
            .all(|d| d.class != DiagnosticClass::ComptimePurityLoss),
        "got: {:?}",
        h.diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

#[test]
fn yield_row_is_admissible_in_comptime() {
    // Callee row = {yield: u8} — admissible (yield is a pure-effect
    // kind compatible with comptime per §1a.6).
    let mut h = crate::test_support::Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    ic.in_comptime_context = true;
    let u8_ty = h.ty_interner.prim(Primitive::U8);
    let pure_row = EffectRow::from_entries([
        EffectEntry::Pure(PureEffect::Yield(u8_ty)),
        EffectEntry::Pure(PureEffect::Panic),
    ]);
    let callee_sym = h.interner.intern("yielder");

    super::call::check_comptime_purity(
        &pure_row,
        callee_sym,
        Span::DUMMY,
        &mut ic,
    );
    assert!(
        h.diags
            .iter()
            .all(|d| d.class != DiagnosticClass::ComptimePurityLoss),
    );
}

#[test]
fn err_row_is_not_pure_in_comptime() {
    let mut h = crate::test_support::Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    ic.in_comptime_context = true;
    let i64 = h.ty_interner.prim(Primitive::I64);
    let row =
        EffectRow::from_entries([EffectEntry::Pure(PureEffect::Err(i64))]);
    let callee_sym = h.interner.intern("fail_helper");

    super::call::check_comptime_purity(
        &row,
        callee_sym,
        Span::DUMMY,
        &mut ic,
    );
    let diag = h
        .diags
        .iter()
        .find(|d| d.class == DiagnosticClass::ComptimePurityLoss);
    assert!(diag.is_some(), "expected ComptimePurityLoss diag");
    let msg = &diag.unwrap().message;
    assert!(msg.contains("fail_helper"), "msg: {msg}");
    assert!(msg.contains("err: i64"), "msg: {msg}");
}

#[test]
fn cancellation_row_is_not_pure_in_comptime() {
    let mut h = crate::test_support::Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    ic.in_comptime_context = true;
    let row = EffectRow::from_entries([EffectEntry::Pure(PureEffect::Cancellation)]);
    let callee_sym = h.interner.intern("await_helper");

    super::call::check_comptime_purity(
        &row,
        callee_sym,
        Span::DUMMY,
        &mut ic,
    );
    let diag = h
        .diags
        .iter()
        .find(|d| d.class == DiagnosticClass::ComptimePurityLoss);
    assert!(diag.is_some(), "expected ComptimePurityLoss diag");
    let msg = &diag.unwrap().message;
    assert!(msg.contains("await_helper"), "msg: {msg}");
    assert!(msg.contains("cancellation"), "msg: {msg}");
}

#[test]
fn capability_row_is_not_pure_in_comptime() {
    let mut h = crate::test_support::Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    ic.in_comptime_context = true;
    let fs_sym = h.interner.intern("fs");
    let row = EffectRow::from_entries([EffectEntry::Capability(fs_sym)]);
    let callee_sym = h.interner.intern("read_file");

    super::call::check_comptime_purity(
        &row,
        callee_sym,
        Span::DUMMY,
        &mut ic,
    );
    let diag = h
        .diags
        .iter()
        .find(|d| d.class == DiagnosticClass::ComptimePurityLoss);
    assert!(diag.is_some());
    let msg = &diag.unwrap().message;
    assert!(msg.contains("read_file"), "msg: {msg}");
    assert!(msg.contains("fs"), "msg: {msg}");
}

#[test]
fn multiple_offending_entries_render_in_diagnostic() {
    let mut h = crate::test_support::Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    ic.in_comptime_context = true;
    let i64 = h.ty_interner.prim(Primitive::I64);
    let fs_sym = h.interner.intern("fs");
    let row = EffectRow::from_entries([
        EffectEntry::Capability(fs_sym),
        EffectEntry::Pure(PureEffect::Err(i64)),
        EffectEntry::Pure(PureEffect::Panic), // admissible — should not appear
    ]);
    let callee_sym = h.interner.intern("multi");

    super::call::check_comptime_purity(
        &row,
        callee_sym,
        Span::DUMMY,
        &mut ic,
    );
    let msg = &h
        .diags
        .iter()
        .find(|d| d.class == DiagnosticClass::ComptimePurityLoss)
        .unwrap()
        .message;
    assert!(msg.contains("fs"));
    assert!(msg.contains("err: i64"));
    assert!(!msg.contains("panic"), "panic should not appear: {msg}");
}

#[test]
fn flag_save_restore_handles_nested_comptime() {
    // `comptime (comptime <e>)` — toggling twice and restoring twice
    // must leave the flag false after both exits.
    let mut h = crate::test_support::Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let mut env = TyEnv::new();
    let mut expr = HirExpr {
        span: Span::DUMMY,
        ty: h.ty_interner.error(),
        kind: HirExprKind::Comptime(Box::new(HirExpr {
            span: Span::DUMMY,
            ty: h.ty_interner.error(),
            kind: HirExprKind::Comptime(Box::new(HirExpr {
                span: Span::DUMMY,
                ty: h.ty_interner.error(),
                kind: HirExprKind::Error,
            })),
        })),
    };
    let _ = super::synth_expr(&mut env, &mut expr, &mut ic);
    assert!(!ic.in_comptime_context);
}

