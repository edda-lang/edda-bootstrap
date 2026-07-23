//! Call-site graded-effect structural check.
//!
//! Per `corpus/edda-codex/language/02-modes-effects-refinements.md` §5,
//! every call site whose callee declares a graded `kind(<var> <= EXPR)`
//! entry must be covered by a matching graded entry on the caller's
//! row. This pass owns the **structural** half of that check: the
//! missing-kind diagnostic, plus a literal-only bound comparison kept
//! as a fallback for builds without the `refine` feature.
//!
//! The §5.4 sum / branch-max / loop-lift **accumulator** and the
//! parameter-referencing bound LIA lift live in
//! [`crate::graded_refine`] under the `refine` feature gate — that
//! pass routes through [`edda_refine::Z3Backend`] and subsumes the
//! literal comparison whenever it runs, so the per-site literal
//! comparison below is `#[cfg(not(feature = "refine"))]`-gated to
//! avoid double-emission with the LIA path.

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_resolve::Resolved;
use edda_span::Span;
use edda_syntax::ast::visit::{self as ast_visit, Visitor};
use edda_syntax::ast::{Expr, ExprKind, FnBody, FnDecl};
#[cfg(not(feature = "refine"))]
use edda_syntax::ast::{Literal, UnOp};

use crate::cx::TyCx;
use crate::effect::{GradedBound, GradedKind};
use crate::lower::LowerCx;
use crate::sig::FnSig;

/// Walk `fn_decl`'s body, discover every call site, and discharge each
/// callee's graded bounds against `caller_sig`.
///
/// Emits [`DiagnosticClass::EffectGradedBoundExceeded`] on every
/// violation. Functions without any graded entries (caller or callee)
/// produce no work.
pub(crate) fn discharge_fn_graded_calls(
    fn_decl: &FnDecl,
    caller_sig: &FnSig,
    ty_cx: &TyCx,
    lower_cx: &LowerCx<'_>,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    let Some(package) = lower_cx.package else {
        return;
    };
    let body_block = match &fn_decl.body {
        FnBody::Block(b) => b,
        FnBody::Extern { .. } => return,
    };

    let mut collector = CallSiteCollector { out: Vec::new() };
    collector.visit_block(body_block);

    for call_expr in collector.out {
        let ExprKind::Call { callee, .. } = &call_expr.kind else {
            continue;
        };
        let ExprKind::Path(path) = &callee.kind else {
            continue;
        };
        let resolved = package.resolutions().lookup_path(path.span);
        let Some(Resolved::Binding(callee_binding)) = resolved else {
            continue;
        };
        let Some(callee_sig) = ty_cx.sig(callee_binding) else {
            continue;
        };
        if callee_sig.graded_bounds.is_empty() {
            continue;
        }
        let callee_entry = package.binding(callee_binding);
        let callee_name = lower_cx.interner.resolve(callee_entry.name).to_string();
        for callee_bound in callee_sig.graded_bounds.iter() {
            check_one_bound(
                callee_bound,
                caller_sig,
                &callee_name,
                call_expr.span,
                lint_cfg,
                diags,
            );
        }
    }
}

/// Discharge one (callee_bound, caller's matching bound) pair at one
/// call site. Emits a diagnostic if the callee's bound is not covered.
fn check_one_bound(
    callee_bound: &GradedBound,
    caller_sig: &FnSig,
    callee_name: &str,
    call_span: Span,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    let sev = lint_cfg.effective(DiagnosticClass::EffectGradedBoundExceeded);
    let kind = callee_bound.kind;
    let caller_bound = caller_sig
        .graded_bounds
        .iter()
        .find(|gb| gb.kind == kind);

    let Some(caller_bound) = caller_bound else {
        let msg = missing_kind_message(kind, callee_name);
        diags.push(Diagnostic::new(
            DiagnosticClass::EffectGradedBoundExceeded,
            sev,
            call_span,
            msg,
        ));
        return;
    };

    // Literal-only bound comparison runs only when the `refine` feature
    // is off. When `refine` is active, [`crate::graded_refine`] runs the
    // full §5.4 accumulator through Z3 and emits the bound-exceeded
    // diagnostic at the caller's bound-span; the literal comparison is
    // skipped to avoid double-emission. See `check_literal_bound`'s
    // cfg-gate.
    check_literal_bound(callee_bound, caller_bound, callee_name, call_span, sev, diags);
}

