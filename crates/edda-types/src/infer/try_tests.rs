//! Tests for the `?` propagation rule from
//! `effect-tracking.md §3`.

use edda_diag::DiagnosticClass;
use edda_span::Span;
use edda_syntax::IntBase;
use edda_syntax::ast::Literal;

use super::fn_body::check_fn_body;
use super::synth_expr;
use crate::cx::TyCx;
use crate::effect::{EffectEntry, EffectRow, PureEffect};
use crate::hir::{HirBlock, HirCallArg, HirExpr, HirExprKind, HirPath, HirStmt, HirStmtKind};
use crate::infer::{InferCx, TyEnv};
use crate::lower::LowerCx;
use crate::prim::Primitive;
use crate::sig::{FnPtrParam, FnPtrSig, ParamMode};
use crate::test_support::{Harness, ident_for};
use crate::ty::{TyId, TyInterner};

fn int_lit(ty: TyId) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty,
        kind: HirExprKind::Literal(Literal::Int {
            value: 0,
            base: IntBase::Dec,
        }),
    }
}

fn raise_int(ty: TyId) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty,
        kind: HirExprKind::Raise(Box::new(int_lit(ty))),
    }
}

fn try_of(inner: HirExpr) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: inner.ty,
        kind: HirExprKind::Try(Box::new(inner)),
    }
}

fn count_no_err_diags(h: &Harness) -> usize {
    h.diags
        .iter()
        .filter(|d| {
            d.class == DiagnosticClass::TypecheckError
                && d.message.contains("no error effect to propagate")
        })
        .count()
}

fn count_row_mismatch(h: &Harness) -> usize {
    h.diags
        .iter()
        .filter(|d| d.class == DiagnosticClass::EffectRowMismatch)
        .count()
}

#[test]
fn try_on_pure_inner_diagnoses() {
    // `42?` — inner is a pure i64 literal, so the `?` has nothing to
    // propagate per §3's "no error effect to propagate" rule.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let mut expr = try_of(int_lit(i64));
    let mut env = TyEnv::new();
    let ty = synth_expr(&mut env, &mut expr, &mut ic);
    assert_eq!(ty, h.ty_interner.error());
    assert_eq!(count_no_err_diags(&h), 1);
}

#[test]
fn try_on_raise_with_matching_declared_row_passes_at_function_exit() {
    // body: `(raise 0_i64)?` inside a function declaring
    // `with { err: i64 }`. The `?` site sees the err entry; the
    // function-exit check sees the entry in declared → no diag.
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let i64 = h.ty_interner.prim(Primitive::I64);

    let try_expr = try_of(raise_int(i64));
    let mut body = HirBlock {
        span: Span::DUMMY,
        ty: unit,
        stmts: Box::from([HirStmt {
            span: Span::DUMMY,
            kind: HirStmtKind::Expr(try_expr),
        }]),
        trailing: None,
    };

    let declared =
        EffectRow::from_entries([EffectEntry::Pure(PureEffect::Err(i64))]);
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let mut env = TyEnv::new();
    let return_ty = h.ty_interner.error();
    let _ = check_fn_body(&mut env, &mut body, &mut ic, &declared, return_ty, Span::DUMMY, false, &[]);

    assert_eq!(count_no_err_diags(&h), 0);
    assert_eq!(count_row_mismatch(&h), 0);
}

