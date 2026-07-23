//! Tests for the §5 per-field tracker — field-projection
//! call-arg transitions (`f(take|set|mutable x.field)`), the
//! M-Field-Assign rule (`x.field = e`), the per-field read gate on
//! `synth_field`, and the function-exit per-field check.
//!
//! These tests construct minimal HIR by hand and pre-seed a `TyCx`
//! with a single nominal product type, then run individual
//! statements / expressions through the inference pass.

use edda_diag::{DiagnosticClass, Diagnostics, LintConfig};
use edda_intern::{Interner, Symbol};
use edda_resolve::{BindingId, ModuleId};
use edda_span::Span;
use edda_syntax::IntBase;
use edda_syntax::ast::{Ident, Literal};

use super::fn_body::check_fn_body;
use super::synth_expr;
use crate::cx::{FieldInfo, TyCx, TypeDeclInfo, TypeDeclShape};
use crate::effect::EffectRow;
use crate::hir::{
    HirBlock, HirCallArg, HirCallMode, HirExpr, HirExprKind, HirPath, HirStmt, HirStmtKind,
};
use crate::infer::{BindingState, InferCx, TyEnv};
use crate::lower::LowerCx;
use crate::prim::Primitive;
use crate::ty::{TyId, TyInterner};

/// Test fixture — bundles the immutable / mutable state every test
/// needs. Pre-allocates a Point product type with fields `x` and `y`
/// (both `f64`), keyed by `BindingId::new_for_tests(1)`.
struct Fixture {
    interner: Interner,
    ty_interner: TyInterner,
    ty_cx: TyCx,
    diags: Diagnostics,
    lint_cfg: LintConfig,
    point_id: BindingId,
    point_ty: TyId,
    field_x: Symbol,
    field_y: Symbol,
}

impl Fixture {
    fn new() -> Self {
        let interner = Interner::new();
        let ty_interner = TyInterner::new();
        let field_x = interner.intern("x");
        let field_y = interner.intern("y");
        let f64_ty = ty_interner.prim(Primitive::F64);
        let point_id = BindingId::new(ModuleId::new(0), 1);
        let point_ty = ty_interner.nominal(point_id);
        let mut ty_cx = TyCx::new();
        ty_cx.insert_type_decl(
            point_id,
            TypeDeclInfo {
                span: Span::DUMMY,
                linearity: None,
                kind: TypeDeclShape::Product {
                    fields: Box::from([
                        FieldInfo {
                            span: Span::DUMMY,
                            name: field_x,
                            ty: f64_ty,
                            refinement: None,
                        },
                        FieldInfo {
                            span: Span::DUMMY,
                            name: field_y,
                            ty: f64_ty,
                            refinement: None,
                        },
                    ]),
                },
            },
        );
        Self {
            interner,
            ty_interner,
            ty_cx,
            diags: Diagnostics::new(),
            lint_cfg: LintConfig::new(),
            point_id,
            point_ty,
            field_x,
            field_y,
        }
    }

    fn lower_cx(&self) -> LowerCx<'_> {
        LowerCx {
            interner: &self.interner,
            ty_interner: &self.ty_interner,
            package: None,
        }
    }

    fn typecheck_count(&self) -> usize {
        self.diags
            .iter()
            .filter(|d| d.class == DiagnosticClass::TypecheckError)
            .count()
    }
}

fn ident(interner: &Interner, name: &str) -> Ident {
    Ident {
        name: interner.intern(name),
        span: Span::DUMMY,
    }
}

fn ident_from(sym: Symbol) -> Ident {
    Ident { name: sym, span: Span::DUMMY }
}

fn hir_path_one(name: Ident, ty: TyId) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty,
        kind: HirExprKind::Path(HirPath {
            span: Span::DUMMY,
            segments: Box::from([name]),
        }),
    }
}

fn hir_field(receiver: HirExpr, field: Ident, ty: TyId) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty,
        kind: HirExprKind::Field {
            receiver: Box::new(receiver),
            name: field,
        },
    }
}

fn hir_lit_float(ty: TyId) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty,
        kind: HirExprKind::Literal(Literal::Int {
            value: 0,
            base: IntBase::Dec,
        }),
    }
}

