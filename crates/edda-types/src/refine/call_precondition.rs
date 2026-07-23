//! Call-site `requires` precondition discharge.
//!
//! `super::clauses::discharge_fn_clauses` lifts a function's own
//! `requires` clauses once into a shared assumption context for that
//! same function's `ensures` obligations — it never checks a callee's
//! `requires` against the actual arguments a caller passes.
//! `edda_refine::ObligationKind::RequiresAtCall` exists for exactly
//! this purpose but was never constructed anywhere in this crate.
//!
//! This pass closes that gap. It walks a function body for every
//! `Call` site whose callee resolves to a `Function` binding declaring
//! one or more `requires` clauses, substitutes the callee's parameter
//! names with the call-site argument expressions (mirroring
//! [`super::termination`]'s in-SCC call walker — see
//! `super::substitute_paths`), and discharges each substituted clause
//! as a `RequiresAtCall` goal under the caller's own `requires_context`
//! (the same context `discharge_fn_clauses` already assembles for
//! `Ensures`).
//!
//! # Scope cut — same-package calls only
//!
//! `super::fn_decl_for` indexes a `BindingId` directly into
//! `ResolvedPackage`'s module vector with no bounds check. A
//! foreign-package `BindingId` reaching this pass would either panic
//! or silently mis-attribute a `requires` clause to the wrong
//! function. `resolve_function_callee` only ever returns bindings
//! resolved through the calling package's own `Resolutions` map, so
//! this cannot happen in practice today — the bounds check below is
//! defense in depth, not a load-bearing filter. Cross-package call-site
//! precondition discharge (calling into a rune dependency or `std.*`
//! function that declares `requires`) is a documented follow-up.
//!
//! # Path-sensitive narrowing — parked pending tree migration
//!
//! [`PathCollector`] can thread a path condition per call site: entering
//! an `if`'s `then` block asserts the condition, entering its `else`
//! asserts the negation, and every statement following a diverging
//! early-return guard (`if <cond> { return … }` with no `else`, whose
//! `then` block exits on every path — see [`early_return_guard`])
//! asserts `!<cond>` for the remainder of that block. That narrowing is
//! what makes the two idiomatic `requires`-bearing shapes provable
//! rather than skipped: a recursive call guarded by `else { … }` — safe
//! only because that branch is unreachable at the base case — and an
//! early-return guard clause (early returns are the documented
//! preferred control-flow style).
//!
//! The narrowing is currently **parked**: [`body_has_branching`] keeps
//! `If` in its disqualifying set, so a body containing any branch skips
//! this pass wholesale and [`PathCollector`] only ever sees
//! straight-line bodies (every recorded `path` is empty). Dropping `If`
//! from the set exposed 33 call sites across the
//! Edda tree whose counter-bound callee `requires`
//! (`file.next_node_id < u32::MAX` shapes) hold by a monotone-counter
//! invariant that no liftable call-site context can establish — true
//! contract gaps, but ones whose diagnostics block every T1 build.
//! Re-dropping `If` must land atomically with the
//! tree-side contract migration (post-state `ensures` on the
//! node/def-id allocators, or checked `+?` counters).
//!
//! When narrowing is active, a guard that cannot be lifted into the
//! predicate fragment is **not** silently dropped: discharging a
//! guarded site under a context weaker than what actually holds would
//! risk a false `refinement_unproven` on correct code, which is a worse
//! outcome than the false negative this pass closes. So
//! [`discharge_call_site`] skips such a site entirely — the same
//! conservative posture a branching body takes.
//!
//! # Caller-local sorts and call-result facts
//!
//! A substituted callee clause may reference a caller *local* (the
//! `scratch.len() >= 8` shape from
//! `var scratch: [u8] = allocator.alloc_array(u8, 32)?`). The lift
//! environment therefore carries a `local_sorts` map built by
//! [`collect_local_facts`] from the body's top-level simple `let`/`var`
//! bindings (mirroring [`super::termination`]'s `collect_caller_lets`),
//! so such a clause lifts instead of skipping on `UnresolvedPath`. Two
//! initialiser shapes additionally contribute **facts** to the per-site
//! obligation context:
//!
//! - `alloc_array(T, n)` (recognised through the
//!   `capability_method_calls` side table) contributes
//!   `local.len() == n` — the intrinsic's implicit postcondition.
//! - a direct same-package call whose callee declares `ensures`
//!   contributes each clause instantiated with params → args and
//!   `result` → the bound local (via
//!   [`FnRefineEnv::set_result_binding`]).
//!
//! Soundness is guarded on three axes. **Position**: a fact activates
//! only for call sites at a strictly later top-level statement, so a
//! postcondition of a call that has not yet executed (and may never —
//! a later `?` or trap exits first) can never discharge an earlier
//! site's obligation. **Frames (positional)**: a fact is
//! alive at a given call site iff no kill-write for a binding it
//! references lands in `(fact.active_after, site.stmt_index]` — same-
//! statement writes kill conservatively, since evaluation order within
//! a statement is not modeled. [`WriteScan`] records every write event
//! with its top-level statement index instead of collapsing them into a
//! body-wide set, so a write *after* a call site no longer kills that
//! site's fact (the `var buf = allocator.alloc_array(u8, 64)?; f(mutable
//! buf); return g(take buf)` shape — `f`'s call site sees `buf.len() ==
//! 64` even though `take buf` moves it two statements later). The
//! `alloc_array` length fact still survives a `mutable` pass — a `[T]`
//! view cannot be resized through a mutable element borrow
//! (`blake3(bytes, mutable scratch, 0)` must not kill `scratch.len() ==
//! 32`) — but does die on reassignment / `take` / `init` in range;
//! instantiated `ensures` facts additionally die on a `mutable` pass or
//! method-receiver use of a referenced binding in range. **Lift
//! failure**: a fact that cannot lift is simply not assumed — dropping
//! an assumption is always sound.
//!
//! The sorts alone would also *widen* the checked-obligation set: a
//! clause referencing a local used to skip on `UnresolvedPath`, and
//! with the local sorted it lifts — under a context that may know
//! nothing about the local's value (no derivable fact, or the frame
//! rules killed it). Reporting a failure there is the false-positive
//! shape this pass's zero-false-positive posture forbids (the
//! monotone-counter `seed.next_def_id < u32::MAX` family across
//! the Edda tree is exactly this). So [`discharge_call_site`] carries a
//! **fact gate**: a substituted clause referencing a sorted local that
//! is no active fact's subject at that site is skipped, exactly as the
//! unliftable clause was. A local whose facts are present but
//! insufficient still discharges — and fails honestly.
//!
//! # Scope cut — branching bodies skip
//!
//! `match`-arm pattern narrowing and the deferred-execution constructs
//! (`loop` / `for` / closure / `spawn` / `handle`) carry no path
//! sensitivity design yet, and `if`/`else` narrowing is parked per the
//! section above, so [`discharge_fn_call_preconditions`] refuses to run
//! on any body containing any of them — see [`body_has_branching`].
//! That leaves straight-line bodies (the previously-repro'd branching
//! shapes, and any simple delegation/wrapper function) fully covered,
//! with zero false-positive risk.