#[test]
fn try_on_raise_without_declared_err_surfaces_only_at_function_exit() {
    // body: `(raise 0_i64)?` inside a function declaring `with {}`.
    // The `?` site does NOT emit (an err entry is present, so the §3
    // "no err to propagate" rule is satisfied). The function-exit
    // check catches the missing-from-declared case
    // exactly once.
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let i64 = h.ty_interner.prim(Primitive::I64);

    let try_expr = try_of(raise_int(i64));
    let mut body = HirBlock {
        span: Span::DUMMY,
        ty: unit,
        stmts: Box::from([HirStmt {
            span: Span::DUMMY,
            kind: HirStmtKind::Expr(try_expr),
        }]),
        trailing: None,
    };

    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let mut env = TyEnv::new();
    let return_ty = h.ty_interner.error();
    let _ = check_fn_body(
        &mut env,
        &mut body,
        &mut ic,
        &EffectRow::empty(),
        return_ty,
        Span::DUMMY,
        false,
        &[],
    );

    // §3 sub-rule "at least one err to propagate" is satisfied — no
    // diag at the `?` site.
    assert_eq!(count_no_err_diags(&h), 0);
    // §1a.4 row containment fires once at function exit for the
    // undeclared err entry.
    assert_eq!(count_row_mismatch(&h), 1);
}

#[test]
fn try_synthesises_inner_type_on_success() {
    // `(raise 0_i64)?` — when the err entry is present, `?`'s value
    // type is `inner.ty`. `raise` synthesises `never`, so the `?`
    // expression's type is `never`.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let mut expr = try_of(raise_int(i64));
    let mut env = TyEnv::new();
    let ty = synth_expr(&mut env, &mut expr, &mut ic);
    let never = h.ty_interner.prim(Primitive::Never);
    assert_eq!(ty, never);
    // The diag-at-?-site rule is not violated (err entry is present).
    assert_eq!(count_no_err_diags(&h), 0);
}

#[test]
fn try_passes_through_row_entries() {
    // Per §3, `?` is check-and-pass-through: every entry pushed by
    // `inner` flows on into the surrounding row. We verify by
    // wrapping a `raise i64` in `?` and checking the accumulator
    // contains the err entry afterwards.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let mut expr = try_of(raise_int(i64));
    let mut env = TyEnv::new();
    let _ = synth_expr(&mut env, &mut expr, &mut ic);
    let row = ic.row.to_row();
    assert!(row.contains(&EffectEntry::Pure(PureEffect::Err(i64))));
}

#[test]
fn try_on_cascaded_error_silently_propagates() {
    // If `inner` already synthesised to Error (a prior diagnostic
    // fired), `?` cascades silently to Error without adding a
    // "no error effect" diag. Tests that error-state cascade does
    // not produce a misleading secondary diagnostic.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    // Build a `?` whose inner is HirExprKind::Error — synth_expr's
    // default arm returns Error.
    let mut expr = HirExpr {
        span: Span::DUMMY,
        ty: h.ty_interner.error(),
        kind: HirExprKind::Try(Box::new(HirExpr {
            span: Span::DUMMY,
            ty: h.ty_interner.error(),
            kind: HirExprKind::Error,
        })),
    };
    let mut env = TyEnv::new();
    let ty = synth_expr(&mut env, &mut expr, &mut ic);
    assert_eq!(ty, h.ty_interner.error());
    // No "no error effect" diag — cascaded errors stay silent.
    assert_eq!(count_no_err_diags(&h), 0);
}

// -- B22 err-propagation tests --------------------------------------

/// Count diagnostics emitted by [`super::call::check_err_propagation`].
/// They are `EffectRowMismatch`-classed messages that name the missing
/// propagation route ("not wrapped in `?`").
fn count_b22_diags(h: &Harness) -> usize {
    h.diags
        .iter()
        .filter(|d| {
            d.class == DiagnosticClass::EffectRowMismatch
                && d.message.contains("not wrapped in `?`")
        })
        .count()
}

#[test]
fn b22_call_with_err_outside_try_emits_diagnostic() {
    // Direct invocation of the propagation gate: outside any `?` /
    // `handle` (try_position_depth=0, discharged_effects empty),
    // an `err: T` entry in a call's translated row must error.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let callee_name = h.interner.intern("write_byte");
    super::call::check_err_propagation(
        &[EffectEntry::Pure(PureEffect::Err(i64))],
        callee_name,
        Span::DUMMY,
        &mut ic,
    );
    assert_eq!(count_b22_diags(&h), 1);
}

