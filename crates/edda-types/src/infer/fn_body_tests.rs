//! Tests for `check_fn_body`'s row-containment check.
//!
//! Each test builds a minimal `HirBlock` by hand (skipping the AST →
//! HIR lowering since we only need the originator nodes), threads
//! `synth_block` through `check_fn_body` with an explicit declared
//! row, and inspects the resulting `Diagnostics` take.

use edda_diag::{DiagnosticClass, LintSeverity};
use edda_span::Span;
use edda_syntax::IntBase;
use edda_syntax::ast::Literal;

use super::check_fn_body;
use crate::cx::TyCx;
use crate::effect::{EffectEntry, EffectRow, PureEffect};
use crate::hir::{HirBlock, HirExpr, HirExprKind, HirStmt, HirStmtKind};
use crate::infer::{InferCx, TyEnv};
use crate::lower::LowerCx;
use crate::prim::Primitive;
use crate::test_support::Harness;
use crate::ty::TyId;

/// Build a `HirBlock` containing only a statement-form expression
/// `expr;` (no trailing value). The block's type is filled by
/// inference; tests use `Primitive::Unit` here as a placeholder so the
/// pre-inference invariant on `HirBlock.ty` holds.
fn block_of_one_stmt(stmt_expr: HirExpr, unit: TyId) -> HirBlock {
    HirBlock {
        span: Span::DUMMY,
        ty: unit,
        stmts: Box::from([HirStmt {
            span: Span::DUMMY,
            kind: HirStmtKind::Expr(stmt_expr),
        }]),
        trailing: None,
    }
}

fn empty_block(unit: TyId) -> HirBlock {
    HirBlock {
        span: Span::DUMMY,
        ty: unit,
        stmts: Box::from([]),
        trailing: None,
    }
}

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

fn panic_str(unit: TyId, str_ty: TyId, str_lit: HirExpr) -> HirExpr {
    let _ = str_ty;
    HirExpr {
        span: Span::DUMMY,
        ty: unit,
        kind: HirExprKind::Panic(Box::new(str_lit)),
    }
}

fn await_lit(ty: TyId) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty,
        kind: HirExprKind::Await(Box::new(int_lit(ty))),
    }
}

fn run_check(
    h: &mut Harness,
    body: &mut HirBlock,
    declared: &EffectRow,
) -> TyId {
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut env = TyEnv::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let return_ty = h.ty_interner.error();
    check_fn_body(&mut env, body, &mut ic, declared, return_ty, Span::DUMMY, false, &[])
}

fn count_row_mismatch(h: &Harness) -> usize {
    h.diags
        .iter()
        .filter(|d| d.class == DiagnosticClass::EffectRowMismatch)
        .count()
}

#[test]
fn empty_body_against_empty_row_is_clean() {
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let mut body = empty_block(unit);
    let _ = run_check(&mut h, &mut body, &EffectRow::empty());
    assert_eq!(count_row_mismatch(&h), 0);
}

#[test]
fn raise_with_matching_declared_row_passes() {
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let declared = EffectRow::from_entries([EffectEntry::Pure(PureEffect::Err(i64))]);
    let mut body = block_of_one_stmt(raise_int(i64), unit);
    let _ = run_check(&mut h, &mut body, &declared);
    assert_eq!(
        count_row_mismatch(&h),
        0,
        "expected no row diagnostics, got {:?}",
        h.diags.iter().map(|d| (d.class, &d.message)).collect::<Vec<_>>(),
    );
}

#[test]
fn raise_against_empty_row_diagnoses() {
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let mut body = block_of_one_stmt(raise_int(i64), unit);
    let _ = run_check(&mut h, &mut body, &EffectRow::empty());
    assert_eq!(count_row_mismatch(&h), 1);
    let msg = &h.diags.iter().find(|d| d.class == DiagnosticClass::EffectRowMismatch).unwrap().message;
    assert!(msg.contains("err: i64"), "msg: {msg}");
    assert!(msg.contains("declared row is `{}`"), "msg: {msg}");
}

