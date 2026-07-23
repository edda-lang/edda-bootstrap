//! Tests for the call-site inference helpers.

use super::*;
use super::super::synth_expr;
use super::capability::CAPABILITY_DERIVATION_DEPTH;
use ahash::AHashMap;
use edda_intern::Symbol;
use crate::cx::TyCx;
use crate::effect::{EffectEntry, PureEffect};
use crate::hir::{HirCallArg, HirExpr, HirExprKind, HirPath};
use crate::sig::Param;
use crate::lower::LowerCx;
use crate::lower::lower_expr;
use crate::prim::Primitive;
use crate::sig::ParamMode;
use crate::test_support::{Harness, ident_for, path_for};
use crate::ty::TyInterner;
use edda_syntax::ast::{CallArg, Expr, ExprKind, Literal};

fn lit_int(value: u128) -> Expr {
    Expr {
        span: Span::DUMMY,
        kind: ExprKind::Literal(Literal::Int {
            value,
            base: edda_syntax::IntBase::Dec,
        }),
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
    synth_expr(
        &mut env,
        &mut hir,
        &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
    )
}

#[test]
fn call_without_package_emits_diagnostic() {
    let mut h = Harness::new();
    let call = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Call {
            callee: Box::new(Expr {
                span: Span::DUMMY,
                kind: ExprKind::Path(path_for(&h.interner, &["f"])),
            }),
            args: vec![CallArg::bare(lit_int(1))],
        },
    };
    let ty = lower_and_synth(&mut h, &call);
    assert_eq!(ty, h.ty_interner.error());
    assert!(
        h.diags
            .iter()
            .any(|d| d.message.contains("resolved-package")),
        "expected resolved-package diagnostic, found {:?}",
        h.diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

#[test]
fn call_with_literal_callee_diagnoses_non_callable_type() {
    let mut h = Harness::new();
    // `42(1)` — callee synthesises to `i64`, which is not a
    // fn-pointer. The indirect-call gate fires.
    let call = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Call {
            callee: Box::new(lit_int(42)),
            args: vec![CallArg::bare(lit_int(1))],
        },
    };
    let ty = lower_and_synth(&mut h, &call);
    assert_eq!(ty, h.ty_interner.error());
    assert!(
        h.diags
            .iter()
            .any(|d| d.message.contains("cannot call value of type")),
    );
}

#[test]
fn call_with_zero_args_without_package_diagnoses() {
    let mut h = Harness::new();
    let call = Expr {
        span: Span::DUMMY,
        kind: ExprKind::Call {
            callee: Box::new(Expr {
                span: Span::DUMMY,
                kind: ExprKind::Path(path_for(&h.interner, &["f"])),
            }),
            args: vec![],
        },
    };
    let ty = lower_and_synth(&mut h, &call);
    assert_eq!(ty, h.ty_interner.error());
    // The no-package branch emits exactly one diagnostic.
    assert_eq!(h.diags.error_count(), 1);
}

// -- Capability substitution tests -------------------

fn hir_path(interner: &edda_intern::Interner, name: &str) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: TyInterner::new().error(),
        kind: HirExprKind::Path(HirPath {
            span: Span::DUMMY,
            segments: Box::from([ident_for(interner, name)]),
        }),
    }
}

fn hir_field(receiver: HirExpr, interner: &edda_intern::Interner, name: &str) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: TyInterner::new().error(),
        kind: HirExprKind::Field {
            receiver: Box::new(receiver),
            name: ident_for(interner, name),
        },
    }
}

fn hir_call_arg(expr: HirExpr) -> HirCallArg {
    HirCallArg {
        span: Span::DUMMY,
        mode: None,
        name: None,
        expr,
    }
}

#[test]
fn capability_source_finds_single_segment_path() {
    let interner = edda_intern::Interner::new();
    let world = hir_path(&interner, "world");
    let sym = interner.intern("world");
    assert_eq!(capability_source(&world), Some(sym));
}

#[test]
fn capability_source_follows_field_chain_to_root() {
    let interner = edda_intern::Interner::new();
    // `world.network.local_addr` — capability source is `world`.
    let world = hir_path(&interner, "world");
    let world_sym = interner.intern("world");
    let net = hir_field(world, &interner, "network");
    let leaf = hir_field(net, &interner, "local_addr");
    assert_eq!(capability_source(&leaf), Some(world_sym));
}

#[test]
fn capability_source_returns_none_for_multi_segment_path() {
    let interner = edda_intern::Interner::new();
    // `std.io.stdin` — multi-segment paths are module references,
    // not capability sources.
    let expr = HirExpr {
        span: Span::DUMMY,
        ty: TyInterner::new().error(),
        kind: HirExprKind::Path(HirPath {
            span: Span::DUMMY,
            segments: Box::from([
                ident_for(&interner, "std"),
                ident_for(&interner, "io"),
                ident_for(&interner, "stdin"),
            ]),
        }),
    };
    assert_eq!(capability_source(&expr), None);
}

#[test]
fn capability_source_returns_none_for_computed_argument() {
    let ty = TyInterner::new();
    let lit = HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::I64),
        kind: HirExprKind::Literal(Literal::Int {
            value: 0,
            base: edda_syntax::IntBase::Dec,
        }),
    };
    assert_eq!(capability_source(&lit), None);
}