use std::collections::HashMap;
use std::time::Duration;

use ahash::AHashMap;
use smol_str::SmolStr;

use edda_diag::Diagnostics;
use edda_intern::Symbol;
use edda_refine::{
    CmpOp, DischargeFailure, DischargeRoute, Obligation, ObligationKind, Predicate, PredicateEnv,
    RefineError, Solver, Sort, UnverifiedAnnotation, Variable, Z3Backend, lift_clause,
    lift_predicate,
};
use edda_resolve::{BindingId, ResolvedPackage};
use edda_span::Span;
use edda_syntax::ast::visit::{self as ast_visit, Visitor};
use edda_syntax::ast::{
    self, CallArg, CallMode, Expr, ExprKind, FnDecl, PatKind, RefinementKind, Stmt, StmtKind,
};

use crate::attr::AttrSet;
use crate::cx::TyCx;
use crate::intrinsic::CapabilityMethod;
use crate::lower::LowerCx;
use crate::prim::Primitive;
use crate::sig::FnSig;

use super::sort::{FnRefineEnv, build_schema, field_refinement_facts, prim_to_sort, sort_of_ty};
use super::{fn_decl_for, resolve_function_callee, substitute_paths};

/// One branch/guard condition on the path to a call site. Lifted into
/// the per-site obligation context by [`discharge_call_site`], negated
/// when `negated`.
#[derive(Clone, Debug)]
struct GuardCond {
    cond: Expr,
    negated: bool,
}

#[derive(Clone, Debug)]
struct CallSite {
    callee: BindingId,
    args: Vec<Expr>,
    call_span: Span,
    path: Vec<GuardCond>,
    stmt_index: usize,
}

struct PathCollector<'a> {
    package: &'a ResolvedPackage,
    out: Vec<CallSite>,
    path: Vec<GuardCond>,
    block_depth: usize,
    stmt_index: usize,
}

impl<'a, 'ast> Visitor<'ast> for PathCollector<'a> {
    fn visit_block(&mut self, block: &'ast ast::Block) {
        self.block_depth += 1;
        let mark = self.path.len();
        for (i, stmt) in block.stmts.iter().enumerate() {
            if self.block_depth == 1 {
                self.stmt_index = i;
            }
            self.visit_stmt(stmt);
            // A diverging early-return guard `if <cond> { return … }`
            // makes every *following* statement in this block reachable
            // only when `<cond>` is false — assert `!<cond>` for the
            // remainder (popped at block exit via `mark`).
            if let Some(cond) = early_return_guard(stmt) {
                self.path.push(GuardCond {
                    cond: cond.clone(),
                    negated: true,
                });
            }
        }
        if let Some(trailing) = &block.trailing {
            if self.block_depth == 1 {
                self.stmt_index = block.stmts.len();
            }
            self.visit_expr(trailing);
        }
        self.path.truncate(mark);
        self.block_depth -= 1;
    }

