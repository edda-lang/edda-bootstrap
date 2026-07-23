//! Tests for the "no `mutable` crosses the spawn boundary" rule
//! and the transparent `Task(T)`
//! result typing of `.spawn` / `.await`.
//!
//! These construct minimal HIR by hand — no `ResolvedPackage` is
//! wired up, so the spawn body's call must reach [`crate::infer::call`]'s
//! indirect (fn-pointer) dispatch rather than the direct named-function
//! path: the callee is wrapped in a bare block so `synth_call` sees a
//! non-`Path` callee and routes straight to `synth_indirect_call`,
//! mirroring the pattern `field_tracker_tests.rs` uses to exercise mode
//! transitions without a resolver.

use edda_diag::DiagnosticClass;
use edda_span::Span;
use edda_syntax::ast::Ident;

use super::synth_expr;
use crate::cx::TyCx;
use crate::effect::EffectRow;
use crate::hir::{
    HirBlock, HirCallArg, HirCallMode, HirExpr, HirExprKind, HirPath, HirSpawn, HirSpawnArg,
};
use crate::infer::{InferCx, TyEnv};
use crate::lower::LowerCx;
use crate::prim::Primitive;
use crate::sig::{FnPtrParam, FnPtrSig, ParamMode};
use crate::test_support::Harness;
use crate::ty::TyId;

fn ident(h: &Harness, name: &str) -> Ident {
    Ident {
        name: h.interner.intern(name),
        span: Span::DUMMY,
    }
}

fn path_one(name: Ident, ty: TyId) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty,
        kind: HirExprKind::Path(HirPath {
            span: Span::DUMMY,
            segments: Box::from([name]),
        }),
    }
}

/// Wraps `inner` in a bare `{ inner }` block so its parent expression is
/// not a `Path` — routes `synth_call`'s callee dispatch straight to the
/// indirect (fn-pointer) call path, which needs no resolved package.
fn block_wrapped(inner: HirExpr, ty: TyId) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty,
        kind: HirExprKind::Block(HirBlock {
            span: Span::DUMMY,
            ty,
            stmts: Box::from([]),
            trailing: Some(Box::new(inner)),
        }),
    }
}

fn mode_violation_count(h: &Harness) -> usize {
    h.diags
        .iter()
        .filter(|d| d.class == DiagnosticClass::ModeViolation)
        .count()
}

