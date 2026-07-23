//! The `exec_scope_without_spawn` lint: `scope(exec)` blocks with no spawn.

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_syntax::ast::visit::{Visitor, walk_expr};
use edda_syntax::ast::{Block, Expr, ExprKind, FnBody, Ident, ItemKind, ScopeKind};

use crate::ResolvedPackage;
use crate::resolve::ResolveCx;

/// Per-function visitor that records every `scope(exec) <name>` block
/// whose body contains no matching `<name>.spawn`. Diagnostics are
/// pushed eagerly; no post-walk emission needed.
struct ExecScopeLintState<'a, 'i> {
    diags: &'a mut Diagnostics,
    severity: edda_diag::Severity,
    interner: &'i edda_intern::Interner,
}

impl<'a, 'i, 'ast> Visitor<'ast> for ExecScopeLintState<'a, 'i> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        if let ExprKind::Scope {
            kind: ScopeKind::Exec,
            name: Some(binder),
            body,
        } = &expr.kind
        {
            if !body_contains_spawn_of(body, binder) {
                let name_text = self.interner.resolve(binder.name);
                let diag = Diagnostic::new(
                    DiagnosticClass::ExecScopeWithoutSpawn,
                    self.severity,
                    expr.span,
                    format!(
                        "`scope(exec) {name_text}` contains no `{name_text}.spawn`",
                    ),
                )
                .with_note(
                    "remove the scope, or spawn at least one task — an empty exec scope adds no structured-concurrency value",
                );
                self.diags.push(diag);
            }
        }
        // Continue recursing — nested `scope(exec)` blocks are still
        // checked (the inner one may also lack a matching spawn).
        walk_expr(self, expr);
    }
}

fn body_contains_spawn_of(body: &Block, binder: &Ident) -> bool {
    struct SpawnFinder<'a> {
        binder: &'a Ident,
        found: bool,
    }
    impl<'a, 'ast> Visitor<'ast> for SpawnFinder<'a> {
        fn visit_expr(&mut self, expr: &'ast Expr) {
            if self.found {
                return;
            }
            if let ExprKind::Spawn(spawn) = &expr.kind {
                if spawn.scope_name.name == self.binder.name {
                    self.found = true;
                    return;
                }
            }
            walk_expr(self, expr);
        }
    }
    let mut finder = SpawnFinder { binder, found: false };
    finder.visit_block(body);
    finder.found
}

/// Emit `exec_scope_without_spawn` for every `scope(exec) <name> { body }`
/// whose `body` contains no `<name>.spawn` call. Name-matched: nested
/// scopes are tracked independently.
pub fn emit_exec_scope_without_spawn_lints(
    package: &ResolvedPackage,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) {
    let severity = lint_cfg.effective(DiagnosticClass::ExecScopeWithoutSpawn);
    for module in &package.modules {
        let entry = package.graph.module(module.id);
        for item in &entry.ast.items {
            let ItemKind::Function(fn_decl) = &item.kind else {
                continue;
            };
            let FnBody::Block(body_block) = &fn_decl.body else {
                continue;
            };
            let mut state = ExecScopeLintState {
                diags,
                severity,
                interner: cx.interner,
            };
            state.visit_block(body_block);
        }
    }
}