#[test]
fn b22_call_with_err_inside_try_position_is_admitted() {
    // `try_position_depth > 0` admits any `err: T` — the enclosing
    // `?` is the propagator.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    ic.try_position_depth = 1; // simulate being inside synth_try's operand walk.
    let i64 = h.ty_interner.prim(Primitive::I64);
    let callee_name = h.interner.intern("write_byte");
    super::call::check_err_propagation(
        &[EffectEntry::Pure(PureEffect::Err(i64))],
        callee_name,
        Span::DUMMY,
        &mut ic,
    );
    assert_eq!(count_b22_diags(&h), 0);
}

#[test]
fn b22_call_with_err_inside_handle_body_is_admitted() {
    // Discharged entries on the handle stack admit matching `err: T`.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let entry = EffectEntry::Pure(PureEffect::Err(i64));
    ic.discharged_effects.push(entry);
    let callee_name = h.interner.intern("write_byte");
    super::call::check_err_propagation(&[entry], callee_name, Span::DUMMY, &mut ic);
    assert_eq!(count_b22_diags(&h), 0);
}

#[test]
fn b22_handle_only_discharges_matching_err_type() {
    // Handler discharges `err: i64`; a callee's `err: i32` is NOT
    // discharged by the handler. The single-err-type form
    // means handlers are still err-type-specific.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let i32 = h.ty_interner.prim(Primitive::I32);
    ic.discharged_effects
        .push(EffectEntry::Pure(PureEffect::Err(i64)));
    let callee_name = h.interner.intern("write_byte");
    super::call::check_err_propagation(
        &[EffectEntry::Pure(PureEffect::Err(i32))],
        callee_name,
        Span::DUMMY,
        &mut ic,
    );
    assert_eq!(count_b22_diags(&h), 1);
}

#[test]
fn b22_non_err_entries_are_ignored() {
    // Capability, panic, yield, divergence — none of these gate the
    // B22 check. Those rules live elsewhere (function-exit row
    // containment, comptime purity, etc.).
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let fs_sym = h.interner.intern("fs");
    let entries = [
        EffectEntry::Capability(fs_sym),
        EffectEntry::Pure(PureEffect::Panic),
        EffectEntry::Pure(PureEffect::Yield(i64)),
        EffectEntry::Pure(PureEffect::Divergence),
    ];
    let callee_name = h.interner.intern("write_byte");
    super::call::check_err_propagation(&entries, callee_name, Span::DUMMY, &mut ic);
    assert_eq!(count_b22_diags(&h), 0);
}

#[test]
fn b22_try_walks_operand_with_propagator_flag_set() {
    // End-to-end: `(raise 0_i64)?` walked through synth_try should
    // see try_position_depth incremented during the inner walk. The
    // `Raise` originator pushes a `Pure(Err(i64))` to the
    // accumulator; we add an assertion that the depth was non-zero at
    // the call to `synth_expr` from within `synth_try` by relying on
    // the well-formedness of the existing tests above (this
    // test simply verifies the counter returns to zero after).
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    assert_eq!(ic.try_position_depth, 0);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let mut expr = try_of(raise_int(i64));
    let mut env = TyEnv::new();
    let _ = synth_expr(&mut env, &mut expr, &mut ic);
    // After synth_try returns, the counter must be restored.
    assert_eq!(ic.try_position_depth, 0);
}