#[test]
fn await_with_matching_declared_row_passes() {
    // `05-concurrency-coherence.md` §2.2 — `await`'s row is
    // `{cancellation}`; declaring it admits the pushed entry.
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let declared = EffectRow::from_entries([EffectEntry::Pure(PureEffect::Cancellation)]);
    let mut body = block_of_one_stmt(await_lit(i64), unit);
    let _ = run_check(&mut h, &mut body, &declared);
    assert_eq!(
        count_row_mismatch(&h),
        0,
        "expected no row diagnostics, got {:?}",
        h.diags.iter().map(|d| (d.class, &d.message)).collect::<Vec<_>>(),
    );
}

#[test]
fn await_against_empty_row_diagnoses() {
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let mut body = block_of_one_stmt(await_lit(i64), unit);
    let _ = run_check(&mut h, &mut body, &EffectRow::empty());
    assert_eq!(count_row_mismatch(&h), 1);
    let msg = &h.diags.iter().find(|d| d.class == DiagnosticClass::EffectRowMismatch).unwrap().message;
    assert!(msg.contains("cancellation"), "msg: {msg}");
    assert!(msg.contains("declared row is `{}`"), "msg: {msg}");
}

#[test]
fn await_inside_its_spawning_scope_exec_is_absorbed() {
    // `05-concurrency-coherence.md` §2.2
    // — an `.await` lexically inside the `scope(exec)` block that
    // spawned it is "absorbed by the enclosing `scope(exec)`", so the
    // enclosing function need not declare `cancellation` even though
    // the body performs an `.await`. Only the `Executor` capability
    // entry is declared here; a row-mismatch would mean the absorption
    // regressed.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let exec_sym = h.interner.intern("exec");
    let exec_ty = h.ty_interner.capability(crate::CapabilityType::Executor);
    let declared = EffectRow::from_entries([EffectEntry::Capability(exec_sym)]);
    let mut env = TyEnv::new();
    env.bind(exec_sym, exec_ty);
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let return_ty = h.ty_interner.error();
    let scope_expr = HirExpr {
        span: Span::DUMMY,
        ty: unit,
        kind: HirExprKind::Scope {
            kind: edda_syntax::ast::ScopeKind::Exec,
            name: None,
            body: block_of_one_stmt(await_lit(i64), unit),
        },
    };
    let mut body = block_of_one_stmt(scope_expr, unit);
    let _ = check_fn_body(&mut env, &mut body, &mut ic, &declared, return_ty, Span::DUMMY, false, &[]);
    assert_eq!(
        h.diags
            .iter()
            .filter(|d| d.class == DiagnosticClass::EffectRowMismatch)
            .count(),
        0,
        "expected cancellation to be absorbed by the enclosing scope(exec), got {:?}",
        h.diags.iter().map(|d| (d.class, &d.message)).collect::<Vec<_>>(),
    );
}

#[test]
fn panic_with_matching_declared_row_passes() {
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let str_ty = h.ty_interner.prim(Primitive::String);
    let sym = h.interner.intern("boom");
    let str_lit = HirExpr {
        span: Span::DUMMY,
        ty: str_ty,
        kind: HirExprKind::Literal(Literal::Str(sym)),
    };
    let declared = EffectRow::from_entries([EffectEntry::Pure(PureEffect::Panic)]);
    let mut body = block_of_one_stmt(panic_str(unit, str_ty, str_lit), unit);
    let _ = run_check(&mut h, &mut body, &declared);
    assert_eq!(count_row_mismatch(&h), 0);
}

#[test]
fn panic_against_empty_row_diagnoses() {
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let str_ty = h.ty_interner.prim(Primitive::String);
    let sym = h.interner.intern("oops");
    let str_lit = HirExpr {
        span: Span::DUMMY,
        ty: str_ty,
        kind: HirExprKind::Literal(Literal::Str(sym)),
    };
    let mut body = block_of_one_stmt(panic_str(unit, str_ty, str_lit), unit);
    let _ = run_check(&mut h, &mut body, &EffectRow::empty());
    assert_eq!(count_row_mismatch(&h), 1);
    let msg = &h.diags.iter().find(|d| d.class == DiagnosticClass::EffectRowMismatch).unwrap().message;
    assert!(msg.contains("`panic`"), "msg: {msg}");
}

