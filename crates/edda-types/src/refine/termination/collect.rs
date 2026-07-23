//! In-SCC recursive-call collection for §5 termination discharge.
//!
//! Walks a function body and records every `Call` site whose callee is a
//! Function binding in the same SCC as the caller, so the termination
//! pass can emit one strict-decrease obligation per recursive edge.

use edda_resolve::{BindingId, ResolvedPackage};
use edda_span::Span;
use edda_syntax::ast::visit::{self as ast_visit, Visitor};
use edda_syntax::ast::{Expr, ExprKind};

use crate::infer::SccMap;

use super::super::resolve_function_callee;

#[derive(Clone, Debug)]
pub(super) struct RecursiveCall {
    pub(super) callee: BindingId,
    pub(super) args: Vec<Expr>,
    pub(super) call_span: Span,
}

/// Collects every Call site in a function body whose callee is in
/// the same SCC as `caller`. Emits per-call records (callee binding,
/// cloned arg expressions, span) for the termination obligation
/// builder.
pub(super) struct RecursiveCallCollector<'a> {
    pub(super) package: &'a ResolvedPackage,
    pub(super) scc_map: &'a SccMap,
    pub(super) caller: BindingId,
    pub(super) out: Vec<RecursiveCall>,
}

impl<'a, 'ast> Visitor<'ast> for RecursiveCallCollector<'a> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                if let Some(callee_binding) = resolve_function_callee(callee, self.package)
                    && self.scc_map.same_scc(self.caller, callee_binding)
                {
                    self.out.push(RecursiveCall {
                        callee: callee_binding,
                        args: args.iter().map(|a| a.expr.clone()).collect(),
                        call_span: expr.span,
                    });
                }
                self.visit_expr(callee);
                for a in args {
                    self.visit_expr(&a.expr);
                }
            }
            ExprKind::Spawn(s) => {
                self.visit_block(&s.body);
            }
            _ => ast_visit::walk_expr(self, expr),
        }
    }
}