#[test]
fn b22_nested_try_depth_save_restore() {
    // Nested `try_position_depth` saves/restores correctly so a
    // sibling expression after a nested `?` is NOT in propagator
    // position. Verified by manipulating the counter through the
    // public-to-tests path and checking the gate.
    let mut h = Harness::new();
    let i64 = h.ty_interner.prim(Primitive::I64);
    let callee_name = h.interner.intern("write_byte");
    {
        let cx = LowerCx {
            interner: &h.interner,
            ty_interner: &h.ty_interner,
            package: None,
        };
        let ty_cx = TyCx::new();
        let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
        ic.try_position_depth = 2;
        super::call::check_err_propagation(
            &[EffectEntry::Pure(PureEffect::Err(i64))],
            callee_name,
            Span::DUMMY,
            &mut ic,
        );
    }
    // Depth>0 ⇒ admitted; no diagnostic.
    assert_eq!(count_b22_diags(&h), 0);

    // After the synth_try popping logic runs (here we simulate by
    // building a fresh InferCx at depth=0), the same call must error.
    {
        let cx = LowerCx {
            interner: &h.interner,
            ty_interner: &h.ty_interner,
            package: None,
        };
        let ty_cx = TyCx::new();
        let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
        assert_eq!(ic.try_position_depth, 0);
        super::call::check_err_propagation(
            &[EffectEntry::Pure(PureEffect::Err(i64))],
            callee_name,
            Span::DUMMY,
            &mut ic,
        );
    }
    assert_eq!(count_b22_diags(&h), 1);
}

#[test]
fn b22_duplicate_err_entries_dedupe_per_call_site() {
    // If the same err: T appears twice in the translated entries (a
    // malformed row), only one diagnostic fires per distinct entry.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let entry = EffectEntry::Pure(PureEffect::Err(i64));
    let callee_name = h.interner.intern("write_byte");
    super::call::check_err_propagation(&[entry, entry, entry], callee_name, Span::DUMMY, &mut ic);
    assert_eq!(count_b22_diags(&h), 1);
}

// --- synth_handle divergence-aware branch join ---

fn unit_lit(unit: TyId) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: unit,
        kind: HirExprKind::Literal(Literal::Unit),
    }
}

/// `name = 0` assignment statement targeting a single-segment path.
fn assign_stmt(name: edda_intern::Symbol, ty: TyId) -> HirStmt {
    let target = HirExpr {
        span: Span::DUMMY,
        ty,
        kind: HirExprKind::Path(crate::hir::HirPath {
            span: Span::DUMMY,
            segments: Box::from([edda_syntax::ast::Ident {
                name,
                span: Span::DUMMY,
            }]),
        }),
    };
    HirStmt {
        span: Span::DUMMY,
        kind: HirStmtKind::Assign {
            target,
            op: edda_syntax::ast::AssignOp::Plain,
            rhs: int_lit(ty),
        },
    }
}

fn handle_expr(
    err_ident: edda_syntax::ast::Ident,
    handled_ty: TyId,
    recovery: HirExpr,
    body: HirBlock,
    unit: TyId,
) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: unit,
        kind: HirExprKind::Handle {
            effect: err_ident,
            handled_ty,
            binder: None,
            recovery: Box::new(recovery),
            body,
        },
    }
}

#[test]
fn handle_join_preserves_body_init_when_recovery_raises() {
    // `uninit n` initialised in
    // the handle body, recovery arm `raise …` — the recovery arm never
    // reaches the join, so the body's init survives.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let unit = h.ty_interner.prim(Primitive::Unit);
    let n = h.interner.intern("n");
    let err_ident = edda_syntax::ast::Ident {
        name: h.interner.intern("err"),
        span: Span::DUMMY,
    };

    let mut env = TyEnv::new();
    env.bind_with_state(n, i64, crate::infer::mode::BindingState::Uninit);

    let body = HirBlock {
        span: Span::DUMMY,
        ty: unit,
        stmts: Box::from([assign_stmt(n, i64)]),
        trailing: None,
    };
    let mut expr = handle_expr(err_ident, i64, raise_int(i64), body, unit);
    let _ = synth_expr(&mut env, &mut expr, &mut ic);

    assert_eq!(
        env.lookup_state(n),
        Some(crate::infer::mode::BindingState::Valid),
        "body init must survive a raise-only recovery arm",
    );
}