#[test]
fn synth_field_on_valid_binding_reads_cleanly() {
    let mut fx = Fixture::new();
    let p_sym = fx.interner.intern("p");
    let f64 = fx.ty_interner.prim(Primitive::F64);
    let cx = LowerCx { interner: &fx.interner, ty_interner: &fx.ty_interner, package: None };
    let mut ic = InferCx::new(&cx, &fx.ty_cx, &fx.lint_cfg, &mut fx.diags);
    let mut env = TyEnv::new();
    env.bind(p_sym, fx.point_ty);

    let mut access = hir_field(
        hir_path_one(ident_from(p_sym), fx.point_ty),
        ident_from(fx.field_x),
        f64,
    );
    let ty = synth_expr(&mut env, &mut access, &mut ic);
    assert_eq!(ty, f64);
    assert_eq!(fx.typecheck_count(), 0);
}

#[test]
fn synth_field_on_uninit_field_diagnoses() {
    let mut fx = Fixture::new();
    let p_sym = fx.interner.intern("p");
    let f64 = fx.ty_interner.prim(Primitive::F64);
    let field_x = fx.field_x;
    let cx = LowerCx { interner: &fx.interner, ty_interner: &fx.ty_interner, package: None };
    let mut ic = InferCx::new(&cx, &fx.ty_cx, &fx.lint_cfg, &mut fx.diags);
    let mut env = TyEnv::new();
    // Bind p as Uninit — every field is Uninit.
    env.bind_with_state(p_sym, fx.point_ty, BindingState::Uninit);

    let mut access = hir_field(
        hir_path_one(ident_from(p_sym), fx.point_ty),
        ident_from(field_x),
        f64,
    );
    let ty = synth_expr(&mut env, &mut access, &mut ic);
    assert_eq!(ty, fx.ty_interner.error());
    assert!(
        fx.diags
            .iter()
            .any(|d| d.message.contains("p.x") && d.message.contains("uninitialised")),
        "got: {:?}",
        fx.diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

#[test]
fn field_assign_after_sink_promotes_to_partial_init() {
    let mut fx = Fixture::new();
    let p_sym = fx.interner.intern("p");
    let _ = fx.point_id; // accessed via TyCx
    let f64 = fx.ty_interner.prim(Primitive::F64);
    let field_x = fx.field_x;
    let field_y = fx.field_y;
    let cx = LowerCx { interner: &fx.interner, ty_interner: &fx.ty_interner, package: None };
    let mut ic = InferCx::new(&cx, &fx.ty_cx, &fx.lint_cfg, &mut fx.diags);
    let mut env = TyEnv::new();
    env.bind(p_sym, fx.point_ty); // p is Valid

    // Statement: `consume(take p.x)` — take the `x` field.
    // We simulate this by calling apply_call_mode_transition directly.
    let arg = HirCallArg {
        span: Span::DUMMY,
        mode: Some(HirCallMode::Take),
        name: None,
        expr: hir_field(
            hir_path_one(ident_from(p_sym), fx.point_ty),
            ident_from(field_x),
            f64,
        ),
    };
    super::mode::apply_call_mode_transition(&mut env, &arg, &mut ic);
    // After take p.x: p should be PartialInit({y}) — only y is valid.
    let post = env.lookup_state(p_sym).unwrap();
    match &post {
        BindingState::PartialInit(f) => {
            assert!(f.contains(&field_y));
            assert!(!f.contains(&field_x));
        }
        other => panic!("expected PartialInit, got {other:?}"),
    }
    assert_eq!(fx.typecheck_count(), 0);
}

// A `mutable` borrow of a field whose
// root binding is immutable (a `let` local / `Default`-mode param) is
// rejected — the backend would lower it as a byval copy and lose the
// write.
#[test]
fn mutable_field_of_immutable_binding_is_rejected() {
    let mut fx = Fixture::new();
    let p_sym = fx.interner.intern("p");
    let f64 = fx.ty_interner.prim(Primitive::F64);
    let field_x = fx.field_x;
    let cx = LowerCx { interner: &fx.interner, ty_interner: &fx.ty_interner, package: None };
    let mut ic = InferCx::new(&cx, &fx.ty_cx, &fx.lint_cfg, &mut fx.diags);
    let mut env = TyEnv::new();
    // `p` is an immutable (`let`) binding.
    env.bind_with_state_mut(p_sym, fx.point_ty, BindingState::Valid, false);
    let arg = HirCallArg {
        span: Span::DUMMY,
        mode: Some(HirCallMode::Mutable),
        name: None,
        expr: hir_field(
            hir_path_one(ident_from(p_sym), fx.point_ty),
            ident_from(field_x),
            f64,
        ),
    };
    super::mode::apply_call_mode_transition(&mut env, &arg, &mut ic);
    drop(ic);
    let mode_violations = fx
        .diags
        .iter()
        .filter(|d| d.class == DiagnosticClass::ModeViolation)
        .count();
    assert_eq!(mode_violations, 1, "expected one mode_violation");
}

// The companion: when the root binding is mutable (`var` / `mutable`
// param), the same borrow checks cleanly.
#[test]
fn mutable_field_of_mutable_binding_is_clean() {
    let mut fx = Fixture::new();
    let p_sym = fx.interner.intern("p");
    let f64 = fx.ty_interner.prim(Primitive::F64);
    let field_x = fx.field_x;
    let cx = LowerCx { interner: &fx.interner, ty_interner: &fx.ty_interner, package: None };
    let mut ic = InferCx::new(&cx, &fx.ty_cx, &fx.lint_cfg, &mut fx.diags);
    let mut env = TyEnv::new();
    // `p` is mutable — `bind` defaults to permissively mutable.
    env.bind(p_sym, fx.point_ty);
    let arg = HirCallArg {
        span: Span::DUMMY,
        mode: Some(HirCallMode::Mutable),
        name: None,
        expr: hir_field(
            hir_path_one(ident_from(p_sym), fx.point_ty),
            ident_from(field_x),
            f64,
        ),
    };
    super::mode::apply_call_mode_transition(&mut env, &arg, &mut ic);
    drop(ic);
    let mode_violations = fx
        .diags
        .iter()
        .filter(|d| d.class == DiagnosticClass::ModeViolation)
        .count();
    assert_eq!(mode_violations, 0, "mutable root must not trip the gate");
}

#[test]
fn field_assign_completing_set_promotes_to_valid() {
    let mut fx = Fixture::new();
    let p_sym = fx.interner.intern("p");
    let field_x = fx.field_x;
    let field_y = fx.field_y;
    let cx = LowerCx { interner: &fx.interner, ty_interner: &fx.ty_interner, package: None };
    let mut ic = InferCx::new(&cx, &fx.ty_cx, &fx.lint_cfg, &mut fx.diags);
    let mut env = TyEnv::new();
    // Start with PartialInit({y}) — y valid, x not.
    let mut valid_set = std::collections::BTreeSet::new();
    valid_set.insert(field_y);
    env.bind_with_state(p_sym, fx.point_ty, BindingState::PartialInit(valid_set));

    // Run M-Field-Assign for x: should restore x to Valid; F={x,y}=full → Valid.
    let restored = super::mode::apply_field_assign_transition(
        &mut env, p_sym, field_x, &mut ic,
    );
    assert!(restored, "apply_field_assign_transition should report success");
    let post = env.lookup_state(p_sym).unwrap();
    assert_eq!(post, BindingState::Valid);
}

#[test]
fn sink_on_already_consumed_field_diagnoses() {
    let mut fx = Fixture::new();
    let p_sym = fx.interner.intern("p");
    let f64 = fx.ty_interner.prim(Primitive::F64);
    let field_x = fx.field_x;
    let field_y = fx.field_y;
    let cx = LowerCx { interner: &fx.interner, ty_interner: &fx.ty_interner, package: None };
    let mut ic = InferCx::new(&cx, &fx.ty_cx, &fx.lint_cfg, &mut fx.diags);
    let mut env = TyEnv::new();
    // p starts at PartialInit({y}) — x has been consumed.
    let mut valid_set = std::collections::BTreeSet::new();
    valid_set.insert(field_y);
    env.bind_with_state(p_sym, fx.point_ty, BindingState::PartialInit(valid_set));

    // Attempting `take p.x` again — x is uninit, not valid.
    let arg = HirCallArg {
        span: Span::DUMMY,
        mode: Some(HirCallMode::Take),
        name: None,
        expr: hir_field(
            hir_path_one(ident_from(p_sym), fx.point_ty),
            ident_from(field_x),
            f64,
        ),
    };
    super::mode::apply_call_mode_transition(&mut env, &arg, &mut ic);
    assert!(
        fx.diags
            .iter()
            .any(|d| d.message.contains("p.x") && d.message.contains("uninitialised")),
        "got: {:?}",
        fx.diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

#[test]
fn set_on_uninit_field_promotes() {
    let mut fx = Fixture::new();
    let p_sym = fx.interner.intern("p");
    let f64 = fx.ty_interner.prim(Primitive::F64);
    let field_x = fx.field_x;
    let cx = LowerCx { interner: &fx.interner, ty_interner: &fx.ty_interner, package: None };
    let mut ic = InferCx::new(&cx, &fx.ty_cx, &fx.lint_cfg, &mut fx.diags);
    let mut env = TyEnv::new();
    // p starts at Uninit — no fields valid.
    env.bind_with_state(p_sym, fx.point_ty, BindingState::Uninit);

    // `init(init p.x)` — should transition x to Valid → PartialInit({x}).
    let arg = HirCallArg {
        span: Span::DUMMY,
        mode: Some(HirCallMode::Init),
        name: None,
        expr: hir_field(
            hir_path_one(ident_from(p_sym), fx.point_ty),
            ident_from(field_x),
            f64,
        ),
    };
    super::mode::apply_call_mode_transition(&mut env, &arg, &mut ic);
    let post = env.lookup_state(p_sym).unwrap();
    match &post {
        BindingState::PartialInit(f) => {
            assert!(f.contains(&field_x));
        }
        other => panic!("expected PartialInit, got {other:?}"),
    }
    assert_eq!(fx.typecheck_count(), 0);
}

#[test]
fn set_on_already_valid_field_diagnoses() {
    let mut fx = Fixture::new();
    let p_sym = fx.interner.intern("p");
    let f64 = fx.ty_interner.prim(Primitive::F64);
    let field_x = fx.field_x;
    let cx = LowerCx { interner: &fx.interner, ty_interner: &fx.ty_interner, package: None };
    let mut ic = InferCx::new(&cx, &fx.ty_cx, &fx.lint_cfg, &mut fx.diags);
    let mut env = TyEnv::new();
    env.bind(p_sym, fx.point_ty); // Valid → every field Valid

    let arg = HirCallArg {
        span: Span::DUMMY,
        mode: Some(HirCallMode::Init),
        name: None,
        expr: hir_field(
            hir_path_one(ident_from(p_sym), fx.point_ty),
            ident_from(field_x),
            f64,
        ),
    };
    super::mode::apply_call_mode_transition(&mut env, &arg, &mut ic);
    assert!(
        fx.diags
            .iter()
            .any(|d| d.message.contains("set p.x") && d.message.contains("uninitialised or consumed")),
        "got: {:?}",
        fx.diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

#[test]
fn function_exit_with_partial_init_diagnoses_missing_fields() {
    let mut fx = Fixture::new();
    let p_sym = fx.interner.intern("p");
    let unit = fx.ty_interner.prim(Primitive::Unit);
    let f64 = fx.ty_interner.prim(Primitive::F64);
    let field_x = fx.field_x;
    let field_y = fx.field_y;
    let cx = LowerCx { interner: &fx.interner, ty_interner: &fx.ty_interner, package: None };
    let mut ic = InferCx::new(&cx, &fx.ty_cx, &fx.lint_cfg, &mut fx.diags);
    let mut env = TyEnv::new();
    env.bind(p_sym, fx.point_ty);

    // Body: `consume(take p.x);` — sinks p.x, leaves p PartialInit({y}).
    let body = HirBlock {
        span: Span::DUMMY,
        ty: unit,
        stmts: Box::from([HirStmt {
            span: Span::DUMMY,
            kind: HirStmtKind::Expr(HirExpr {
                span: Span::DUMMY,
                ty: unit,
                // We use a "synthetic" call that bypasses the resolved-package
                // check path by being a no-op expression. To drive a real
                // take transition through the function body, simulate it
                // via the mode transition helper directly before the exit
                // check. The block body itself stays empty for the exit
                // check; the partial-init state is set up below.
                kind: HirExprKind::Error,
            }),
        }]),
        trailing: None,
    };
    // Pre-set the PartialInit state to mirror what `take p.x` would do.
    let mut valid_set = std::collections::BTreeSet::new();
    valid_set.insert(field_y);
    let _ = (field_x, f64); // unused outside of the type fixture
    env.transition(p_sym, BindingState::PartialInit(valid_set));

    let declared = EffectRow::empty();
    let mut body_mut = body;
    let return_ty = fx.ty_interner.error();
    let _ = check_fn_body(
        &mut env,
        &mut body_mut,
        &mut ic,
        &declared,
        return_ty,
        Span::DUMMY,
        false,
        &[],
    );

    let exit_diag = fx
        .diags
        .iter()
        .find(|d| d.message.contains("partially initialised at function exit"));
    assert!(exit_diag.is_some(), "expected partial-init exit diag, got: {:?}",
        fx.diags.iter().map(|d| &d.message).collect::<Vec<_>>());
    let msg = &exit_diag.unwrap().message;
    assert!(msg.contains("x"), "msg: {msg}");
}

#[test]
fn function_exit_admits_take_param_partial_when_remainder_is_droppable() {
    // §4 function-exit relaxation: a
    // `take`-mode parameter exiting at `PartialInit({y})` (i.e. `x`
    // consumed, `y` still valid) is admitted when every still-valid
    // field's type is trivially droppable. The Point fixture has both
    // fields as `f64`, which is droppable, so this admits without diag.
    let mut fx = Fixture::new();
    let p_sym = fx.interner.intern("p");
    let unit = fx.ty_interner.prim(Primitive::Unit);
    let field_y = fx.field_y;
    let cx = LowerCx { interner: &fx.interner, ty_interner: &fx.ty_interner, package: None };
    let mut ic = InferCx::new(&cx, &fx.ty_cx, &fx.lint_cfg, &mut fx.diags);
    let mut env = TyEnv::new();
    env.bind(p_sym, fx.point_ty);

    let body = HirBlock {
        span: Span::DUMMY,
        ty: unit,
        stmts: Box::from([HirStmt {
            span: Span::DUMMY,
            kind: HirStmtKind::Expr(HirExpr {
                span: Span::DUMMY,
                ty: unit,
                kind: HirExprKind::Error,
            }),
        }]),
        trailing: None,
    };
    // Set p to PartialInit({y}) — same exit state as if `take p.x` ran.
    let mut valid_set = std::collections::BTreeSet::new();
    valid_set.insert(field_y);
    env.transition(p_sym, BindingState::PartialInit(valid_set));

    let declared = EffectRow::empty();
    let mut body_mut = body;
    let return_ty = fx.ty_interner.error();
    // The crucial difference vs the prior test: pass p_sym as a take-param.
    let _ = check_fn_body(
        &mut env,
        &mut body_mut,
        &mut ic,
        &declared,
        return_ty,
        Span::DUMMY,
        false,
        &[p_sym],
    );

    let exit_diag = fx
        .diags
        .iter()
        .find(|d| d.message.contains("partially initialised at function exit"));
    assert!(
        exit_diag.is_none(),
        "expected no partial-init exit diag for take-param with droppable remainder, got: {:?}",
        fx.diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

#[test]
fn whole_struct_read_on_partial_init_diagnoses() {
    // Reading the binding as a whole when it's PartialInit must be a
    // compile error per §5's "Reading the whole struct requires every
    // field to be valid."
    let mut fx = Fixture::new();
    let p_sym = fx.interner.intern("p");
    let field_y = fx.field_y;
    let cx = LowerCx { interner: &fx.interner, ty_interner: &fx.ty_interner, package: None };
    let mut ic = InferCx::new(&cx, &fx.ty_cx, &fx.lint_cfg, &mut fx.diags);
    let mut env = TyEnv::new();
    let mut valid_set = std::collections::BTreeSet::new();
    valid_set.insert(field_y);
    env.bind_with_state(p_sym, fx.point_ty, BindingState::PartialInit(valid_set));

    // `p` as a whole expression — goes through path-binding read gate.
    let mut access = hir_path_one(ident_from(p_sym), fx.point_ty);
    let ty = synth_expr(&mut env, &mut access, &mut ic);
    assert_eq!(ty, fx.ty_interner.error());
    assert!(
        fx.diags
            .iter()
            .any(|d| d.message.contains("partially initialised") || d.message.contains("partially-initialised")),
        "got: {:?}",
        fx.diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

#[test]
fn reading_valid_field_on_partial_init_succeeds() {
    let mut fx = Fixture::new();
    let p_sym = fx.interner.intern("p");
    let f64 = fx.ty_interner.prim(Primitive::F64);
    let field_y = fx.field_y;
    let cx = LowerCx { interner: &fx.interner, ty_interner: &fx.ty_interner, package: None };
    let mut ic = InferCx::new(&cx, &fx.ty_cx, &fx.lint_cfg, &mut fx.diags);
    let mut env = TyEnv::new();
    let mut valid_set = std::collections::BTreeSet::new();
    valid_set.insert(field_y);
    env.bind_with_state(p_sym, fx.point_ty, BindingState::PartialInit(valid_set));

    // Reading `p.y` — y is still Valid; should succeed.
    let mut access = hir_field(
        hir_path_one(ident_from(p_sym), fx.point_ty),
        ident_from(field_y),
        f64,
    );
    let ty = synth_expr(&mut env, &mut access, &mut ic);
    assert_eq!(ty, f64);
    assert_eq!(fx.typecheck_count(), 0);
}

// Tuple staged-init — a tuple-typed
// `uninit` binding tracks per-element initialisation through the same §5
// `PartialInit` lattice a record does, keyed by the interned decimal
// element index. `out.(0) = e` then `out.(1) = e` drives
// `Uninit → PartialInit({0}) → Valid`, so `return out` (a whole read)
// becomes admissible only after every element is written — mirroring the
// `deserialize` / `properties` record-walk arm over a tuple `U`.
#[test]
fn tuple_index_assign_promotes_uninit_to_valid() {
    let mut fx = Fixture::new();
    let out_sym = fx.interner.intern("out");
    let u16_ty = fx.ty_interner.prim(Primitive::U16);
    let tuple_ty = fx.ty_interner.tuple(vec![u16_ty, u16_ty]);
    // The element keys `type_field_set` interns for a tuple.
    let idx0 = fx.interner.intern("0");
    let idx1 = fx.interner.intern("1");
    let cx = LowerCx { interner: &fx.interner, ty_interner: &fx.ty_interner, package: None };
    let mut ic = InferCx::new(&cx, &fx.ty_cx, &fx.lint_cfg, &mut fx.diags);
    let mut env = TyEnv::new();
    // `uninit out: (u16, u16)`.
    env.bind_with_state(out_sym, tuple_ty, BindingState::Uninit);

    // `out.(0) = e` — element 0 valid; the binding is partially initialised.
    assert!(super::mode::apply_field_assign_transition(
        &mut env, out_sym, idx0, &mut ic,
    ));
    match env.lookup_state(out_sym).unwrap() {
        BindingState::PartialInit(f) => {
            assert!(f.contains(&idx0), "element 0 should be valid");
            assert!(!f.contains(&idx1), "element 1 not yet written");
        }
        other => panic!("expected PartialInit after out.(0), got {other:?}"),
    }

    // `out.(1) = e` — every element valid; the binding promotes to Valid.
    assert!(super::mode::apply_field_assign_transition(
        &mut env, out_sym, idx1, &mut ic,
    ));
    assert_eq!(env.lookup_state(out_sym).unwrap(), BindingState::Valid);
    assert_eq!(fx.typecheck_count(), 0);
}

#[test]
fn unused_helper_ident() {
    // Keep ident-helper warning-free across configurations.
    let interner = Interner::new();
    let _ = ident(&interner, "_anchor");
    let _ = hir_lit_float(TyInterner::new().prim(Primitive::F64));
}
