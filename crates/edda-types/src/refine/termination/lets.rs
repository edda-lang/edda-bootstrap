//! Caller-`let` scanning and call-result postcondition instantiation for
//! §5 termination discharge.
//!
//! A `decreases` measure may reference a `let`-bound local (the natural
//! `decreases hi - lo` across `let p = partition(arr, lo, hi)`). This
//! module derives the local sorts the lifter needs and instantiates the
//! callee's `ensures` clauses into the strict-decrease obligation context.

use std::collections::{HashMap, HashSet};

use smol_str::SmolStr;

use edda_intern::Symbol;
use edda_refine::{Predicate, PredicateEnv, Sort, lift_predicate};
use edda_resolve::{BindingId, ResolvedPackage};
use edda_syntax::ast::visit::{self as ast_visit, Visitor};
use edda_syntax::ast::{self, Expr, ExprKind, PatKind, RefinementKind, Stmt, StmtKind};

use crate::cx::TyCx;
use crate::lower::LowerCx;

use super::super::sort::{FnRefineEnv, sort_of_ty};
use super::super::{fn_decl_for, resolve_function_callee, substitute_paths};

/// Scan a function body's `let` statements to build the two maps the
/// termination measure lift needs: `local_sorts` (every simple
/// identifier-bound local → its [`Sort`]) so a measure that mentions a
/// local lifts instead of failing `UnresolvedPath`, and `let_calls` (the
/// subset bound directly to a known function call → callee binding + cloned
/// arg exprs) so the callee's `ensures` can be instantiated into the
/// obligation context. Locals are processed in source order; a re-bound
/// name is retracted from both maps.
pub(super) fn collect_caller_lets(
    fn_decl: &ast::FnDecl,
    param_sorts: &HashMap<Symbol, (SmolStr, Sort)>,
    ty_cx: &TyCx,
    lower_cx: &LowerCx<'_>,
    package: &ResolvedPackage,
) -> (
    HashMap<Symbol, (SmolStr, Sort)>,
    HashMap<Symbol, (BindingId, Vec<Expr>)>,
) {
    let mut local_sorts: HashMap<Symbol, (SmolStr, Sort)> = HashMap::new();
    let mut let_calls: HashMap<Symbol, (BindingId, Vec<Expr>)> = HashMap::new();
    let ast::FnBody::Block(body) = &fn_decl.body else {
        return (local_sorts, let_calls);
    };
    let mut collector = LetCollector { out: Vec::new() };
    collector.visit_block(body);

    let mut ambiguous: HashSet<Symbol> = HashSet::new();
    for (name_sym, init) in collector.out {
        if ambiguous.contains(&name_sym) {
            continue;
        }
        if local_sorts.contains_key(&name_sym) || let_calls.contains_key(&name_sym) {
            // Second definition of this name → ambiguous: retract both.
            local_sorts.remove(&name_sym);
            let_calls.remove(&name_sym);
            ambiguous.insert(name_sym);
            continue;
        }
        let Some(name_text) = lower_cx.interner.try_resolve(name_sym) else {
            continue;
        };
        let name = SmolStr::new(name_text);
        let call = as_known_call(init, package);
        // Sort: a call's declared return type (so the local lifts to a free
        // var of the right sort), else a structural sort of the initialiser
        // over the params + earlier locals.
        let sort = match &call {
            Some((callee, _)) => ty_cx
                .sig(*callee)
                .and_then(|s| sort_of_ty(s.return_ty, lower_cx.ty_interner, ty_cx)),
            None => {
                let env = FnRefineEnv::new(
                    package,
                    param_sorts,
                    &local_sorts,
                    lower_cx.interner,
                    lower_cx.ty_interner,
                    ty_cx,
                );
                env.expr_sort(init)
            }
        };
        if let Some(sort) = sort {
            local_sorts.insert(name_sym, (name.clone(), sort));
        }
        if let Some((callee, args)) = call {
            let_calls.insert(name_sym, (callee, args));
        }
    }
    (local_sorts, let_calls)
}