    fn visit_expr(&mut self, expr: &'ast Expr) {
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                if let Some(callee_binding) = resolve_function_callee(callee, self.package) {
                    self.out.push(CallSite {
                        callee: callee_binding,
                        args: args.iter().map(|a| a.expr.clone()).collect(),
                        call_span: expr.span,
                        path: self.path.clone(),
                        stmt_index: self.stmt_index,
                    });
                }
                // Descend for calls nested in the callee/argument exprs.
                ast_visit::walk_expr(self, expr);
            }
            ExprKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                // The condition itself runs before the branch splits.
                self.visit_expr(cond);
                self.path.push(GuardCond {
                    cond: (**cond).clone(),
                    negated: false,
                });
                self.visit_block(then_block);
                self.path.pop();
                if let Some(else_expr) = else_branch {
                    self.path.push(GuardCond {
                        cond: (**cond).clone(),
                        negated: true,
                    });
                    self.visit_expr(else_expr);
                    self.path.pop();
                }
            }
            _ => ast_visit::walk_expr(self, expr),
        }
    }
}

/// If `stmt` is a diverging early-return guard, return its condition.
///
/// The guard shape is a bare `if <cond> { <block> }` statement (no
/// `else`) whose `then` block exits on every path — `return` / `raise` /
/// `panic` / `break` / `continue`. Every statement *after* it in the
/// same block runs only when `<cond>` is false, so [`PathCollector`]
/// asserts `!<cond>` into the path condition for the remainder.
fn early_return_guard(stmt: &Stmt) -> Option<&Expr> {
    let StmtKind::Expr(expr) = &stmt.kind else {
        return None;
    };
    let ExprKind::If {
        cond,
        then_block,
        else_branch: None,
    } = &expr.kind
    else {
        return None;
    };
    if block_diverges(then_block) {
        Some(&**cond)
    } else {
        None
    }
}

/// `true` if `block` exits on every path: an earlier statement diverges
/// (making the rest dead), or its value position is a divergent
/// expression.
fn block_diverges(block: &ast::Block) -> bool {
    for stmt in &block.stmts {
        if let StmtKind::Expr(e) = &stmt.kind
            && expr_diverges(e)
        {
            return true;
        }
    }
    block.trailing.as_deref().is_some_and(expr_diverges)
}

/// `true` if evaluating `expr` always transfers control away
/// (`return` / `raise` / `panic` / `break` / `continue`), or reduces to
/// a block / two-armed `if` whose every path does.
fn expr_diverges(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Return(_)
        | ExprKind::Raise(_)
        | ExprKind::Panic(_)
        | ExprKind::Break { .. }
        | ExprKind::Continue { .. } => true,
        ExprKind::Block(b) => block_diverges(b),
        ExprKind::If {
            then_block,
            else_branch: Some(else_expr),
            ..
        } => block_diverges(then_block) && expr_diverges(else_expr),
        _ => false,
    }
}

/// One caller-local fact derived from a top-level `let`/`var`
/// call-result binding: the `alloc_array` length
/// postcondition, or an instantiated same-package callee `ensures`.
struct LocalFact {
    subject: Symbol,
    active_after: usize,
    pred: Predicate,
    referenced_syms: Vec<Symbol>,
    hard_only: bool,
}

/// Where the body writes: reassignment targets, bindings passed with a
/// non-default call mode, and method-call receivers (whose mode is
/// declared at the method, invisible at the call site) — each recorded
/// against the top-level statement index it occurred in.
struct WriteScan<'a> {
    package: &'a ResolvedPackage,
    assigned: HashMap<Symbol, Vec<usize>>,
    hard_moded: HashMap<Symbol, Vec<usize>>,
    mutably_passed: HashMap<Symbol, Vec<usize>>,
    receivers: HashMap<Symbol, Vec<usize>>,
    unknown_write: bool,
    block_depth: usize,
    stmt_index: usize,
}

impl<'a> WriteScan<'a> {
    fn new(package: &'a ResolvedPackage) -> Self {
        Self {
            package,
            assigned: HashMap::new(),
            hard_moded: HashMap::new(),
            mutably_passed: HashMap::new(),
            receivers: HashMap::new(),
            unknown_write: false,
            block_depth: 0,
            stmt_index: 0,
        }
    }

    fn record_moded(&mut self, mode: CallMode, value: &Expr) {
        match place_root(value) {
            Some(sym) => match mode {
                CallMode::Mutable => {
                    self.mutably_passed.entry(sym).or_default().push(self.stmt_index);
                }
                CallMode::Take | CallMode::Init => {
                    self.hard_moded.entry(sym).or_default().push(self.stmt_index);
                }
            },
            None => self.unknown_write = true,
        }
    }

    fn record_arg(&mut self, arg: &CallArg) {
        if let Some(mode) = arg.mode {
            self.record_moded(mode, &arg.expr);
        }
    }

    fn record_bound_head_receiver(&mut self, callee: &Expr) {
        let ExprKind::Path(p) = &callee.kind else {
            return;
        };
        if p.segments.len() < 2 {
            return;
        }
        let Some(edda_resolve::Resolved::Binding(id)) =
            self.package.resolutions().lookup_path(p.span)
        else {
            return;
        };
        if matches!(
            self.package.binding(id).kind,
            edda_resolve::BindingKind::Param | edda_resolve::BindingKind::Local
        ) && let Some(seg) = p.segments.first()
        {
            self.receivers.entry(seg.name).or_default().push(self.stmt_index);
        }
    }