#[test]
fn raise_against_wrong_err_type_diagnoses() {
    // Body raises `err: i64`, declared row lists `err: String` — the
    // mismatched payload type means the entry isn't contained.
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let str_ty = h.ty_interner.prim(Primitive::String);
    let declared = EffectRow::from_entries([EffectEntry::Pure(PureEffect::Err(str_ty))]);
    let mut body = block_of_one_stmt(raise_int(i64), unit);
    let _ = run_check(&mut h, &mut body, &declared);
    assert_eq!(count_row_mismatch(&h), 1);
}

#[test]
fn multiple_extras_each_diagnose_independently() {
    // Body performs `raise i64` and then `panic "x"`; declared row is
    // empty. Two distinct entries → two diagnostics.
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let str_ty = h.ty_interner.prim(Primitive::String);
    let sym = h.interner.intern("msg");
    let str_lit = HirExpr {
        span: Span::DUMMY,
        ty: str_ty,
        kind: HirExprKind::Literal(Literal::Str(sym)),
    };
    let mut body = HirBlock {
        span: Span::DUMMY,
        ty: unit,
        stmts: Box::from([
            HirStmt {
                span: Span::DUMMY,
                kind: HirStmtKind::Expr(raise_int(i64)),
            },
            HirStmt {
                span: Span::DUMMY,
                kind: HirStmtKind::Expr(panic_str(unit, str_ty, str_lit)),
            },
        ]),
        trailing: None,
    };
    let _ = run_check(&mut h, &mut body, &EffectRow::empty());
    assert_eq!(count_row_mismatch(&h), 2);
}

#[test]
fn duplicate_entries_collapse_before_diagnostic() {
    // Body performs `raise i64` twice; declared row is empty. The
    // accumulator records both pushes, but `to_row` canonicalises so
    // only one diagnostic fires for the single entry not in declared.
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let mut body = HirBlock {
        span: Span::DUMMY,
        ty: unit,
        stmts: Box::from([
            HirStmt {
                span: Span::DUMMY,
                kind: HirStmtKind::Expr(raise_int(i64)),
            },
            HirStmt {
                span: Span::DUMMY,
                kind: HirStmtKind::Expr(raise_int(i64)),
            },
        ]),
        trailing: None,
    };
    let _ = run_check(&mut h, &mut body, &EffectRow::empty());
    assert_eq!(count_row_mismatch(&h), 1);
}

#[test]
fn declared_with_unused_entries_is_not_a_diagnostic() {
    // Containment is one-directional: declared ⊇ actual is fine even
    // when declared has entries the body did not perform. Function
    // authors may declare richer rows than they exercise; the surface
    // formatter does not normalise.
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let declared = EffectRow::from_entries([
        EffectEntry::Pure(PureEffect::Panic),
        EffectEntry::Pure(PureEffect::Err(h.ty_interner.prim(Primitive::I64))),
    ]);
    let mut body = empty_block(unit);
    let _ = run_check(&mut h, &mut body, &declared);
    assert_eq!(count_row_mismatch(&h), 0);
}

#[test]
fn check_fn_body_restores_previous_declared_row_and_accumulator() {
    // After `check_fn_body` returns, the InferCx's `declared_row` and
    // `row` must be back to their pre-call values so the function is
    // re-entrant for future nested-body waves.
    let mut h = Harness::new();
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);

    // Install a non-default state.
    let i32 = h.ty_interner.prim(Primitive::I32);
    let outer_declared =
        EffectRow::from_entries([EffectEntry::Pure(PureEffect::Err(i32))]);
    ic.declared_row = outer_declared.clone();
    ic.row.push(EffectEntry::Pure(PureEffect::Panic));
    let outer_acc_cp = ic.row.checkpoint();

    // Run a check with a different declared row.
    let unit = h.ty_interner.prim(Primitive::Unit);
    let mut env = TyEnv::new();
    let mut body = empty_block(unit);
    let inner_declared = EffectRow::empty();
    let inner_return_ty = h.ty_interner.error();
    let _ = check_fn_body(
        &mut env,
        &mut body,
        &mut ic,
        &inner_declared,
        inner_return_ty,
        Span::DUMMY,
        false,
        &[],
    );

    // The state we set up before the call must be intact.
    assert_eq!(ic.declared_row, outer_declared);
    assert_eq!(ic.row.checkpoint(), outer_acc_cp);
}