/// Recognise a `let`-initialiser that is a direct call to an in-package
/// function, returning the callee binding and the cloned positional
/// argument expressions. Wrapped, method-form, and non-call initialisers
/// return `None`.
fn as_known_call(init: &Expr, package: &ResolvedPackage) -> Option<(BindingId, Vec<Expr>)> {
    let ExprKind::Call { callee, args } = &init.kind else {
        return None;
    };
    let binding = resolve_function_callee(callee, package)?;
    Some((binding, args.iter().map(|a| a.expr.clone()).collect()))
}

/// For every call-result local referenced by `measure`, instantiate the
/// callee's `ensures` clauses (callee params → the call's args, `result` →
/// the local via the env's `result_binding`) and push each lifted predicate
/// onto `out` (the obligation context). Clauses outside the predicate
/// fragment are skipped.
pub(super) fn ensure_facts_for_measure(
    measure: &Expr,
    let_calls: &HashMap<Symbol, (BindingId, Vec<Expr>)>,
    local_sorts: &HashMap<Symbol, (SmolStr, Sort)>,
    result_sym: Symbol,
    package: &ResolvedPackage,
    env: &FnRefineEnv<'_>,
    out: &mut Vec<Predicate>,
) {
    let mut names = PathSymCollector { out: Vec::new() };
    names.visit_expr(measure);
    let mut seen: HashSet<Symbol> = HashSet::new();
    for name_sym in names.out {
        if !seen.insert(name_sym) {
            continue;
        }
        let Some((callee, call_args)) = let_calls.get(&name_sym) else {
            continue;
        };
        let Some((local_name, local_sort)) = local_sorts.get(&name_sym) else {
            continue;
        };
        let Some(callee_decl) = fn_decl_for(*callee, package) else {
            continue;
        };
        // callee params → the call's positional args (capture-avoiding via
        // `substitute_paths`); `result` is left in place for the env's
        // `result_binding` to resolve to the local.
        let mut subst: HashMap<Symbol, Expr> = HashMap::new();
        for (pdecl, arg) in callee_decl.params.iter().zip(call_args.iter()) {
            subst.insert(pdecl.name.name, arg.clone());
        }
        for clause in callee_decl
            .refinements
            .iter()
            .filter(|c| c.kind == RefinementKind::Ensures)
        {
            let instantiated = substitute_paths(&clause.pred, &subst, package);
            env.set_result_binding(Some((result_sym, local_name.clone(), local_sort.clone())));
            let lifted = lift_predicate(&instantiated, env);
            env.set_result_binding(None);
            if let Ok(pred) = lifted {
                out.push(pred);
            }
        }
    }
}

/// Collects `(bound-name-symbol, &initialiser)` for every simple
/// identifier `let` in a function body, in source order (nested blocks
/// included). Order is significant — `collect_caller_lets` treats a
/// repeated name as ambiguous.
struct LetCollector<'ast> {
    out: Vec<(Symbol, &'ast Expr)>,
}

impl<'ast> Visitor<'ast> for LetCollector<'ast> {
    fn visit_stmt(&mut self, stmt: &'ast Stmt) {
        if let StmtKind::Let {
            pat, init: Some(init), ..
        } = &stmt.kind
            && let PatKind::Binding(ident) = &pat.kind
        {
            self.out.push((ident.name, init));
        }
        ast_visit::walk_stmt(self, stmt);
    }
}

/// Collects the interned name of every single-segment path expression in a
/// subtree — used to find which call-result locals a substituted measure
/// references.
struct PathSymCollector {
    out: Vec<Symbol>,
}

impl<'ast> Visitor<'ast> for PathSymCollector {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        if let ExprKind::Path(path) = &expr.kind
            && path.segments.len() == 1
        {
            self.out.push(path.segments[0].name);
        }
        ast_visit::walk_expr(self, expr);
    }
}