#[test]
fn spawn_body_rejects_mutable_call_on_outer_var() {
    // `var x: i32 = 5; scope(exec) group { group.spawn { helper(mutable x) } }`
    // — `x` is an outer binding, mutable at the parent scope, but the
    // spawn body must not be able to borrow it `mutable`.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let i32_ty = h.ty_interner.prim(Primitive::I32);
    let unit_ty = h.ty_interner.prim(Primitive::Unit);

    let x_sym = h.interner.intern("x");
    let helper_sym = h.interner.intern("helper");
    let helper_ty = h.ty_interner.fn_ptr(FnPtrSig {
        params: Box::from([FnPtrParam {
            mode: ParamMode::Mutable,
            ty: i32_ty,
        }]),
        return_ty: unit_ty,
        effects: EffectRow::empty(),
    });

    let mut env = TyEnv::new();
    env.bind(x_sym, i32_ty); // `var x` — mutable at bind time
    env.bind(helper_sym, helper_ty);

    let call = HirExpr {
        span: Span::DUMMY,
        ty: unit_ty,
        kind: HirExprKind::Call {
            callee: Box::new(block_wrapped(
                path_one(ident(&h, "helper"), helper_ty),
                helper_ty,
            )),
            args: Box::from([HirCallArg {
                span: Span::DUMMY,
                mode: Some(HirCallMode::Mutable),
                name: None,
                expr: path_one(ident(&h, "x"), i32_ty),
            }]),
        },
    };
    let spawn = HirSpawn {
        span: Span::DUMMY,
        scope_name: ident(&h, "group"),
        args: Box::from([]),
        body: HirBlock {
            span: Span::DUMMY,
            ty: unit_ty,
            stmts: Box::from([]),
            trailing: Some(Box::new(call)),
        },
    };
    let mut expr = HirExpr {
        span: Span::DUMMY,
        ty: unit_ty,
        kind: HirExprKind::Spawn(Box::new(spawn)),
    };

    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    synth_expr(&mut env, &mut expr, &mut ic);
    drop(ic);

    assert_eq!(
        mode_violation_count(&h),
        1,
        "spawn body must reject a `mutable` call-arg on an outer var, got: {:?}",
        h.diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
    // The restriction lifts once the spawn body exits.
    assert_eq!(env.lookup_mutable(x_sym), Some(true));
}

#[test]
fn spawn_body_admits_mutable_call_on_body_local_take_arg() {
    // `group.spawn(take owned = clone(shared)) { mutate(mutable owned) }`
    // — `owned` is a body-local binding introduced by the spawn's
    // explicit take-arg list, not an outer binding, so it must keep its
    // normal mutability inside the body.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let i32_ty = h.ty_interner.prim(Primitive::I32);
    let unit_ty = h.ty_interner.prim(Primitive::Unit);

    let shared_sym = h.interner.intern("shared");
    let mutate_sym = h.interner.intern("mutate");
    let mutate_ty = h.ty_interner.fn_ptr(FnPtrSig {
        params: Box::from([FnPtrParam {
            mode: ParamMode::Mutable,
            ty: i32_ty,
        }]),
        return_ty: unit_ty,
        effects: EffectRow::empty(),
    });

    let mut env = TyEnv::new();
    env.bind(shared_sym, i32_ty);
    env.bind(mutate_sym, mutate_ty);

    let call = HirExpr {
        span: Span::DUMMY,
        ty: unit_ty,
        kind: HirExprKind::Call {
            callee: Box::new(block_wrapped(
                path_one(ident(&h, "mutate"), mutate_ty),
                mutate_ty,
            )),
            args: Box::from([HirCallArg {
                span: Span::DUMMY,
                mode: Some(HirCallMode::Mutable),
                name: None,
                expr: path_one(ident(&h, "owned"), i32_ty),
            }]),
        },
    };
    let spawn = HirSpawn {
        span: Span::DUMMY,
        scope_name: ident(&h, "group"),
        args: Box::from([HirSpawnArg {
            span: Span::DUMMY,
            name: ident(&h, "owned"),
            ty: Some(i32_ty),
            init: path_one(ident(&h, "shared"), i32_ty),
        }]),
        body: HirBlock {
            span: Span::DUMMY,
            ty: unit_ty,
            stmts: Box::from([]),
            trailing: Some(Box::new(call)),
        },
    };
    let mut expr = HirExpr {
        span: Span::DUMMY,
        ty: unit_ty,
        kind: HirExprKind::Spawn(Box::new(spawn)),
    };

    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    synth_expr(&mut env, &mut expr, &mut ic);
    drop(ic);

    assert_eq!(
        mode_violation_count(&h),
        0,
        "a body-local take-arg binding must stay mutable inside its own spawn body, got: {:?}",
        h.diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

#[test]
fn spawn_synthesises_body_result_type() {
    // `group.spawn { y }` where `y: i32` — the spawn expression's own
    // value type is the body's result type `T` (transparent typing).
    // No `Task(T)` implicit-spec
    // request is registered — `.spawn` never binds a `Task_<T>`
    // nominal, so nothing would consume a materialised one.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let i32_ty = h.ty_interner.prim(Primitive::I32);
    let error_ty = h.ty_interner.error();

    let y_sym = h.interner.intern("y");
    let mut env = TyEnv::new();
    env.bind(y_sym, i32_ty);

    let spawn = HirSpawn {
        span: Span::DUMMY,
        scope_name: ident(&h, "group"),
        args: Box::from([]),
        body: HirBlock {
            span: Span::DUMMY,
            ty: error_ty,
            stmts: Box::from([]),
            trailing: Some(Box::new(path_one(ident(&h, "y"), error_ty))),
        },
    };
    let mut expr = HirExpr {
        span: Span::DUMMY,
        ty: error_ty,
        kind: HirExprKind::Spawn(Box::new(spawn)),
    };

    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let out = synth_expr(&mut env, &mut expr, &mut ic);

    assert_eq!(out, i32_ty, "spawn must synthesise the body's result type");
    assert_eq!(expr.ty, i32_ty);
    assert!(
        ic.implicit_specs.is_empty(),
        "spawn must not register a Task implicit-spec request, got: {:?}",
        ic.implicit_specs,
    );
}

#[test]
fn await_synthesises_operand_type() {
    // `t.await` where `t: i32` (the transparent task value) — the join
    // yields the task's semantic result type, i.e. the operand's type
    // passes through. This is what MIR's
    // `Await::dest` lowers from.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let i32_ty = h.ty_interner.prim(Primitive::I32);
    let error_ty = h.ty_interner.error();

    let t_sym = h.interner.intern("t");
    let mut env = TyEnv::new();
    env.bind(t_sym, i32_ty);

    let mut expr = HirExpr {
        span: Span::DUMMY,
        ty: error_ty,
        kind: HirExprKind::Await(Box::new(path_one(ident(&h, "t"), error_ty))),
    };

    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let out = synth_expr(&mut env, &mut expr, &mut ic);

    assert_eq!(out, i32_ty, "await must synthesise its operand's type");
    assert_eq!(expr.ty, i32_ty);
}
