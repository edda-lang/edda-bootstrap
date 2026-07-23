//! Return-position borrow-mode region check.
//!
//! A function may return a borrow instead of a value:
//! `function at_ref(v: Vec, i: usize) -> let T { return v.data[i] }`.
//! The `-> let T` / `-> mutable T` mode means the result aliases one of
//! the function's by-reference parameters rather than being moved or
//! copied out. For that to be sound the borrow's *region* must be tied
//! to a parameter the caller still owns — otherwise the returned
//! reference would dangle past the call.
//!
//! This pass enforces that binding without a full region/lifetime
//! system, using two locked rules:
//!
//! - **Signature shape.** A borrow-returning function needs at least one
//!   *tie-able* receiver parameter — a `let` (default) or `mutable`
//!   by-reference parameter the borrow can alias. A `-> mutable T`
//!   return additionally requires a `mutable` receiver (you cannot hand
//!   out a mutable borrow of a read-only argument). A borrow-returning
//!   function may not be declared `stable function`: a returned borrow
//!   is pointer-identity-dependent, which the determinism surface
//!   (`03-verification.md` §7) forbids.
//! - **Body shape.** Every value returned (each `return <place>` and the
//!   function's tail expression) must be a *place rooted at* a tie-able
//!   receiver parameter — `v`, `v.field`, `v.field[i]`, `v.0`, etc. A
//!   borrow of a local, a temporary, or a `take`/`init` parameter cannot
//!   outlive the function body, so it is rejected. For a `-> mutable T`
//!   return the root must be a `mutable` parameter.
//!
//! The check is conservative: any returned form that is not a
//! statically-recognisable place rooted at a receiver is rejected rather
//! than admitted. All diagnostics route through
//! [`DiagnosticClass::ModeViolation`].

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_intern::Symbol;
use edda_span::Span;
use edda_syntax::ast::visit::{self as ast_visit, Visitor};
use edda_syntax::ast::{Expr, ExprKind, FnBody, FnDecl};

use crate::lower::LowerCx;
use crate::sig::{FnSig, ParamMode, ReturnMode};

/// Enforce the return-position borrow-mode rules on `fn_decl`.
///
/// No-op for by-value returns and for `extern` bodies (no AST to walk).
/// Emits one [`DiagnosticClass::ModeViolation`] per offending return
/// site plus the signature-shape diagnostics described in the module
/// docs.
pub(crate) fn discharge_fn_return_mode(
    fn_decl: &FnDecl,
    sig: &FnSig,
    lower_cx: &LowerCx<'_>,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    if sig.return_mode == ReturnMode::ByValue {
        return;
    }

    // The span the signature-shape diagnostics attribute to: the return
    // type if present, else the whole declaration.
    let sig_span = fn_decl
        .return_ty
        .as_ref()
        .map(|t| t.span)
        .unwrap_or(fn_decl.span);

    // Tie-able receivers: `let` (Default) and `mutable` parameters.
    let tieable: Vec<(Symbol, ParamMode)> = sig
        .params
        .iter()
        .filter(|p| matches!(p.mode, ParamMode::Default | ParamMode::Mutable))
        .map(|p| (p.name, p.mode))
        .collect();

    // A `stable function` cannot return a borrow — pointer identity is
    // non-deterministic (`03-verification.md` §7).
    if fn_decl.refinement_stable {
        emit(
            diags,
            lint_cfg,
            sig_span,
            format!(
                "a `stable function` cannot return a `{}` borrow — a returned borrow is \
                 pointer-identity-dependent, which the determinism surface forbids; drop the \
                 `stable` marker or return by value",
                sig.return_mode.keyword(),
            ),
        );
    }

    if tieable.is_empty() {
        emit(
            diags,
            lint_cfg,
            sig_span,
            format!(
                "a `-> {} T` return needs a by-reference parameter to tie the borrow to, but this \
                 function has none — add a `let` or `mutable` parameter the result can alias, or \
                 return by value",
                sig.return_mode.keyword(),
            ),
        );
        // No receiver to root any return against — the per-return checks
        // would all repeat the same point. Stop here.
        return;
    }

    let want_mutable = sig.return_mode == ReturnMode::Mutable;
    if want_mutable && !tieable.iter().any(|(_, m)| *m == ParamMode::Mutable) {
        emit(
            diags,
            lint_cfg,
            sig_span,
            "a `-> mutable T` return requires a `mutable` parameter to borrow from, but every \
             by-reference parameter here is read-only (`let`); declare the receiver `mutable`"
                .to_string(),
        );
    }

    let FnBody::Block(body) = &fn_decl.body else {
        return;
    };

    // Every value the function yields: explicit `return <e>` sites plus
    // the tail-position leaf expressions.
    let mut returns: Vec<&Expr> = Vec::new();
    let mut collector = ReturnCollector { out: &mut returns };
    collector.visit_block(body);
    if let Some(trailing) = &body.trailing {
        collect_tail_leaves(trailing, &mut returns);
    }

    for ret in returns {
        check_return_place(ret, &tieable, want_mutable, sig, lower_cx, lint_cfg, diags);
    }
}