#[test]
fn translate_callee_capability_substitutes_to_caller_param() {
    let interner = edda_intern::Interner::new();
    let ty = TyInterner::new();
    // Callee: `function open(fs: Filesystem) with {fs}`. Caller
    // passes its own `fs` parameter — substitution yields
    // `Capability(fs)` from the caller's binding (same Symbol).
    let fs_sym = interner.intern("fs");
    let params = vec![Param {
        span: Span::DUMMY,
        name: fs_sym,
        mode: ParamMode::Default,
        ty: ty.prim(Primitive::I32),
    }];
    let args = vec![hir_call_arg(hir_path(&interner, "fs"))];

    let translated = translate_entry(
        EffectEntry::Capability(fs_sym),
        &params,
        None,
        &args,
        &AHashMap::default(),
    );
    assert_eq!(translated, EffectEntry::Capability(fs_sym));
}

#[test]
fn translate_callee_capability_substitutes_via_field_derivation() {
    let interner = edda_intern::Interner::new();
    let ty = TyInterner::new();
    // Callee: `function read(fs: Filesystem) with {fs}`. Caller
    // passes `world.fs` — substitution yields `Capability(world)`
    // because the row names parameters held, not derived
    // bindings (`effect-tracking.md §2`).
    let fs_sym = interner.intern("fs");
    let world_sym = interner.intern("world");
    let params = vec![Param {
        span: Span::DUMMY,
        name: fs_sym,
        mode: ParamMode::Default,
        ty: ty.prim(Primitive::I32),
    }];
    let world_expr = hir_path(&interner, "world");
    let world_fs = hir_field(world_expr, &interner, "fs");
    let args = vec![hir_call_arg(world_fs)];

    let translated = translate_entry(
        EffectEntry::Capability(fs_sym),
        &params,
        None,
        &args,
        &AHashMap::default(),
    );
    assert_eq!(translated, EffectEntry::Capability(world_sym));
}

#[test]
fn translate_callee_pure_entries_pass_through_unchanged() {
    // `Pure(_)` entries are matched by qualified name at the
    // function-exit row check; no caller-side rewrite needed.
    let interner = edda_intern::Interner::new();
    let ty = TyInterner::new();
    let i64 = ty.prim(Primitive::I64);
    let fs_sym = interner.intern("fs");
    let params = vec![Param {
        span: Span::DUMMY,
        name: fs_sym,
        mode: ParamMode::Default,
        ty: ty.prim(Primitive::I32),
    }];
    let args = vec![hir_call_arg(hir_path(&interner, "fs"))];

    let empty: AHashMap<Symbol, Symbol> = AHashMap::default();
    let pure_err = EffectEntry::Pure(PureEffect::Err(i64));
    assert_eq!(translate_entry(pure_err, &params, None, &args, &empty), pure_err);

    let pure_panic = EffectEntry::Pure(PureEffect::Panic);
    assert_eq!(
        translate_entry(pure_panic, &params, None, &args, &empty),
        pure_panic,
    );
}

#[test]
fn translate_callee_falls_back_when_source_untraceable() {
    // If the argument is a computed expression (no static path
    // root), translation falls back to the callee-side entry so
    // the function-exit check surfaces the issue.
    let interner = edda_intern::Interner::new();
    let ty = TyInterner::new();
    let fs_sym = interner.intern("fs");
    let params = vec![Param {
        span: Span::DUMMY,
        name: fs_sym,
        mode: ParamMode::Default,
        ty: ty.prim(Primitive::I32),
    }];
    let lit_arg = HirExpr {
        span: Span::DUMMY,
        ty: ty.prim(Primitive::I64),
        kind: HirExprKind::Literal(Literal::Int {
            value: 0,
            base: edda_syntax::IntBase::Dec,
        }),
    };
    let args = vec![hir_call_arg(lit_arg)];

    let translated = translate_entry(
        EffectEntry::Capability(fs_sym),
        &params,
        None,
        &args,
        &AHashMap::default(),
    );
    assert_eq!(translated, EffectEntry::Capability(fs_sym));
}

#[test]
fn translate_callee_unknown_parameter_passes_through() {
    // Callee row mentions a capability whose symbol doesn't match
    // any positional parameter — malformed callee row. Pass
    // through; the function-exit check will surface it.
    let interner = edda_intern::Interner::new();
    let ty = TyInterner::new();
    let fs_sym = interner.intern("fs");
    let bogus_sym = interner.intern("notaparam");
    let params = vec![Param {
        span: Span::DUMMY,
        name: fs_sym,
        mode: ParamMode::Default,
        ty: ty.prim(Primitive::I32),
    }];
    let args = vec![hir_call_arg(hir_path(&interner, "fs"))];

    let entry = EffectEntry::Capability(bogus_sym);
    assert_eq!(translate_entry(entry, &params, None, &args, &AHashMap::default()), entry);
}

#[test]
fn capability_source_bounded_recursion_terminates() {
    // Build a deeply-nested Field chain over a single root and
    // confirm `capability_source` resolves to the root within
    // `CAPABILITY_DERIVATION_DEPTH` levels. Beyond the bound,
    // `None` is returned (defence-in-depth against malformed
    // HIR).
    let interner = edda_intern::Interner::new();
    let root_sym = interner.intern("root");
    let mut expr = hir_path(&interner, "root");
    for i in 0..(CAPABILITY_DERIVATION_DEPTH - 1) {
        let name = format!("f{i}");
        expr = hir_field(expr, &interner, &name);
    }
    assert_eq!(capability_source(&expr), Some(root_sym));

    // One more level pushes us past the bound.
    expr = hir_field(expr, &interner, "fbeyond");
    assert_eq!(capability_source(&expr), None);
}