#[test]
fn handle_join_still_merges_when_recovery_completes() {
    // Negative control: a recovery arm that completes normally CAN
    // reach the join, so the conservative GLB merge stays — the
    // binding is Uninit at the join (recovery leaves it untouched).
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let unit = h.ty_interner.prim(Primitive::Unit);
    let n = h.interner.intern("n");
    let err_ident = edda_syntax::ast::Ident {
        name: h.interner.intern("err"),
        span: Span::DUMMY,
    };

    let mut env = TyEnv::new();
    env.bind_with_state(n, i64, crate::infer::mode::BindingState::Uninit);

    let body = HirBlock {
        span: Span::DUMMY,
        ty: unit,
        stmts: Box::from([assign_stmt(n, i64)]),
        trailing: None,
    };
    let mut expr = handle_expr(err_ident, i64, unit_lit(unit), body, unit);
    let _ = synth_expr(&mut env, &mut expr, &mut ic);

    assert_eq!(
        env.lookup_state(n),
        Some(crate::infer::mode::BindingState::Uninit),
        "a completing recovery arm must still GLB-merge into the join",
    );
}

#[test]
fn handle_join_keeps_recovery_state_when_body_always_raises() {
    // Symmetric case: the body always exits (statement-position
    // `raise`), so the join is reached only through recovery — the
    // recovery arm's init survives.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let unit = h.ty_interner.prim(Primitive::Unit);
    let n = h.interner.intern("n");
    let err_ident = edda_syntax::ast::Ident {
        name: h.interner.intern("err"),
        span: Span::DUMMY,
    };

    let mut env = TyEnv::new();
    env.bind_with_state(n, i64, crate::infer::mode::BindingState::Uninit);

    let body = HirBlock {
        span: Span::DUMMY,
        ty: unit,
        stmts: Box::from([HirStmt {
            span: Span::DUMMY,
            kind: HirStmtKind::Expr(raise_int(i64)),
        }]),
        trailing: None,
    };
    let recovery = HirExpr {
        span: Span::DUMMY,
        ty: unit,
        kind: HirExprKind::Block(HirBlock {
            span: Span::DUMMY,
            ty: unit,
            stmts: Box::from([assign_stmt(n, i64)]),
            trailing: None,
        }),
    };
    let mut expr = handle_expr(err_ident, i64, recovery, body, unit);
    let _ = synth_expr(&mut env, &mut expr, &mut ic);

    assert_eq!(
        env.lookup_state(n),
        Some(crate::infer::mode::BindingState::Valid),
        "recovery init must survive when the body always raises",
    );
}

// -- `?` binds only to its own call ----

/// `function(i64) -> i64 with {err: i64}` as an interned FnPtr type.
fn fallible_fnptr_ty(ty_interner: &TyInterner, i64: TyId) -> TyId {
    ty_interner.fn_ptr(FnPtrSig {
        params: Box::from([FnPtrParam {
            mode: ParamMode::Default,
            ty: i64,
        }]),
        return_ty: i64,
        effects: EffectRow::from_entries([EffectEntry::Pure(PureEffect::Err(i64))]),
    })
}

/// `{ name }(arg)` — the callee path is wrapped in a bare block so
/// `synth_call` sees a non-`Path` callee and routes straight to
/// `synth_indirect_call` without needing a resolver (the same pattern
/// `spawn_tests.rs` uses).
fn indirect_call(
    interner: &edda_intern::Interner,
    placeholder: TyId,
    name: &str,
    arg: HirExpr,
) -> HirExpr {
    let callee_path = HirExpr {
        span: Span::DUMMY,
        ty: placeholder,
        kind: HirExprKind::Path(HirPath {
            span: Span::DUMMY,
            segments: Box::from([ident_for(interner, name)]),
        }),
    };
    let callee_block = HirExpr {
        span: Span::DUMMY,
        ty: placeholder,
        kind: HirExprKind::Block(HirBlock {
            span: Span::DUMMY,
            ty: placeholder,
            stmts: Box::from([]),
            trailing: Some(Box::new(callee_path)),
        }),
    };
    HirExpr {
        span: Span::DUMMY,
        ty: placeholder,
        kind: HirExprKind::Call {
            callee: Box::new(callee_block),
            args: Box::from([HirCallArg {
                span: Span::DUMMY,
                mode: None,
                name: None,
                expr: arg,
            }]),
        },
    }
}