    fn indices_hit(indices: &[usize], after: usize, upto_inclusive: usize) -> bool {
        indices.iter().any(|&i| i > after && i <= upto_inclusive)
    }

    /// `true` if `sym` was reassigned or passed `take`/`init` in
    /// `(after, upto_inclusive]` — the kill set an `alloc_array` length
    /// fact respects (a `mutable` pass alone does not kill it).
    fn hard_write_in_range(&self, sym: Symbol, after: usize, upto_inclusive: usize) -> bool {
        self.unknown_write
            || self
                .assigned
                .get(&sym)
                .is_some_and(|v| Self::indices_hit(v, after, upto_inclusive))
            || self
                .hard_moded
                .get(&sym)
                .is_some_and(|v| Self::indices_hit(v, after, upto_inclusive))
    }

    /// `true` if `sym` was reassigned, passed with any non-default mode,
    /// or used as a method-call receiver in `(after, upto_inclusive]` —
    /// the full kill set an instantiated `ensures` fact respects.
    fn any_write_in_range(&self, sym: Symbol, after: usize, upto_inclusive: usize) -> bool {
        self.hard_write_in_range(sym, after, upto_inclusive)
            || self
                .mutably_passed
                .get(&sym)
                .is_some_and(|v| Self::indices_hit(v, after, upto_inclusive))
            || self
                .receivers
                .get(&sym)
                .is_some_and(|v| Self::indices_hit(v, after, upto_inclusive))
    }
}

impl<'a, 'ast> Visitor<'ast> for WriteScan<'a> {
    fn visit_block(&mut self, block: &'ast ast::Block) {
        self.block_depth += 1;
        for (i, stmt) in block.stmts.iter().enumerate() {
            if self.block_depth == 1 {
                self.stmt_index = i;
            }
            self.visit_stmt(stmt);
        }
        if let Some(trailing) = &block.trailing {
            if self.block_depth == 1 {
                self.stmt_index = block.stmts.len();
            }
            self.visit_expr(trailing);
        }
        self.block_depth -= 1;
    }

    fn visit_stmt(&mut self, stmt: &'ast Stmt) {
        if let StmtKind::Assign { target, .. } = &stmt.kind {
            match place_root(target) {
                Some(sym) => {
                    self.assigned.entry(sym).or_default().push(self.stmt_index);
                }
                None => self.unknown_write = true,
            }
        }
        ast_visit::walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, expr: &'ast Expr) {
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                self.record_bound_head_receiver(callee);
                for arg in args {
                    self.record_arg(arg);
                }
            }
            ExprKind::MethodCall { receiver, args, .. } => {
                if let Some(sym) = place_root(receiver) {
                    self.receivers.entry(sym).or_default().push(self.stmt_index);
                }
                for arg in args {
                    self.record_arg(arg);
                }
            }
            ExprKind::StructLit { fields, .. } => {
                for field in fields {
                    if let Some(mode) = field.mode {
                        self.record_moded(mode, &field.value);
                    }
                }
            }
            _ => {}
        }
        ast_visit::walk_expr(self, expr);
    }
}

/// `true` if `fact` is both position-gated (bound at a strictly earlier
/// top-level statement) and unkilled (no referenced binding was written
/// in `(fact.active_after, site_stmt_index]`) at the given call site.
fn fact_alive_at_site(fact: &LocalFact, writes: &WriteScan, site_stmt_index: usize) -> bool {
    if fact.active_after >= site_stmt_index {
        return false;
    }
    !fact.referenced_syms.iter().any(|&sym| {
        if fact.hard_only {
            writes.hard_write_in_range(sym, fact.active_after, site_stmt_index)
        } else {
            writes.any_write_in_range(sym, fact.active_after, site_stmt_index)
        }
    })
}

/// Root binding of a place expression: strip `Index` / `Field` /
/// `TupleIndex` / `CompField` projections down to the underlying path.
fn place_root(expr: &Expr) -> Option<Symbol> {
    match &expr.kind {
        ExprKind::Path(path) => path.segments.first().map(|seg| seg.name),
        ExprKind::Index { receiver, .. }
        | ExprKind::Field { receiver, .. }
        | ExprKind::TupleIndex { receiver, .. }
        | ExprKind::CompField { receiver, .. } => place_root(receiver),
        _ => None,
    }
}

/// Strip `?` layers off an initialiser: `f(a)?` binds `f`'s success
/// value, and a failure exits the function before any later statement
/// runs, so the callee's postcondition holds wherever the fact is
/// admitted (activation is position-gated to later statements).
fn strip_try(expr: &Expr) -> &Expr {
    match &expr.kind {
        ExprKind::Try(inner) => strip_try(inner),
        _ => expr,
    }
}