/// Literal-bound fallback (non-`refine` builds). When `refine` is on,
/// this function is a no-op so the LIA path in `graded_refine` has
/// exclusive ownership of the bound-exceeded diagnostic.
#[cfg(not(feature = "refine"))]
fn check_literal_bound(
    callee_bound: &GradedBound,
    caller_bound: &GradedBound,
    callee_name: &str,
    call_span: Span,
    sev: edda_diag::Severity,
    diags: &mut Diagnostics,
) {
    let caller_val = eval_constant(&caller_bound.bound);
    let callee_val = eval_constant(&callee_bound.bound);
    if let (Some(caller_n), Some(callee_n)) = (caller_val, callee_val)
        && caller_n < callee_n
    {
        let msg = bound_exceeded_message(callee_bound.kind, callee_name, caller_n, callee_n);
        diags.push(Diagnostic::new(
            DiagnosticClass::EffectGradedBoundExceeded,
            sev,
            call_span,
            msg,
        ));
    }
}

/// No-op shim active under the `refine` feature gate. Bound-exceeded
/// diagnostics are emitted by [`crate::graded_refine`] in this build
/// configuration; the literal fallback is excluded to avoid
/// double-emission.
#[cfg(feature = "refine")]
fn check_literal_bound(
    _callee_bound: &GradedBound,
    _caller_bound: &GradedBound,
    _callee_name: &str,
    _call_span: Span,
    _sev: edda_diag::Severity,
    _diags: &mut Diagnostics,
) {
}

/// Diagnostic message body for the missing-graded-kind case.
fn missing_kind_message(kind: GradedKind, callee_name: &str) -> String {
    let k = kind.as_str();
    format!(
        "callee `{callee_name}` declares a graded `{k}` bound but the \
         caller's row has no `{k}(...)` entry to cover it"
    )
}

/// Diagnostic message body for the bound-exceeded case. Only reachable
/// from the literal-only fallback inside `check_one_bound`; the LIA
/// path in `graded_refine` renders its own message text.
#[cfg(not(feature = "refine"))]
fn bound_exceeded_message(
    kind: GradedKind,
    callee_name: &str,
    caller_n: i128,
    callee_n: i128,
) -> String {
    let k = kind.as_str();
    let var = kind.resource_var();
    format!(
        "graded `{k}` bound exceeded at call to `{callee_name}`: caller declares \
         `{k}({var} <= {caller_n})`, callee requires `{k}({var} <= {callee_n})`"
    )
}

/// Evaluate an AST expression as an integer constant. Phase A
/// discharge fragment: integer literals and a leading unary minus.
/// Anything else returns `None` and the obligation is admitted.
#[cfg(not(feature = "refine"))]
fn eval_constant(expr: &Expr) -> Option<i128> {
    match &expr.kind {
        ExprKind::Literal(Literal::Int { value, .. }) => i128::try_from(*value).ok(),
        ExprKind::Unary {
            op: UnOp::Neg,
            expr: inner,
        } => eval_constant(inner).map(|v| -v),
        _ => None,
    }
}

// --- Call-site collection ---------------------------------------------------

/// Collects every direct function-call site reachable from a function
/// body. Method calls are excluded — only `ExprKind::Call` form
/// matches; the surrounding §5 graded-effects check only discharges
/// against named callee signatures.
struct CallSiteCollector<'ast> {
    out: Vec<&'ast Expr>,
}

impl<'ast> Visitor<'ast> for CallSiteCollector<'ast> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        if let ExprKind::Call { .. } = &expr.kind {
            self.out.push(expr);
        }
        ast_visit::walk_expr(self, expr);
    }
}