// Divergence-injection tests.
//
// Per `corpus/edda-codex/language/03-verification.md` §5, a function
// whose call-graph SCC is recursive without any member supplying a
// `decreases` measure must accumulate `Pure(Divergence)` at entry; the
// existing row-containment check then surfaces the missing entry when
// the declared row does not admit it. A `loop` without a `decreases`
// measure pushes the same entry at the loop site.

fn run_check_with_divergence(
    h: &mut Harness,
    body: &mut HirBlock,
    declared: &EffectRow,
    inject_divergence: bool,
) -> TyId {
    let cx = LowerCx {
        interner: &h.interner,
        ty_interner: &h.ty_interner,
        package: None,
    };
    let ty_cx = TyCx::new();
    let mut env = TyEnv::new();
    let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
    let return_ty = h.ty_interner.error();
    check_fn_body(
        &mut env,
        body,
        &mut ic,
        declared,
        return_ty,
        Span::DUMMY,
        inject_divergence,
        &[],
    )
}

fn termination_unproven_count(h: &Harness) -> usize {
    h.diags
        .iter()
        .filter(|d| d.class == DiagnosticClass::TerminationUnproven)
        .count()
}

fn divergence_not_admitted_count(h: &Harness) -> usize {
    h.diags
        .iter()
        .filter(|d| d.class == DiagnosticClass::DivergenceNotAdmitted)
        .count()
}

fn empty_loop_block(unit: TyId, decreases: Option<HirExpr>) -> HirExpr {
    HirExpr {
        span: Span::DUMMY,
        ty: unit,
        kind: HirExprKind::Loop {
            body: empty_block(unit),
            label: None,
            decreases: decreases.map(Box::new),
        },
    }
}

#[test]
fn injected_divergence_against_empty_row_diagnoses_once() {
    // A recursive function without `decreases` (modelled here by
    // `inject_divergence=true`) and without `divergence` in its
    // declared row surfaces exactly one `termination_unproven` naming
    // the missing decreases measure. Per the Stream-D projection,
    // function-entry injection routes to TerminationUnproven so the
    // remediation guidance ("supply `decreases`") is foregrounded.
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let mut body = empty_block(unit);
    let _ = run_check_with_divergence(&mut h, &mut body, &EffectRow::empty(), true);
    assert_eq!(termination_unproven_count(&h), 1);
    assert_eq!(divergence_not_admitted_count(&h), 0);
}

#[test]
fn injected_divergence_against_declared_divergence_row_passes() {
    // Same recursive-without-decreases shape, but the user has admitted
    // `with {divergence}` per `03-verification.md` §5 *Divergence as
    // positive admission* — no diagnostic should fire.
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let declared = EffectRow::from_entries([EffectEntry::Pure(PureEffect::Divergence)]);
    let mut body = empty_block(unit);
    let _ = run_check_with_divergence(&mut h, &mut body, &declared, true);
    assert_eq!(termination_unproven_count(&h), 0);
    assert_eq!(divergence_not_admitted_count(&h), 0);
}

#[test]
fn no_injection_with_clean_body_is_silent() {
    // Non-recursive function with a clean body — no divergence should
    // appear in the accumulator and no diagnostic should fire.
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let mut body = empty_block(unit);
    let _ = run_check_with_divergence(&mut h, &mut body, &EffectRow::empty(), false);
    assert_eq!(termination_unproven_count(&h), 0);
    assert_eq!(divergence_not_admitted_count(&h), 0);
}

#[test]
fn loop_without_decreases_pushes_divergence() {
    // A bare `loop {}` with no `decreases` measure contributes
    // `Pure(Divergence)` at the loop site; the function-exit row check
    // catches it because the declared row is empty. Per the Stream-D
    // projection, body-pushed divergence routes to
    // `divergence_not_admitted` (the function isn't itself injected
    // — the loop body is).
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let loop_expr = empty_loop_block(unit, None);
    let mut body = block_of_one_stmt(loop_expr, unit);
    let _ = run_check_with_divergence(&mut h, &mut body, &EffectRow::empty(), false);
    assert_eq!(divergence_not_admitted_count(&h), 1);
    assert_eq!(termination_unproven_count(&h), 0);
}