/// A recognised `let`/`var` initialiser shape (after [`strip_try`]).
enum InitShape<'a> {
    /// `<allocator>.alloc_array(T, n)` per the capability-method side table.
    AllocArray { elem: &'a Expr, count: &'a Expr },
    /// Direct call to an in-package function binding.
    KnownCall { callee: BindingId, args: Vec<Expr> },
    Other,
}

/// Classify an initialiser. `alloc_array` is recognised by span lookup
/// in the `capability_method_calls` side table — the inference pass
/// records the method dispatch keyed by the call expression's span,
/// which AST→HIR lowering preserves.
fn recognised_init<'a>(
    init: &'a Expr,
    package: &ResolvedPackage,
    capability_method_calls: &AHashMap<Span, CapabilityMethod>,
) -> InitShape<'a> {
    let is_alloc_array = matches!(
        capability_method_calls.get(&init.span),
        Some(CapabilityMethod::AllocatorAllocArray)
    );
    match &init.kind {
        ExprKind::MethodCall { args, .. } if is_alloc_array && args.len() == 2 => {
            InitShape::AllocArray {
                elem: &args[0].expr,
                count: &args[1].expr,
            }
        }
        ExprKind::Call { callee, args } => {
            if is_alloc_array && args.len() == 2 {
                return InitShape::AllocArray {
                    elem: &args[0].expr,
                    count: &args[1].expr,
                };
            }
            match resolve_function_callee(callee, package) {
                Some(binding) => InitShape::KnownCall {
                    callee: binding,
                    args: args.iter().map(|a| a.expr.clone()).collect(),
                },
                None => InitShape::Other,
            }
        }
        _ => InitShape::Other,
    }
}

/// Sort of `alloc_array`'s element type argument — a bare primitive
/// type name (`u8`, `usize`, …). Nominal element types yield `None`:
/// the binding then gets no sort and clauses referencing it keep
/// skipping conservatively.
fn alloc_array_elem_sort(elem: &Expr, lower_cx: &LowerCx<'_>) -> Option<Sort> {
    let ExprKind::Path(path) = &elem.kind else {
        return None;
    };
    if path.segments.len() != 1 {
        return None;
    }
    let name = lower_cx.interner.try_resolve(path.segments[0].name)?;
    prim_to_sort(Primitive::from_name(name)?)
}

/// Collects the head symbol of every path expression in a subtree —
/// the bindings a substituted clause or instantiated fact references.
/// Module/type heads land here too; consumers ignore symbols that are
/// not sorted locals or scanned writes.
struct PathSyms {
    out: Vec<Symbol>,
}

impl<'ast> Visitor<'ast> for PathSyms {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        if let ExprKind::Path(path) = &expr.kind
            && let Some(head) = path.segments.first()
        {
            self.out.push(head.name);
        }
        ast_visit::walk_expr(self, expr);
    }
}