/// Validate that a single returned expression is a place rooted at a
/// tie-able receiver parameter of the required mutability.
fn check_return_place(
    ret: &Expr,
    tieable: &[(Symbol, ParamMode)],
    want_mutable: bool,
    sig: &FnSig,
    lower_cx: &LowerCx<'_>,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    let kw = sig.return_mode.keyword();
    let Some(root) = place_root_sym(ret) else {
        emit(
            diags,
            lint_cfg,
            ret.span,
            format!(
                "a `-> {kw} T` borrow must return a place rooted at a by-reference parameter \
                 (e.g. `v`, `v.field`, `v.field[i]`); this expression is not such a place, so the \
                 borrow would outlive the value it refers to"
            ),
        );
        return;
    };

    let Some((_, root_mode)) = tieable.iter().find(|(name, _)| *name == root) else {
        let rname = lower_cx.interner.resolve(root).to_string();
        emit(
            diags,
            lint_cfg,
            ret.span,
            format!(
                "a `-> {kw} T` borrow must be rooted at a by-reference parameter, but `{rname}` is \
                 not one (a local, a temporary, or a `take`/`init` parameter cannot outlive the \
                 function body)"
            ),
        );
        return;
    };

    if want_mutable && *root_mode != ParamMode::Mutable {
        let rname = lower_cx.interner.resolve(root).to_string();
        emit(
            diags,
            lint_cfg,
            ret.span,
            format!(
                "a `-> mutable T` borrow must be rooted at a `mutable` parameter, but `{rname}` is \
                 a read-only (`let`) parameter"
            ),
        );
    }
}

/// The root binding symbol of a place expression, peeling field, tuple,
/// and index projections. Returns `None` for any expression that is not
/// a path-rooted place.
fn place_root_sym(expr: &Expr) -> Option<Symbol> {
    match &expr.kind {
        // Path head is the root binding. `parse_path` folds dotted field
        // access into segments, so `h.data` is `Path([h, data])`, not an
        // `ExprKind::Field`; take the first segment.
        ExprKind::Path(p) => p.segments.first().map(|s| s.name),
        // `Field` / `TupleIndex` only arise when the receiver is itself a
        // non-path postfix (`d[i].field`, `foo().0`); peel to its root.
        ExprKind::Field { receiver, .. } => place_root_sym(receiver),
        ExprKind::TupleIndex { receiver, .. } => place_root_sym(receiver),
        ExprKind::Index { receiver, .. } => place_root_sym(receiver),
        _ => None,
    }
}

/// Collect the leaf expressions that occupy a function's tail (implicit
/// return) position, descending through block trailers, `if`/`else`
/// branches, and `match` arms.
fn collect_tail_leaves<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
    match &expr.kind {
        ExprKind::Block(b) => {
            if let Some(t) = &b.trailing {
                collect_tail_leaves(t, out);
            }
        }
        ExprKind::If {
            then_block,
            else_branch,
            ..
        } => {
            if let Some(t) = &then_block.trailing {
                collect_tail_leaves(t, out);
            }
            if let Some(e) = else_branch {
                collect_tail_leaves(e, out);
            }
        }
        ExprKind::Match { arms, .. } => {
            for arm in arms {
                collect_tail_leaves(&arm.body, out);
            }
        }
        // An explicit `return` in tail position is collected by the
        // `ReturnCollector`; it produces no tail value of its own.
        ExprKind::Return(_) => {}
        _ => out.push(expr),
    }
}

/// Emit one `mode_violation` at `span` unless the class is suppressed.
fn emit(diags: &mut Diagnostics, lint_cfg: &LintConfig, span: Span, msg: String) {
    let sev = lint_cfg.effective(DiagnosticClass::ModeViolation);
    diags.push(Diagnostic::new(
        DiagnosticClass::ModeViolation,
        sev,
        span,
        msg,
    ));
}

/// AST visitor that gathers the value expression of every explicit
/// `return <value>` reachable from the function body, stopping at
/// closure boundaries.
struct ReturnCollector<'a, 'ast> {
    out: &'a mut Vec<&'ast Expr>,
}

impl<'ast> Visitor<'ast> for ReturnCollector<'_, 'ast> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        match &expr.kind {
            // A `return` inside a closure body returns from the closure,
            // not from us — do not descend.
            ExprKind::Closure(_) => {}
            ExprKind::Return(Some(value)) => {
                self.out.push(value);
                ast_visit::walk_expr(self, expr);
            }
            _ => ast_visit::walk_expr(self, expr),
        }
    }
}