#[test]
fn loop_with_decreases_does_not_push_divergence() {
    // `loop decreases <expr> { ... }` opts out of divergence
    // injection. The measure expression is synthesised for its side-
    // effect (its `ty` field is populated); no divergence appears in
    // the accumulator. C5 will discharge the per-iteration termination
    // obligation; this test only verifies the divergence-suppression
    // half of the rule.
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let measure = int_lit(i64);
    let loop_expr = empty_loop_block(unit, Some(measure));
    let mut body = block_of_one_stmt(loop_expr, unit);
    let _ = run_check_with_divergence(&mut h, &mut body, &EffectRow::empty(), false);
    assert_eq!(termination_unproven_count(&h), 0);
    assert_eq!(divergence_not_admitted_count(&h), 0);
}

#[test]
fn loop_without_decreases_admitted_when_declared() {
    // `loop {}` inside a function declaring `with {divergence}` —
    // accumulator picks up the entry but the row-containment check
    // sees it in the declared row and stays silent.
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let declared = EffectRow::from_entries([EffectEntry::Pure(PureEffect::Divergence)]);
    let loop_expr = empty_loop_block(unit, None);
    let mut body = block_of_one_stmt(loop_expr, unit);
    let _ = run_check_with_divergence(&mut h, &mut body, &declared, false);
    assert_eq!(termination_unproven_count(&h), 0);
    assert_eq!(divergence_not_admitted_count(&h), 0);
}

// --- comptime-envelope discharge ---

#[test]
fn comptime_block_panic_does_not_contribute_runtime_panic() {
    // `comptime { panic("…") }` is a compile-time event (codex §3.3);
    // the enclosing function's declared row `{}` must stay clean.
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let panic_expr = HirExpr {
        span: Span::DUMMY,
        ty: unit,
        kind: HirExprKind::Panic(Box::new(int_lit(i64))),
    };
    let comptime_block = HirExpr {
        span: Span::DUMMY,
        ty: unit,
        kind: HirExprKind::ComptimeBlock(block_of_one_stmt(panic_expr, unit)),
    };
    let mut body = block_of_one_stmt(comptime_block, unit);
    let _ = run_check(&mut h, &mut body, &EffectRow::empty());
    assert_eq!(
        count_row_mismatch(&h),
        0,
        "comptime panic must not impose a runtime panic row entry; got {:?}",
        h.diags.iter().map(|d| (d.class, &d.message)).collect::<Vec<_>>(),
    );
}

#[test]
fn comptime_expr_panic_does_not_contribute_runtime_panic() {
    // Forced-expression form `comptime panic("…")` — same rule as the
    // block form.
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let panic_expr = HirExpr {
        span: Span::DUMMY,
        ty: unit,
        kind: HirExprKind::Panic(Box::new(int_lit(i64))),
    };
    let comptime_expr = HirExpr {
        span: Span::DUMMY,
        ty: unit,
        kind: HirExprKind::Comptime(Box::new(panic_expr)),
    };
    let mut body = block_of_one_stmt(comptime_expr, unit);
    let _ = run_check(&mut h, &mut body, &EffectRow::empty());
    assert_eq!(count_row_mismatch(&h), 0);
}

#[test]
fn comptime_block_raise_still_contributes_err() {
    // Negative control: only the comptime envelope ({panic, yield})
    // discharges. An `err: T` contribution inside a comptime body is a
    // purity violation and must stay visible to the row-containment
    // check rather than be silently masked.
    let mut h = Harness::new();
    let unit = h.ty_interner.prim(Primitive::Unit);
    let i64 = h.ty_interner.prim(Primitive::I64);
    let comptime_block = HirExpr {
        span: Span::DUMMY,
        ty: unit,
        kind: HirExprKind::ComptimeBlock(block_of_one_stmt(raise_int(i64), unit)),
    };
    let mut body = block_of_one_stmt(comptime_block, unit);
    let _ = run_check(&mut h, &mut body, &EffectRow::empty());
    assert_eq!(count_row_mismatch(&h), 1);
}
