//! The `binding_should_be_let` lint: `var` declarations never reassigned.

use std::collections::HashSet;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_intern::Symbol;
use edda_span::Span;
use edda_syntax::ast::visit::{Visitor, walk_expr, walk_stmt};
use edda_syntax::ast::{
    BindingMode, CallMode, Expr, ExprKind, FnBody, ItemKind, Pat, PatKind, Stmt, StmtKind,
};

use crate::ResolvedPackage;
use crate::resolve::ResolveCx;
use super::expr_root_path_segment;

/// Per-function-body visitor for the `binding_should_be_let` lint.
///
/// Walks the AST collecting `var NAME` declarations into `var_decls` and
/// the root binding of every *mutation* into `reassigned` (via the shared
/// `expr_root_path_segment` helper). A mutation is either an Assign LHS
/// (`x = …`, `x.f = …`, `x[i] = …`) or a `mutable` / `init` call-argument
/// borrow (`f(mutable x)`, `f(init x.field)`) — the mode-checker
/// classifies that borrow as a write, so
/// the lint must too, or the fill-through-helper idiom would satisfy
/// neither check. After the walk, a `var` whose name is absent
/// from `reassigned` should have been `let`.
///
/// Name-based bookkeeping; block-shadowing produces false negatives (the
/// inner `var x` and outer `var x` collapse) but never false positives —
/// any reassignment of any `x` in the function suppresses the warning.
struct VarLintState {
    var_decls: Vec<(Symbol, Span)>,
    reassigned: HashSet<Symbol>,
}

impl<'ast> Visitor<'ast> for VarLintState {
    fn visit_stmt(&mut self, stmt: &'ast Stmt) {
        match &stmt.kind {
            StmtKind::Let {
                mutability: BindingMode::Mutable,
                pat,
                init,
                ..
            } => {
                collect_pat_bindings(pat, &mut self.var_decls);
                if let Some(e) = init {
                    self.visit_expr(e);
                }
            }
            StmtKind::Assign { target, rhs, .. } => {
                if let Some(root) = expr_root_path_segment(target) {
                    self.reassigned.insert(root);
                }
                self.visit_expr(target);
                self.visit_expr(rhs);
            }
            _ => walk_stmt(self, stmt),
        }
    }

    fn visit_expr(&mut self, expr: &'ast Expr) {
        if let ExprKind::Call { args, .. } | ExprKind::MethodCall { args, .. } = &expr.kind {
            for arg in args {
                if matches!(arg.mode, Some(CallMode::Mutable) | Some(CallMode::Init)) {
                    if let Some(root) = expr_root_path_segment(&arg.expr) {
                        self.reassigned.insert(root);
                    }
                }
            }
        }
        walk_expr(self, expr);
    }
}

fn collect_pat_bindings(pat: &Pat, out: &mut Vec<(Symbol, Span)>) {
    match &pat.kind {
        PatKind::Binding(ident) => out.push((ident.name, ident.span)),
        PatKind::Tuple(ps) => {
            for p in ps {
                collect_pat_bindings(p, out);
            }
        }
        _ => {}
    }
}

/// Emit `binding_should_be_let` warnings for every `var` declaration
/// whose binding is never reassigned in its function body.
///
/// Per-function-body name-based walk. The lint fires when a `var NAME`
/// declaration's `NAME` never appears as the root of any mutation within
/// the same function body. Mutations are Assign LHS stores — including
/// through field access (`obj.field = ...`), tuple index (`tup.0 = ...`),
/// or array index (`arr[i] = ...`) — and `mutable` / `init` call-argument
/// borrows (`f(mutable x)`, `f(init x.field)`), which the mode-checker
/// treats as writes. A `var x` is
/// only flagged when its root never appears in any such mutation.
pub fn emit_binding_should_be_let_lints(
    package: &ResolvedPackage,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) {
    let severity = lint_cfg.effective(DiagnosticClass::BindingShouldBeLet);
    for module in &package.modules {
        let entry = package.graph.module(module.id);
        for item in &entry.ast.items {
            let ItemKind::Function(fn_decl) = &item.kind else {
                continue;
            };
            let FnBody::Block(body_block) = &fn_decl.body else {
                continue;
            };
            let mut state = VarLintState {
                var_decls: Vec::new(),
                reassigned: HashSet::new(),
            };
            state.visit_block(body_block);
            for (name, span) in &state.var_decls {
                if state.reassigned.contains(name) {
                    continue;
                }
                let name_text = cx.interner.resolve(*name);
                let diag = Diagnostic::new(
                    DiagnosticClass::BindingShouldBeLet,
                    severity,
                    *span,
                    format!("`var {name_text}` is never reassigned; use `let`"),
                )
                .with_note(
                    "`var` declares a mutable binding; if the binding never changes, prefer `let`",
                );
                diags.push(diag);
            }
        }
    }
}
