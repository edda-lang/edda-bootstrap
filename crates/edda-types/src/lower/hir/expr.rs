//! Expression and block lowering for the AST → HIR pass.
//!
//! Walks `ast::ExprKind` / `ast::Block` and produces the structurally
//! equivalent HIR. The mapping is mechanical: every variant has a
//! matching HIR variant; the lowering recurses through `Box` / `Vec`
//! and copies `Copy`-typed payloads verbatim.

use edda_diag::{Diagnostics, LintConfig};
use edda_syntax::ast;

use crate::effect::EffectRow;
use crate::hir::{
    HirBlock, HirCapture, HirClosure, HirClosureParam, HirExpr, HirExprKind, HirMatchArm,
    HirSpawn, HirSpawnArg, HirStructLitField,
};
use crate::sig::ParamMode;

use super::super::{LowerCx, row::lower_effect_row, ty::lower_type};
use super::path::{lower_call, lower_call_args, lower_hir_path, lower_path_as_value};
use super::pat::lower_pat;
use super::stmt::lower_stmt;

/// Lower an AST expression to its HIR counterpart.
///
/// The mapping is shape-preserving — sub-expressions recurse through
/// the matching HIR variant. The resulting `HirExpr.ty` is the Error
/// sentinel until bidirectional inference runs.
pub(crate) fn lower_expr(
    expr: &ast::Expr,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> HirExpr {
    let kind = lower_expr_kind(&expr.kind, cx, diags, lint_cfg);
    HirExpr {
        span: expr.span,
        ty: cx.ty_interner.error(),
        kind,
    }
}

fn lower_expr_kind(
    kind: &ast::ExprKind,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> HirExprKind {
    match kind {
        ast::ExprKind::Literal(lit) => HirExprKind::Literal(*lit),
        ast::ExprKind::FString(parts) => HirExprKind::FString(
            parts
                .iter()
                .map(|part| match part {
                    ast::FStringPart::Text(sym) => crate::hir::HirFStringPart::Text(*sym),
                    ast::FStringPart::Slot(e) => crate::hir::HirFStringPart::Slot(Box::new(
                        lower_expr(e, cx, diags, lint_cfg),
                    )),
                })
                .collect(),
        ),
        ast::ExprKind::Path(p) => lower_path_as_value(p, cx),
        ast::ExprKind::Binary { op, lhs, rhs } => HirExprKind::Binary {
            op: *op,
            lhs: Box::new(lower_expr(lhs, cx, diags, lint_cfg)),
            rhs: Box::new(lower_expr(rhs, cx, diags, lint_cfg)),
        },
        ast::ExprKind::Unary { op, expr } => HirExprKind::Unary {
            op: *op,
            expr: Box::new(lower_expr(expr, cx, diags, lint_cfg)),
        },
        ast::ExprKind::Call { callee, args } => lower_call(callee, args, cx, diags, lint_cfg),
        ast::ExprKind::MethodCall {
            receiver,
            name,
            args,
        } => HirExprKind::MethodCall {
            receiver: Box::new(lower_expr(receiver, cx, diags, lint_cfg)),
            name: *name,
            args: lower_call_args(args, cx, diags, lint_cfg),
        },
        ast::ExprKind::Field { receiver, name } => HirExprKind::Field {
            receiver: Box::new(lower_expr(receiver, cx, diags, lint_cfg)),
            name: *name,
        },
        ast::ExprKind::TupleIndex { receiver, index } => HirExprKind::TupleIndex {
            receiver: Box::new(lower_expr(receiver, cx, diags, lint_cfg)),
            index: *index,
        },
        // Comptime-indexed field access (D-22) is an AST-only form: the
        // pass-2 comptime expansion (`edda-comptime`) rewrites every
        // `out.(i)` to a concrete `ExprKind::Field` before AST→HIR runs.
        // Reaching this arm means a `CompField` survived expansion — i.e.
        // it appeared outside a `comptime for` / comptime-driven context.
        // Lower to the `Error` sentinel (mirroring `Spawn`); the
        // comptime-context diagnostic is emitted by the expansion pass,
        // which holds the context to explain the misuse.
        ast::ExprKind::CompField { .. } => HirExprKind::Error,
        ast::ExprKind::Index { receiver, index } => HirExprKind::Index {
            receiver: Box::new(lower_expr(receiver, cx, diags, lint_cfg)),
            index: Box::new(lower_expr(index, cx, diags, lint_cfg)),
        },
        ast::ExprKind::If {
            cond,
            then_block,
            else_branch,
        } => HirExprKind::If {
            cond: Box::new(lower_expr(cond, cx, diags, lint_cfg)),
            then_block: lower_block(then_block, cx, diags, lint_cfg),
            else_branch: else_branch
                .as_ref()
                .map(|e| Box::new(lower_expr(e, cx, diags, lint_cfg))),
        },
        ast::ExprKind::Match { scrutinee, arms } => HirExprKind::Match {
            scrutinee: Box::new(lower_expr(scrutinee, cx, diags, lint_cfg)),
            arms: arms
                .iter()
                .map(|a| lower_match_arm(a, cx, diags, lint_cfg))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        },
        ast::ExprKind::Block(b) => HirExprKind::Block(lower_block(b, cx, diags, lint_cfg)),
        ast::ExprKind::Cast { expr, ty, mode } => HirExprKind::Cast {
            expr: Box::new(lower_expr(expr, cx, diags, lint_cfg)),
            target_ty: lower_type(ty, cx, diags, lint_cfg),
            mode: *mode,
        },
        ast::ExprKind::Range { lo, hi, kind } => HirExprKind::Range {
            lo: lo.as_ref().map(|e| Box::new(lower_expr(e, cx, diags, lint_cfg))),
            hi: hi.as_ref().map(|e| Box::new(lower_expr(e, cx, diags, lint_cfg))),
            kind: *kind,
        },
        ast::ExprKind::Tuple(elems) => HirExprKind::Tuple(lower_exprs(elems, cx, diags, lint_cfg)),
        ast::ExprKind::Array(elems) => HirExprKind::Array(lower_exprs(elems, cx, diags, lint_cfg)),
        ast::ExprKind::StructLit { path, fields } => HirExprKind::StructLit {
            path: lower_hir_path(path),
            fields: fields
                .iter()
                .map(|f| lower_struct_lit_field(f, cx, diags, lint_cfg))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        },
        ast::ExprKind::Loop {
            body,
            label,
            decreases,
        } => HirExprKind::Loop {
            body: lower_block(body, cx, diags, lint_cfg),
            label: *label,
            decreases: decreases
                .as_ref()
                .map(|m| Box::new(lower_expr(m, cx, diags, lint_cfg))),
        },
        ast::ExprKind::For {
            pat,
            iter,
            body,
            label,
        } => HirExprKind::For {
            pat: Box::new(lower_pat(pat, cx, diags, lint_cfg)),
            iter: Box::new(lower_expr(iter, cx, diags, lint_cfg)),
            body: lower_block(body, cx, diags, lint_cfg),
            label: *label,
        },
        ast::ExprKind::Try(inner) => {
            HirExprKind::Try(Box::new(lower_expr(inner, cx, diags, lint_cfg)))
        }
        ast::ExprKind::Await(inner) => {
            HirExprKind::Await(Box::new(lower_expr(inner, cx, diags, lint_cfg)))
        }
        ast::ExprKind::Raise(inner) => {
            HirExprKind::Raise(Box::new(lower_expr(inner, cx, diags, lint_cfg)))
        }
        ast::ExprKind::Panic(inner) => {
            HirExprKind::Panic(Box::new(lower_expr(inner, cx, diags, lint_cfg)))
        }
        ast::ExprKind::Comptime(inner) => {
            HirExprKind::Comptime(Box::new(lower_expr(inner, cx, diags, lint_cfg)))
        }
        ast::ExprKind::ComptimeBlock(b) => {
            HirExprKind::ComptimeBlock(lower_block(b, cx, diags, lint_cfg))
        }
        ast::ExprKind::Scope { kind, name, body } => HirExprKind::Scope {
            kind: *kind,
            name: name.clone(),
            body: lower_block(body, cx, diags, lint_cfg),
        },
        ast::ExprKind::Return(opt) => HirExprKind::Return(
            opt.as_ref()
                .map(|e| Box::new(lower_expr(e, cx, diags, lint_cfg))),
        ),
        ast::ExprKind::Break { label, value } => HirExprKind::Break {
            label: *label,
            value: value
                .as_ref()
                .map(|e| Box::new(lower_expr(e, cx, diags, lint_cfg))),
        },
        ast::ExprKind::Continue { label } => HirExprKind::Continue { label: *label },
        ast::ExprKind::EffectRow(row) => HirExprKind::EffectRow(row.clone()),
        ast::ExprKind::Closure(closure) => lower_closure_kind(closure, cx, diags, lint_cfg),
        ast::ExprKind::Spawn(spawn) => lower_spawn_kind(spawn, cx, diags, lint_cfg),
        // Handle expressions: HIR lowering deferred until effect-discharge
        // semantics are wired into the type checker. `ty` is `None` for
        // the payload-less pure-effect forms (`handle cancellation ->
        // ...`, `handle divergence -> ...`) admitted by the parser —
        // `handled_ty` lowers to the Error sentinel for those; `infer`'s
        // `synth_handle` already rejects any non-`err` effect label by
        // name, independent of `handled_ty`'s validity.
        ast::ExprKind::Handle {
            effect,
            ty,
            binder,
            recovery,
            body,
        } => HirExprKind::Handle {
            effect: *effect,
            handled_ty: ty
                .as_deref()
                .map(|ty| lower_type(ty, cx, diags, lint_cfg))
                .unwrap_or_else(|| cx.ty_interner.error()),
            binder: *binder,
            recovery: Box::new(lower_expr(recovery, cx, diags, lint_cfg)),
            body: lower_block(body, cx, diags, lint_cfg),
        },
        ast::ExprKind::Forall { bound, iter, body } => HirExprKind::Forall {
            bound: *bound,
            iter: Box::new(lower_expr(iter, cx, diags, lint_cfg)),
            body: Box::new(lower_expr(body, cx, diags, lint_cfg)),
        },
        ast::ExprKind::Exists { bound, iter, body } => HirExprKind::Exists {
            bound: *bound,
            iter: Box::new(lower_expr(iter, cx, diags, lint_cfg)),
            body: Box::new(lower_expr(body, cx, diags, lint_cfg)),
        },
        ast::ExprKind::Error => HirExprKind::Error,
    }
}

/// Lower a `function(params) -> ret [with {row}] [captures {caps}] { body }`
/// closure literal (PR-B2). Lower the
/// signature (param types / return / effect row) and the body to their
/// HIR forms; bidirectional inference synthesises the FnPtr type and
/// type-checks the body re-entrantly via `check_fn_body`.
fn lower_closure_kind(
    closure: &ast::Closure,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> HirExprKind {
    let params = closure
        .params
        .iter()
        .map(|p| HirClosureParam {
            span: p.span,
            name: p.name,
            mode: ParamMode::from_ast(p.mode),
            ty: lower_type(&p.ty, cx, diags, lint_cfg),
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let ret_ty = lower_type(&closure.ret, cx, diags, lint_cfg);
    let effects = match &closure.effects {
        Some(row) => lower_effect_row(row, cx, diags, lint_cfg),
        None => EffectRow::empty(),
    };
    let captures = closure
        .captures
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|cap| HirCapture {
            span: cap.span,
            name: cap.name,
            mode: cap.mode,
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let body = lower_block(&closure.body, cx, diags, lint_cfg);
    HirExprKind::Closure(Box::new(HirClosure {
        span: closure.span,
        params,
        ret_ty,
        effects,
        captures,
        body,
    }))
}

/// Lower a `<scope>.spawn (take a [: T] = init, ...)? { body }`
/// structured-concurrency spawn.
/// Mechanical mirror of [`lower_closure_kind`]: each explicit `take`
/// arg's optional type annotation and initialiser lower verbatim; the
/// body lowers via [`lower_block`].
fn lower_spawn_kind(
    spawn: &ast::SpawnExpr,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> HirExprKind {
    let args = spawn
        .args
        .iter()
        .map(|arg| HirSpawnArg {
            span: arg.span,
            name: arg.name,
            ty: arg.ty.as_ref().map(|ty| lower_type(ty, cx, diags, lint_cfg)),
            init: lower_expr(&arg.init, cx, diags, lint_cfg),
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    let body = lower_block(&spawn.body, cx, diags, lint_cfg);
    HirExprKind::Spawn(Box::new(HirSpawn {
        span: spawn.span,
        scope_name: spawn.scope_name,
        args,
        body,
    }))
}

/// Lower an AST block to its HIR counterpart.
///
/// Statements lower in source order; the trailing expression (if any)
/// lowers to a boxed `HirExpr`. The block's value type is the Error
/// sentinel until bidirectional inference fills it from the trailing expression's type
/// (or [`Primitive::Unit`](crate::Primitive::Unit) for blocks without
/// a trailing expression).
pub(crate) fn lower_block(
    block: &ast::Block,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> HirBlock {
    HirBlock {
        span: block.span,
        ty: cx.ty_interner.error(),
        stmts: block
            .stmts
            .iter()
            .map(|s| lower_stmt(s, cx, diags, lint_cfg))
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        trailing: block
            .trailing
            .as_ref()
            .map(|e| Box::new(lower_expr(e, cx, diags, lint_cfg))),
    }
}

/// Lower a list of AST expressions to a boxed slice of HIR expressions.
fn lower_exprs(
    exprs: &[ast::Expr],
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Box<[HirExpr]> {
    exprs
        .iter()
        .map(|e| lower_expr(e, cx, diags, lint_cfg))
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

fn lower_match_arm(
    arm: &ast::MatchArm,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> HirMatchArm {
    HirMatchArm {
        span: arm.span,
        pat: lower_pat(&arm.pat, cx, diags, lint_cfg),
        guard: arm
            .guard
            .as_ref()
            .map(|g| lower_expr(g, cx, diags, lint_cfg)),
        body: lower_expr(&arm.body, cx, diags, lint_cfg),
    }
}

fn lower_struct_lit_field(
    field: &ast::StructLitField,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> HirStructLitField {
    HirStructLitField {
        span: field.span,
        name: field.name,
        mode: field.mode.map(|m| match m {
            ast::CallMode::Mutable => crate::hir::HirCallMode::Mutable,
            ast::CallMode::Take => crate::hir::HirCallMode::Take,
            ast::CallMode::Init => crate::hir::HirCallMode::Init,
        }),
        value: lower_expr(&field.value, cx, diags, lint_cfg),
    }
}