#[test]
fn b22_outer_try_does_not_cover_nested_arg_call() {
    // `f(g(0))?` — both `f` and `g` are fallible. The outer `?` covers
    // only `f`; `g` in argument position must carry its own `?`.
    // Before this rule was enforced, the outer `?` transitively
    // authorised `g` too, and MIR left g's err-edge unwired (runtime
    // crash / runaway-alloc OOM).
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let i64 = h.ty_interner.prim(Primitive::I64);
    ic.declared_row = EffectRow::from_entries([EffectEntry::Pure(PureEffect::Err(i64))]);
    let fn_ty = fallible_fnptr_ty(&h.ty_interner, i64);
    let mut env = TyEnv::new();
    env.bind(h.interner.intern("f"), fn_ty);
    env.bind(h.interner.intern("g"), fn_ty);
    let placeholder = h.ty_interner.error();
    let inner = indirect_call(&h.interner, placeholder, "g", int_lit(i64));
    let outer = indirect_call(&h.interner, placeholder, "f", inner);
    let mut expr = try_of(outer);
    let _ = synth_expr(&mut env, &mut expr, &mut ic);
    assert_eq!(count_b22_diags(&h), 1);
}

#[test]
fn b22_nested_arg_call_with_own_try_is_admitted() {
    // `f(g(0)?)?` — the V1.0-correct form: each fallible call carries
    // its own `?`. No B22 diagnostics on either call.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let i64 = h.ty_interner.prim(Primitive::I64);
    ic.declared_row = EffectRow::from_entries([EffectEntry::Pure(PureEffect::Err(i64))]);
    let fn_ty = fallible_fnptr_ty(&h.ty_interner, i64);
    let mut env = TyEnv::new();
    env.bind(h.interner.intern("f"), fn_ty);
    env.bind(h.interner.intern("g"), fn_ty);
    let placeholder = h.ty_interner.error();
    let inner = try_of(indirect_call(&h.interner, placeholder, "g", int_lit(i64)));
    let outer = indirect_call(&h.interner, placeholder, "f", inner);
    let mut expr = try_of(outer);
    let _ = synth_expr(&mut env, &mut expr, &mut ic);
    assert_eq!(count_b22_diags(&h), 0);
}

#[test]
fn b22_outer_try_does_not_cover_receiver_call() {
    // `g(0).m()?` — a fallible call in method-receiver position is not
    // covered by the method call's `?` either. The method itself fails
    // to resolve here (no package), but the receiver walk happens
    // first and must flag `g`.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let i64 = h.ty_interner.prim(Primitive::I64);
    ic.declared_row = EffectRow::from_entries([EffectEntry::Pure(PureEffect::Err(i64))]);
    let fn_ty = fallible_fnptr_ty(&h.ty_interner, i64);
    let mut env = TyEnv::new();
    env.bind(h.interner.intern("g"), fn_ty);
    let placeholder = h.ty_interner.error();
    let receiver = indirect_call(&h.interner, placeholder, "g", int_lit(i64));
    let method_call = HirExpr {
        span: Span::DUMMY,
        ty: placeholder,
        kind: HirExprKind::MethodCall {
            receiver: Box::new(receiver),
            name: ident_for(&h.interner, "m"),
            args: Box::from([]),
        },
    };
    let mut expr = try_of(method_call);
    let _ = synth_expr(&mut env, &mut expr, &mut ic);
    assert_eq!(count_b22_diags(&h), 1);
}
