//! The `unused_closure_capture` lint: captures never referenced in the body.

use std::collections::HashSet;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_intern::Symbol;
use edda_syntax::ast::visit::{Visitor, walk_expr};
use edda_syntax::ast::{Block, Expr, ExprKind, FnBody, ItemKind};

use crate::ResolvedPackage;
use crate::resolve::ResolveCx;

/// Per-function visitor for `unused_closure_capture`.
///
/// On each closure literal: collects the leading segment of every Path
/// reference inside the closure body (covers bare identifiers as well as
/// field-access / method-receiver heads, e.g. `p` in `p.a` or `s.bytes()`)
/// and warns on any capture whose name is not in that set. Continues
/// recursing into nested closures (they're checked independently).
struct ClosureCaptureLintState<'a, 'i> {
    diags: &'a mut Diagnostics,
    severity: edda_diag::Severity,
    interner: &'i edda_intern::Interner,
}

impl<'a, 'i, 'ast> Visitor<'ast> for ClosureCaptureLintState<'a, 'i> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        if let ExprKind::Closure(closure) = &expr.kind {
            if let Some(captures) = &closure.captures {
                if !captures.is_empty() {
                    let used = collect_leading_segment_refs(&closure.body);
                    for capture in captures {
                        if used.contains(&capture.name.name) {
                            continue;
                        }
                        let name_text = self.interner.resolve(capture.name.name);
                        let diag = Diagnostic::new(
                            DiagnosticClass::UnusedClosureCapture,
                            self.severity,
                            capture.span,
                            format!("closure capture `{name_text}` is never referenced in the body"),
                        )
                        .with_note(
                            "remove the capture entry — unused captures change the closure's content-addressed identity without affecting behaviour",
                        );
                        self.diags.push(diag);
                    }
                }
            }
        }
        walk_expr(self, expr);
    }
}

fn collect_leading_segment_refs(body: &Block) -> HashSet<Symbol> {
    struct Collector {
        seen: HashSet<Symbol>,
    }
    impl<'ast> Visitor<'ast> for Collector {
        fn visit_expr(&mut self, expr: &'ast Expr) {
            if let ExprKind::Path(p) = &expr.kind {
                self.seen.insert(p.segments[0].name);
            }
            walk_expr(self, expr);
        }
    }
    let mut c = Collector { seen: HashSet::new() };
    c.visit_block(body);
    c.seen
}

/// Emit `unused_closure_capture` for every entry in a closure literal's
/// `captures { ... }` list whose name does not appear as the leading
/// segment of a Path reference inside the closure body.
pub fn emit_unused_closure_capture_lints(
    package: &ResolvedPackage,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) {
    let severity = lint_cfg.effective(DiagnosticClass::UnusedClosureCapture);
    for module in &package.modules {
        let entry = package.graph.module(module.id);
        for item in &entry.ast.items {
            let ItemKind::Function(fn_decl) = &item.kind else {
                continue;
            };
            let FnBody::Block(body_block) = &fn_decl.body else {
                continue;
            };
            let mut state = ClosureCaptureLintState {
                diags,
                severity,
                interner: cx.interner,
            };
            state.visit_block(body_block);
        }
    }
}
