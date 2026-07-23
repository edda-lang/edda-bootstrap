//! Path / call lowering for the AST → HIR pass.
//!
//! Owns the bound-head decomposition rules: a multi-segment Path whose
//! head resolves to a value binding becomes a `Field` chain (or a
//! `MethodCall` when it heads a call).

use edda_diag::{Diagnostics, LintConfig};
use edda_resolve::{BindingKind, Resolved};
use edda_syntax::ast;

use crate::hir::{HirCallArg, HirCallMode, HirExpr, HirExprKind, HirPath};

use super::super::LowerCx;
use super::expr::lower_expr;

/// Lower a list of AST call arguments to HIR, preserving the
/// optional call-site mode keyword and the optional payload-field name
/// on each argument.
pub(in crate::lower::hir) fn lower_call_args(
    args: &[ast::CallArg],
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Box<[HirCallArg]> {
    args.iter()
        .map(|a| HirCallArg {
            span: a.span,
            mode: a.mode.map(|m| match m {
                ast::CallMode::Mutable => HirCallMode::Mutable,
                ast::CallMode::Take => HirCallMode::Take,
                ast::CallMode::Init => HirCallMode::Init,
            }),
            name: a.name.clone(),
            expr: lower_expr(&a.expr, cx, diags, lint_cfg),
        })
        .collect()
}

/// Lower an AST path to its HIR placeholder. Currently the shape
/// is identical (segments + span); a future pass will attach a
/// resolution field.
pub(in crate::lower::hir) fn lower_hir_path(path: &ast::Path) -> HirPath {
    HirPath {
        span: path.span,
        segments: path.segments.to_vec().into_boxed_slice(),
    }
}

/// Lower a Path AST node in expression position. When the head segment
/// resolves (per the resolver's [`edda_resolve::Resolutions`]) to a
/// value binding (Param/Local) and there are 2+ segments, decompose
/// into a `Field` chain so the inference layer's per-field gate fires
/// correctly. Otherwise emit a plain [`HirExprKind::Path`] that the
/// type-checker resolves through the Resolutions map.
pub(in crate::lower::hir) fn lower_path_as_value(p: &ast::Path, cx: &LowerCx<'_>) -> HirExprKind {
    if p.segments.len() >= 2
        && let Some(pkg) = cx.package
        && let Some(Resolved::Binding(id)) = pkg.resolutions().lookup_path(p.span)
        && matches!(
            pkg.binding(id).kind,
            BindingKind::Param | BindingKind::Local
        )
    {
        return decompose_path_to_field_chain(p, cx);
    }
    HirExprKind::Path(lower_hir_path(p))
}

/// Decompose `Path([head, s1, …, sn])` into
/// `Field { receiver: Field { … Field { receiver: Path([head]), name: s1 } … }, name: sn }`.
fn decompose_path_to_field_chain(p: &ast::Path, cx: &LowerCx<'_>) -> HirExprKind {
    let head_seg = p.segments[0].clone();
    let head_span = head_seg.span;
    let head_path = HirPath {
        span: head_span,
        segments: vec![head_seg].into_boxed_slice(),
    };
    let mut current = HirExpr {
        span: head_span,
        ty: cx.ty_interner.error(),
        kind: HirExprKind::Path(head_path),
    };
    let n = p.segments.len();
    let mut accumulated = head_span;
    for seg in &p.segments[1..(n - 1)] {
        accumulated = accumulated.join(seg.span);
        current = HirExpr {
            span: accumulated,
            ty: cx.ty_interner.error(),
            kind: HirExprKind::Field {
                receiver: Box::new(current),
                name: seg.clone(),
            },
        };
    }
    HirExprKind::Field {
        receiver: Box::new(current),
        name: p.segments[n - 1].clone(),
    }
}

/// Lower a Call AST node. When the callee is a 2+-segment Path whose
/// head resolves to a value binding (Param/Local), rewrite the call
/// into a [`HirExprKind::MethodCall`] whose receiver is the
/// path's prefix (decomposed via [`decompose_path_to_field_chain`])
/// and whose method name is the path's last segment.
pub(in crate::lower::hir) fn lower_call(
    callee: &ast::Expr,
    args: &[ast::CallArg],
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> HirExprKind {
    if let ast::ExprKind::Path(p) = &callee.kind
        && p.segments.len() >= 2
        && let Some(pkg) = cx.package
        && let Some(Resolved::Binding(id)) = pkg.resolutions().lookup_path(p.span)
        && matches!(
            pkg.binding(id).kind,
            BindingKind::Param | BindingKind::Local
        )
    {
        let method_name = p.segments.last().unwrap().clone();
        let receiver = build_path_prefix_receiver(p, cx);
        return HirExprKind::MethodCall {
            receiver: Box::new(receiver),
            name: method_name,
            args: lower_call_args(args, cx, diags, lint_cfg),
        };
    }
    HirExprKind::Call {
        callee: Box::new(lower_expr(callee, cx, diags, lint_cfg)),
        args: lower_call_args(args, cx, diags, lint_cfg),
    }
}

/// Build the receiver expression for a decomposed method call. For
/// `Path([a, b, c, m])` the receiver is `a.b.c`; for `Path([a, m])`
/// the receiver is the single-segment Path `a`.
fn build_path_prefix_receiver(p: &ast::Path, cx: &LowerCx<'_>) -> HirExpr {
    let head_seg = p.segments[0].clone();
    let head_span = head_seg.span;
    let head_path = HirPath {
        span: head_span,
        segments: vec![head_seg].into_boxed_slice(),
    };
    let mut current = HirExpr {
        span: head_span,
        ty: cx.ty_interner.error(),
        kind: HirExprKind::Path(head_path),
    };
    let n = p.segments.len();
    let mut accumulated = head_span;
    for seg in &p.segments[1..(n - 1)] {
        accumulated = accumulated.join(seg.span);
        current = HirExpr {
            span: accumulated,
            ty: cx.ty_interner.error(),
            kind: HirExprKind::Field {
                receiver: Box::new(current),
                name: seg.clone(),
            },
        };
    }
    current
}