/// Walk the body's top-level `let`/`var` bindings in order, populating
/// `local_sorts` (so substituted callee clauses referencing a caller
/// local lift) and returning every [`LocalFact`] the per-site obligation
/// context may assume once [`fact_alive_at_site`] confirms it is both
/// position-gated and unkilled at that site. See the module doc's
/// "Caller-local sorts and call-result facts" section.
#[allow(clippy::too_many_arguments)]
fn collect_local_facts(
    body: &ast::Block,
    param_sorts: &HashMap<Symbol, (SmolStr, Sort)>,
    local_sorts: &mut HashMap<Symbol, (SmolStr, Sort)>,
    package: &ResolvedPackage,
    ty_cx: &TyCx,
    lower_cx: &LowerCx<'_>,
    capability_method_calls: &AHashMap<Span, CapabilityMethod>,
) -> Vec<LocalFact> {
    let result_sym = lower_cx.interner.intern("result");
    let mut facts: Vec<LocalFact> = Vec::new();

    for (stmt_index, stmt) in body.stmts.iter().enumerate() {
        let StmtKind::Let {
            pat,
            init: Some(init),
            ..
        } = &stmt.kind
        else {
            continue;
        };
        let PatKind::Binding(ident) = &pat.kind else {
            continue;
        };
        let name_sym = ident.name;
        if local_sorts.contains_key(&name_sym) || param_sorts.contains_key(&name_sym) {
            // Re-bound or param-shadowing name — rejected upstream by the
            // no-shadowing rule; defensively contribute nothing.
            continue;
        }
        let Some(name_text) = lower_cx.interner.try_resolve(name_sym) else {
            continue;
        };
        let name = SmolStr::new(name_text);

        match recognised_init(strip_try(init), package, capability_method_calls) {
            InitShape::AllocArray { elem, count } => {
                let Some(elem_sort) = alloc_array_elem_sort(elem, lower_cx) else {
                    continue;
                };
                let sort = Sort::slice(elem_sort);
                // The length fact survives `mutable` passing and method-
                // receiver use (a `[T]` view cannot be resized) but dies
                // on reassignment / `take` / `init` — enforced positionally
                // by `fact_alive_at_site` (`hard_only: true`), not here.
                let env = FnRefineEnv::new(
                    package,
                    param_sorts,
                    local_sorts,
                    lower_cx.interner,
                    lower_cx.ty_interner,
                    ty_cx,
                );
                if let Ok(count_pred) = lift_predicate(count, &env) {
                    facts.push(LocalFact {
                        subject: name_sym,
                        active_after: stmt_index,
                        pred: Predicate::Cmp {
                            op: CmpOp::Eq,
                            lhs: Box::new(Predicate::SliceLen {
                                slice: Box::new(Predicate::Var(Variable::new(
                                    name.clone(),
                                    sort.clone(),
                                ))),
                            }),
                            rhs: Box::new(count_pred),
                        },
                        referenced_syms: vec![name_sym],
                        hard_only: true,
                    });
                }
                local_sorts.insert(name_sym, (name, sort));
            }
            InitShape::KnownCall { callee, args } => {
                // Defense in depth — same bounds check as `discharge_call_site`.
                if callee.module.as_usize() >= package.modules().len() {
                    continue;
                }
                let Some(sort) = ty_cx
                    .sig(callee)
                    .and_then(|s| sort_of_ty(s.return_ty, lower_cx.ty_interner, ty_cx))
                else {
                    continue;
                };
                if let Some(callee_decl) = fn_decl_for(callee, package) {
                    let mut subst: HashMap<Symbol, Expr> = HashMap::new();
                    for (pdecl, arg) in callee_decl.params.iter().zip(args.iter()) {
                        subst.insert(pdecl.name.name, arg.clone());
                    }
                    for clause in callee_decl
                        .refinements
                        .iter()
                        .filter(|c| c.kind == RefinementKind::Ensures)
                    {
                        let instantiated = substitute_paths(&clause.pred, &subst, package);
                        let env = FnRefineEnv::new(
                            package,
                            param_sorts,
                            local_sorts,
                            lower_cx.interner,
                            lower_cx.ty_interner,
                            ty_cx,
                        );
                        env.set_result_binding(Some((result_sym, name.clone(), sort.clone())));
                        let lifted = lift_predicate(&instantiated, &env);
                        env.set_result_binding(None);
                        if let Ok(pred) = lifted {
                            // `referenced_syms` covers the bound local
                            // (the `result`-substituted subject, which
                            // the path walk below cannot see — it
                            // appears as `result` in the clause) plus
                            // every path-head symbol the instantiated
                            // predicate references, so a write to
                            // either kills this fact at `fact_alive_at_site`.
                            let mut syms = PathSyms { out: vec![name_sym] };
                            syms.visit_expr(&instantiated);
                            facts.push(LocalFact {
                                subject: name_sym,
                                active_after: stmt_index,
                                pred,
                                referenced_syms: syms.out,
                                hard_only: false,
                            });
                        }
                    }
                }
                local_sorts.insert(name_sym, (name, sort));
            }
            InitShape::Other => {
                // Structural sort only (mirrors `collect_caller_lets`'
                // fallback) — no facts.
                let sort = {
                    let env = FnRefineEnv::new(
                        package,
                        param_sorts,
                        local_sorts,
                        lower_cx.interner,
                        lower_cx.ty_interner,
                        ty_cx,
                    );
                    env.expr_sort(strip_try(init))
                };
                if let Some(sort) = sort {
                    local_sorts.insert(name_sym, (name, sort));
                }
            }
        }
    }
    facts
}

/// `true` if `block` contains an `If` / `Match` / `Loop` / `For` /
/// `Closure` / `Spawn` / `Handle` construct anywhere, at any nesting
/// depth. `If` is back in the set — [`PathCollector`]'s `if`/`else`
/// path narrowing is parked until the tree-side
/// counter-contract migration lands atomically with its re-enable.
/// See the module doc's "parked" section.
fn body_has_branching(block: &ast::Block) -> bool {
    struct BranchDetector {
        found: bool,
    }
    impl<'ast> Visitor<'ast> for BranchDetector {
        fn visit_expr(&mut self, expr: &'ast Expr) {
            if self.found {
                return;
            }
            match &expr.kind {
                ExprKind::If { .. }
                | ExprKind::Match { .. }
                | ExprKind::Loop { .. }
                | ExprKind::For { .. }
                | ExprKind::Closure(_)
                | ExprKind::Spawn(_)
                | ExprKind::Handle { .. } => {
                    self.found = true;
                }
                _ => ast_visit::walk_expr(self, expr),
            }
        }
    }
    let mut detector = BranchDetector { found: false };
    detector.visit_block(block);
    detector.found
}

/// Discharge every `requires` clause a call site owes its callee,
/// per `corpus/edda-codex/language/02-modes-effects-refinements.md`
/// §refinements. For each `Call` in `fn_decl`'s body whose callee
/// declares `requires` clauses, substitutes the callee's parameters
/// with the call's actual argument expressions and discharges the
/// result under [`ObligationKind::RequiresAtCall`].
///
/// Returns immediately when `lower_cx.package` is `None` (unit-test
/// lowering scenarios that pre-date resolution), when the function has
/// no body block (`extern`-bodied), or when the body contains no call
/// sites at all.
pub(crate) fn discharge_fn_call_preconditions(
    fn_decl: &FnDecl,
    sig: &FnSig,
    attrs: &AttrSet,
    ty_cx: &TyCx,
    lower_cx: &LowerCx<'_>,
    capability_method_calls: &AHashMap<Span, CapabilityMethod>,
    diags: &mut Diagnostics,
) {
    let Some(package) = lower_cx.package else {
        return;
    };
    let body_block = match &fn_decl.body {
        ast::FnBody::Block(b) => b,
        ast::FnBody::Extern { .. } => return,
    };
    if body_has_branching(body_block) {
        return;
    }

    let mut collector = PathCollector {
        package,
        out: Vec::new(),
        path: Vec::new(),
        block_depth: 0,
        stmt_index: 0,
    };
    collector.visit_block(body_block);
    if collector.out.is_empty() {
        return;
    }

    // Per-function param-sort map (same shape as `discharge_fn_clauses`)
    // so the lifter knows the sort of every caller-side path a
    // substituted callee clause can still reference.
    let mut param_sorts: HashMap<Symbol, (SmolStr, Sort)> = HashMap::new();
    for (param_decl, param_sig) in fn_decl.params.iter().zip(sig.params.iter()) {
        let Some(name_text) = lower_cx.interner.try_resolve(param_decl.name.name) else {
            continue;
        };
        if let Some(sort) = sort_of_ty(param_sig.ty, lower_cx.ty_interner, ty_cx) {
            let name = SmolStr::new(name_text);
            param_sorts.insert(param_decl.name.name, (name, sort));
        }
    }

    let unverified_route = attrs.unverified.and_then(|ann| {
        let reason = lower_cx.interner.resolve(ann.reason).to_string();
        UnverifiedAnnotation::new(reason, ann.attr_span)
            .ok()
            .map(DischargeRoute::Unverified)
    });

    // Caller-local sorts + position-gated call-result facts —
    // see the module doc. `writes` backs the per-site
    // positional liveness check `fact_alive_at_site` runs below —
    // collection no longer gates on it.
    let mut local_sorts: HashMap<Symbol, (SmolStr, Sort)> = HashMap::new();
    let local_facts = collect_local_facts(
        body_block,
        &param_sorts,
        &mut local_sorts,
        package,
        ty_cx,
        lower_cx,
        capability_method_calls,
    );
    let mut writes = WriteScan::new(package);
    writes.visit_block(body_block);
    let env = FnRefineEnv::new(
        package,
        &param_sorts,
        &local_sorts,
        lower_cx.interner,
        lower_cx.ty_interner,
        ty_cx,
    );

    // The caller's own `requires` hold throughout its body, at every
    // call site inside it — the identical context `discharge_fn_clauses`
    // assembles for its `Ensures` obligations.
    let mut requires_context: Vec<Predicate> = fn_decl
        .refinements
        .iter()
        .filter(|c| c.kind == RefinementKind::Requires)
        .filter_map(|c| lift_clause(c, &env).ok())
        .collect();
    // Per-parameter inline refinements hold at function entry exactly like
    // top-level `requires` clauses — a
    // delegation wrapper like `sp(file, start, end: usize where start <=
    // end)` forwarding to `span_of(...) requires start <= end` is provable
    // only from the inline bound.
    requires_context.extend(
        fn_decl
            .params
            .iter()
            .filter_map(|p| super::clauses::param_refinement_pred(&p.ty))
            .filter_map(|pred| lift_predicate(pred, &env).ok()),
    );
    // A record field's own inline `where` refinement holds unconditionally
    // at every field-projection read site; fold these facts
    // from the caller's own clauses too so they're available at every call
    // site below. `env` is the caller's own frame here — same-frame use,
    // safe per `field_refinement_facts`' frame invariant.
    for clause in &fn_decl.refinements {
        requires_context.extend(field_refinement_facts(&clause.pred, &env));
    }

    let schema = std::sync::Arc::new(build_schema(ty_cx, lower_cx.ty_interner, lower_cx.interner));
    let solver = Z3Backend::with_schema(schema);
    for site in collector.out {
        discharge_call_site(
            &site,
            package,
            &env,
            &requires_context,
            &local_sorts,
            &local_facts,
            &writes,
            &unverified_route,
            &solver,
            lower_cx,
            diags,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn discharge_call_site(
    site: &CallSite,
    package: &ResolvedPackage,
    env: &FnRefineEnv<'_>,
    requires_context: &[Predicate],
    local_sorts: &HashMap<Symbol, (SmolStr, Sort)>,
    local_facts: &[LocalFact],
    writes: &WriteScan,
    unverified_route: &Option<DischargeRoute>,
    solver: &Z3Backend,
    lower_cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
) {
    // Defense in depth — see the module doc's "Scope cut" section.
    if site.callee.module.as_usize() >= package.modules().len() {
        return;
    }
    let Some(callee_fn_decl) = fn_decl_for(site.callee, package) else {
        return;
    };
    let requires_clauses: Vec<&ast::RefinementClause> = callee_fn_decl
        .refinements
        .iter()
        .filter(|c| c.kind == RefinementKind::Requires)
        .collect();
    if requires_clauses.is_empty() {
        return;
    }
    let Some(callee_name_text) = lower_cx.interner.try_resolve(callee_fn_decl.name.name) else {
        return;
    };
    let callee_name = SmolStr::new(callee_name_text);

    // Narrow the obligation context to this specific call site: the
    // caller's own `requires` (which hold everywhere in the body) plus
    // every branch condition / diverging-guard negation on the path to
    // this call. A guard that cannot be lifted into the
    // predicate fragment is *not* droppable — discharging without it
    // would risk a false `refinement_unproven` on correct guarded code —
    // so the whole site is skipped conservatively, exactly as a
    // branching body was skipped before path sensitivity landed.
    let mut site_context = requires_context.to_vec();
    for guard in &site.path {
        let Ok(lifted) = lift_predicate(&guard.cond, env) else {
            return;
        };
        site_context.push(if guard.negated {
            Predicate::Not(Box::new(lifted))
        } else {
            lifted
        });
    }
    // Call-result facts from bindings at strictly earlier top-level
    // statements, unkilled between their binding and this site — see
    // `fact_alive_at_site`.
    for fact in local_facts {
        if fact_alive_at_site(fact, writes, site.stmt_index) {
            site_context.push(fact.pred.clone());
        }
    }

    let mut subst: HashMap<Symbol, Expr> = HashMap::new();
    for (param_decl, arg) in callee_fn_decl.params.iter().zip(site.args.iter()) {
        subst.insert(param_decl.name.name, arg.clone());
    }

    for (clause_index, clause) in requires_clauses.iter().enumerate() {
        let substituted = substitute_paths(&clause.pred, &subst, package);
        // Fact gate: a substituted clause that references
        // a sorted caller local with NO active (position-gated AND
        // unkilled) fact at this site would discharge
        // under a known-incomplete context — the pass knows nothing
        // about that local's value, so a failure would be the exact
        // false-positive shape the unliftable-guard skip exists to
        // prevent. Skip the clause; a local whose facts are merely
        // insufficient still discharges (and honestly fails).
        let mut refs = PathSyms { out: Vec::new() };
        refs.visit_expr(&substituted);
        let missing_fact = refs.out.iter().any(|sym| {
            local_sorts.contains_key(sym)
                && !local_facts
                    .iter()
                    .any(|f| f.subject == *sym && fact_alive_at_site(f, writes, site.stmt_index))
        });
        if missing_fact {
            continue;
        }
        let predicate = match lift_predicate(&substituted, env) {
            Ok(p) => p,
            Err(_) => continue,
        };
        // The substituted clause may project into an argument's own
        // refined fields (`arg.field`, e.g. `use_nanos(d)`'s `requires
        // d.nanos >= 0` substituting the caller's `d`) — fold those
        // facts in alongside `site_context`.
        let field_facts = field_refinement_facts(&substituted, env);
        // Parking gate: closing the
        // field-projection hazard in `substitute_paths`
        // makes every such clause's GOAL newly liftable in the caller's
        // frame — including ones over UNREFINED fields, where the
        // resulting obligation is often a pre-existing contract gap with
        // no available proof (confirmed against real Edda-tree sources:
        // `LowerCtx.next_region_id` / `alloc_region`'s monotone-counter
        // family already documented above as the branching-body carve-out, and
        // `Rational.num != 0` in `recip`/`div`, whose fix cascades three
        // call-site hops deep into `find_entering`/`check` — out of scope
        // here). `body_has_branching` cannot park these: both real callers
        // are 100% straight-line, so they were never excluded by that
        // carve-out — the field-projection hazard was simply masking them
        // as `UnresolvedPath` skips until this fix. Preserve that
        // conservative skip specifically when (a) no refined-field fact
        // backs this goal AND (b) the callee's OWN unsubstituted clause
        // does not already lift against this caller's frame on its own
        // merits (a genuine same-name binding, not a substitution
        // artifact) — this is exactly the set of obligations the
        // prior code silently skipped as `UnresolvedPath`. When the
        // callee's own clause DOES already lift in the caller's frame,
        // that is the collision hazard (a same-named-but-
        // possibly-unrelated caller binding) `substitute_paths`'s fix
        // exists to correct — always use the substituted, correct
        // semantics there, never the coincidental old lift.
        if field_facts.is_empty() && lift_predicate(&clause.pred, env).is_err() {
            continue;
        }
        let mut obligation_context = site_context.clone();
        obligation_context.extend(field_facts);
        let mut obligation = Obligation::new(
            predicate,
            obligation_context,
            site.call_span,
            ObligationKind::RequiresAtCall {
                callee: callee_name.clone(),
                clause_index: clause_index as u32,
            },
            "",
        );
        if let Some(r) = unverified_route {
            obligation = obligation.with_route(r.clone());
        }
        let outcome = solver.discharge(&obligation, Duration::from_secs(5));
        if let Some(failure) = DischargeFailure::from_outcome(outcome) {
            let err = RefineError::discharge(
                site.call_span,
                obligation.kind().clone(),
                obligation.predicate_text(),
                failure,
            );
            diags.push(err.to_diagnostic());
        }
    }
}
