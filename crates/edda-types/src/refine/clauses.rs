//! `requires` / `ensures` clause discharge.
//!
//! Lifts every `requires` / `ensures` clause on a function declaration
//! into the [`edda_refine::Predicate`] IR and routes each obligation
//! through [`edda_refine::Z3Backend`]. Emits a
//! [`edda_diag::DiagnosticClass::RefinementUnproven`] diagnostic for
//! each clause that is not provable.
//!
//! `Requires` clauses are preconditions the caller guarantees — they
//! are lifted once into a shared `requires_context` and threaded into
//! the obligation context of every other clause (currently `Ensures`;
//! future call-site `RequiresAtCall` obligations and body-induced
//! obligations reuse the same context). Per-call-site discharge of a
//! callee's `requires` is deferred to a follow-up wave.
//!
//! `where` clauses (type-level invariants) are silently skipped — they
//! guard struct-construction sites and are not discharged at function
//! definitions. `decreases` clauses are the
//! [`super::termination`] pass's responsibility.

use std::collections::HashMap;
use std::time::Duration;

use smol_str::SmolStr;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, Severity};
use edda_intern::Symbol;
use edda_refine::{
    CmpOp, DischargeFailure, DischargeRoute, LiftError, Obligation, ObligationKind, Predicate,
    RefineError, Solver, Sort, UnverifiedAnnotation, Variable, Z3Backend, lift_clause,
    lift_predicate,
};
use edda_syntax::ast::visit::{Visitor, walk_expr};
use edda_syntax::ast::{
    self, Expr, ExprKind, FnBody, FnDecl, Literal, RefinementClause, RefinementKind,
};

use crate::attr::AttrSet;
use crate::cx::TyCx;
use crate::lower::LowerCx;
use crate::sig::FnSig;

use super::sort::{FnRefineEnv, build_schema, field_refinement_facts, sort_of_ty};

//            `Return` — `single_tail_return_expr` relies on `count == 1`
//            meaning "the trailing return is the body's only return"
/// Counts every `return` expression reachable from a visited [`Block`]
/// (including nested inside `if`/`match`/loop bodies) — used to detect
/// multi-return-site bodies that [`single_tail_return_expr`] must reject.
struct ReturnCounter {
    count: usize,
}

impl<'ast> Visitor<'ast> for ReturnCounter {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        if matches!(expr.kind, ExprKind::Return(_)) {
            self.count += 1;
        }
        walk_expr(self, expr);
    }
}

//            statement in `block` runs unconditionally and exactly once
//            relative to the function's own entry, mirroring
//            `call_precondition::body_has_branching`'s soundness
//            argument: a param's range can only differ at the trailing
//            return from its value at entry if some earlier branch
//            narrowed it, so straight-line-only bodies are the ones where
//            "assert `result == <tail expr>` with no other context change"
//            is sound
/// `true` if `block` contains an `If` / `Match` / `Loop` / `For` /
/// `Closure` / `Spawn` / `Handle` construct anywhere, at any nesting
/// depth. Kept as a separate copy of
/// `call_precondition::body_has_branching` — the two callers sit in
/// unrelated discharge passes.
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
                _ => walk_expr(self, expr),
            }
        }
    }
    let mut detector = BranchDetector { found: false };
    detector.visit_block(block);
    detector.found
}

//            body ending in either a bare trailing expression or a
//            trailing `return <expr>` that is the body's only `return` —
//            e.g. `{ if b < 0x30 { raise ... } if b > 0x39 { raise ... }
//            return b - 0x30 }` (`lib/link/src/coff/read.ea`) is rejected
//            despite having only one `return`, because the two guard
//            `if`s narrow `b`'s range before the trailing return and that
//            narrowing isn't part of the equality fact asserted below —
//            without this check the fact `result == b - 0x30` would be
//            asserted with `b` left unconstrained, letting Z3 falsify a
//            correct postcondition (flips the bug from false-negative to
//            false-positive, same failure mode `body_has_branching` exists
//            to prevent in the call-site pass)
/// Return the function body's tail-value expression when the body matches
/// the single-tail-expression shape `discharge_fn_clauses` correlates with
/// the `ensures` `result` binding: no branching construct anywhere in the
/// body, and either a bare trailing expression (no `return` anywhere) or a
/// trailing `return <expr>` that is the body's only `return`. Any other
/// shape (branching present, no trailing expression, a `return` with no
/// value, or a `return` elsewhere in the body) yields `None`.
fn single_tail_return_expr(fn_decl: &FnDecl) -> Option<&Expr> {
    let FnBody::Block(block) = &fn_decl.body else {
        return None;
    };
    if body_has_branching(block) {
        return None;
    }
    let trailing = block.trailing.as_deref()?;
    let mut counter = ReturnCounter { count: 0 };
    counter.visit_block(block);
    match &trailing.kind {
        ExprKind::Return(Some(inner)) if counter.count == 1 => Some(inner),
        ExprKind::Return(_) => None,
        _ if counter.count == 0 => Some(trailing),
        _ => None,
    }
}

//            as a separate copy rather than shared because the two callers
//            sit in different discharge passes with otherwise-unrelated
//            context-building code
/// Return the inline `where`-clause predicate on a parameter's own type
/// (`byte: u8 where byte >= 48 && byte <= 57`), so it becomes a fact in the
/// same `requires_context` a top-level `requires` clause populates: a
/// caller can only pass a value satisfying the refinement, so it holds at
/// function entry exactly like a `requires` precondition. Recurses through
/// `[T where P]` slice element refinements; every other type shape yields
/// `None`. Shared with [`super::call_precondition`], whose
/// `requires_context` needs the identical fold.
pub(super) fn param_refinement_pred(ty: &ast::Type) -> Option<&Expr> {
    match &ty.kind {
        ast::TypeKind::Refined { pred, .. } => Some(pred),
        ast::TypeKind::Slice(inner) => param_refinement_pred(inner),
        _ => None,
    }
}

//            finalised and the resolver's `Resolutions` map is fully populated
/// Lift and discharge every `requires`/`ensures` clause on `fn_decl` through
/// the Z3 backend. Emits a [`edda_diag::DiagnosticClass::RefinementUnproven`]
/// diagnostic for each clause that is not provable.
///
/// `Requires` clauses are not goals at the function definition — they are
/// preconditions the caller guarantees. They are lifted once into a shared
/// `requires_context` and added to the discharge context of every other
/// obligation produced inside the body (currently the `Ensures` discharge;
/// future call-site `RequiresAtCall` and body-induced obligations will reuse
/// the same context). Per-call-site discharge of a callee's `requires` is
/// deferred to a follow-up wave.
///
/// `Where` clauses are silently skipped — they guard struct-construction sites
/// and are not discharged at function definitions.
///
/// Returns immediately when `lower_cx.package` is `None` (unit-test lowering
/// scenarios that pre-date resolution do not carry a resolved package).
pub(crate) fn discharge_fn_clauses(
    fn_decl: &FnDecl,
    sig: &FnSig,
    attrs: &AttrSet,
    ty_cx: &TyCx,
    lower_cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
) {
    if fn_decl.refinements.is_empty() {
        return;
    }

    // Lint C — `refinement_trivially_true`: any `requires` / `ensures` /
    // `where` clause whose predicate is the literal `true` carries zero
    // verification information. Runs as a syntactic check independent of
    // the SMT discharge layer so it fires even when no resolved package
    // is available.
    for clause in &fn_decl.refinements {
        emit_trivially_true_lint(clause, diags);
    }

    let Some(package) = lower_cx.package else { return; };

    let mut param_sorts: HashMap<Symbol, (SmolStr, Sort)> = HashMap::new();
    for (param_decl, param_sig) in fn_decl.params.iter().zip(sig.params.iter()) {
        // Parser-recovery DUMMY name → skip; a refinement keyed on an unnamed
        // parameter would produce a degenerate predicate.
        let Some(name_text) = lower_cx.interner.try_resolve(param_decl.name.name) else {
            continue;
        };
        if let Some(sort) = sort_of_ty(param_sig.ty, lower_cx.ty_interner, ty_cx) {
            let name = SmolStr::new(name_text);
            param_sorts.insert(param_decl.name.name, (name, sort));
        }
    }

    // Per refinement-decidability.md §9 a function-level `@unverified`
    // admits every obligation inside the function without SMT discharge.
    // Pre-build the route once so each `ensures` obligation below can
    // short-circuit through the certificate path.
    let unverified_route = attrs.unverified.and_then(|ann| {
        let reason = lower_cx.interner.resolve(ann.reason).to_string();
        UnverifiedAnnotation::new(reason, ann.attr_span)
            .ok()
            .map(DischargeRoute::Unverified)
    });

    // A non-empty schema is required for any `.field` access
    // in a `requires`/`ensures` clause to actually discharge — an empty
    // schema makes the Z3 translator reject every `Sort::Record` value
    // with `UnknownTypeName` instead of proving (or falsifying) the goal.
    let schema = std::sync::Arc::new(build_schema(ty_cx, lower_cx.ty_interner, lower_cx.interner));
    let solver = Z3Backend::with_schema(schema);
    // No caller `let`-bound locals participate in `requires` / `ensures`
    // discharge (those clauses range over parameters + `result`); pass an
    // empty local-sort map. The termination pass is the only caller that
    // populates `local_sorts`.
    let no_locals: HashMap<Symbol, (SmolStr, Sort)> = HashMap::new();
    let env = FnRefineEnv::new(
        package,
        &param_sorts,
        &no_locals,
        lower_cx.interner,
        lower_cx.ty_interner,
        ty_cx,
    );

    // Lift the function's own `requires` clauses once into the shared
    // assumption context. A clause that falls outside the lifter's
    // admitted fragment is dropped from the context (a hypothesis, so
    // dropping only loses completeness) — but no longer silently: its
    // description is recorded and attached as a note to every
    // discharge-failure diagnostic emitted below, mirroring the native
    // compiler's dropped-assumption note, so a spurious counterexample
    // names the assumption the solver never saw.
    let mut dropped_assumptions: Vec<String> = Vec::new();
    let mut requires_context: Vec<Predicate> = Vec::new();
    let mut req_idx: u32 = 0;
    for clause in fn_decl
        .refinements
        .iter()
        .filter(|c| c.kind == RefinementKind::Requires)
    {
        let i = req_idx;
        req_idx += 1;
        match lift_clause(clause, &env) {
            Ok(p) => requires_context.push(p),
            Err(e) => dropped_assumptions.push(format!(
                "attempted without out-of-fragment assumption: requires clause {i} — {e}"
            )),
        }
    }
    // Per-parameter inline refinements (`byte: u8 where byte >= 48 && byte
    // <= 57`) hold at function entry exactly like a top-level `requires`
    // clause — without this, an `ensures` clause correlated
    // with `result` via the tail-expression equality below has no way to
    // see the param bound it depends on, so Z3 falsifies a genuinely
    // correct postcondition (e.g. `digit_value`'s `ensures result <= 9` on
    // `return byte - 48` needs `byte <= 57` in context to prove).
    for param in &fn_decl.params {
        let Some(pred) = param_refinement_pred(&param.ty) else {
            continue;
        };
        match lift_predicate(pred, &env) {
            Ok(p) => requires_context.push(p),
            Err(e) => {
                let name = lower_cx.interner.try_resolve(param.name.name).unwrap_or("_");
                dropped_assumptions.push(format!(
                    "attempted without out-of-fragment assumption: `{name}` where-refinement — {e}"
                ));
            }
        }
    }
    // A record field's own inline `where` refinement (`nanos: i64 where
    // nanos >= 0`) holds unconditionally at every field-projection read
    // site — the type system guarantees every constructed
    // value satisfies its field refinements. Fold these facts from every
    // clause's predicate up front so they're in context for every
    // obligation discharged below. `env` here is this function's own
    // frame throughout, so `field_refinement_facts`' span-keyed lookups
    // resolve against this function's own params/locals — safe.
    for clause in &fn_decl.refinements {
        requires_context.extend(field_refinement_facts(&clause.pred, &env));
    }

    // Bind the `ensures` keyword `result` to a fresh `Var` of the
    // function's return sort so `lookup_path` resolves it
    // instead of returning `None` and silently dropping every `ensures`
    // clause that mentions it. An unconstrained `result` var alone would
    // let Z3 pick any value and falsify almost any nontrivial
    // postcondition, flipping the bug from false-negative to
    // false-positive — so the binding is installed ONLY when the
    // single-tail-expression body shape also yields a `result == <tail
    // expr>` equality fact to constrain it. Every other case (no
    // predicate-fragment return `Sort`, no trailing expression, a
    // `return` elsewhere in the body, a tail expression that doesn't
    // lift) leaves `result_binding` at its default `None`, so `result`
    // fails to resolve and the clause is skipped exactly as it was
    // before this fix — never discharged with `result` left
    // unconstrained.
    let return_sort = sort_of_ty(sig.return_ty, lower_cx.ty_interner, ty_cx);
    let mut result_equality: Option<Predicate> = None;
    if let Some(rsort) = &return_sort
        && let Some(tail_expr) = single_tail_return_expr(fn_decl)
        && let Ok(lifted) = lift_predicate(tail_expr, &env)
    {
        let result_sym = lower_cx.interner.intern("result");
        env.set_result_binding(Some((result_sym, SmolStr::new("result"), rsort.clone())));
        result_equality = Some(Predicate::cmp(
            CmpOp::Eq,
            Predicate::Var(Variable::new("result", rsort.clone())),
            lifted,
        ));
        // The tail expression itself may project into refined fields
        // (`return d.nanos`) — fold those facts in too so the equality
        // above is provably non-negative, not merely equal to an
        // unconstrained field read. Same-frame `env` as
        // every other fold in this function — safe.
        requires_context.extend(field_refinement_facts(tail_expr, &env));
    }

    let mut ens_idx: u32 = 0;
    for clause in &fn_decl.refinements {
        // `Requires` clauses are preconditions, not goals — they
        // populated `requires_context` above and are not discharged
        // here. C5 emits `TerminationDecreases` obligations at
        // recursive call sites; the function-level `Decreases` clause
        // itself does not produce an obligation. `Where` clauses guard
        // struct-construction sites and are deferred to a later wave.
        let obligation_kind = match clause.kind {
            RefinementKind::Ensures => {
                let i = ens_idx;
                ens_idx += 1;
                ObligationKind::EnsuresAtReturn { clause_index: i }
            }
            RefinementKind::Requires | RefinementKind::Where | RefinementKind::Decreases => {
                continue;
            }
        };
        let predicate = match lift_clause(clause, &env) {
            Ok(p) => p,
            Err(e) => {
                // A goal clause that fails to lift was previously skipped
                // with no signal of any kind — the signature advertises an
                // `ensures` the toolchain never checked (a
                // soundness-of-trust gap, unlike the dropped hypotheses
                // above which only lose completeness). Surface it as a
                // warn-severity `refinement_unproven` so the author knows
                // the contract is unverified. `@unverified` already admits
                // every obligation in the function explicitly, so the warn
                // is suppressed there.
                //
                // Only PREDICATE-CONTENT failures warn — the forms an
                // author can act on by restating or trusting. Resolution-
                // side failures stay on the documented silent path:
                // `UnresolvedPath` is dominated by the deliberate
                // `result`-binding skip above (an `ensures` naming
                // `result` in a body that is not single-tail-shaped — an
                // engine capability gap, 246 sites in std alone, not an
                // author-written out-of-fragment predicate), and
                // `SortMismatch` / `UnknownField` duplicate an upstream
                // typechecker diagnostic or bug per their doc contract.
                let author_actionable = !matches!(
                    e,
                    LiftError::UnresolvedPath { .. }
                        | LiftError::SortMismatch { .. }
                        | LiftError::UnknownField { .. }
                );
                if author_actionable && unverified_route.is_none() {
                    let diag = Diagnostic::new(
                        DiagnosticClass::RefinementUnproven,
                        Severity::Warn,
                        clause.span,
                        format!("{}: not verified — {e}", obligation_kind.header()),
                    )
                    .with_note(
                        "the clause fell outside the lifter's admitted fragment and never reached the solver",
                    )
                    .with_note(
                        "restate the predicate inside the decidable fragment, or admit the gap explicitly with `@trust(reason: \"...\")` at the site or `@unverified(reason: \"...\")` on the function",
                    );
                    diags.push(diag);
                }
                continue;
            }
        };
        let mut obligation_context = requires_context.clone();
        if let Some(eq) = &result_equality {
            obligation_context.push(eq.clone());
        }
        let mut obligation = Obligation::new(
            predicate,
            obligation_context,
            clause.span,
            obligation_kind,
            "",
        );
        if let Some(ref r) = unverified_route {
            obligation = obligation.with_route(r.clone());
        }
        let outcome = solver.discharge(&obligation, Duration::from_secs(5));
        if let Some(failure) = DischargeFailure::from_outcome(outcome) {
            let err = RefineError::discharge(
                clause.span,
                obligation.kind().clone(),
                obligation.predicate_text(),
                failure,
            );
            let mut diag = err.to_diagnostic();
            for note in &dropped_assumptions {
                diag = diag.with_note(note.clone());
            }
            diags.push(diag);
        }
    }
}

/// Emit `refinement_trivially_true` when `clause`'s predicate is the
/// literal `true`. `Decreases` is excluded because its predicate is a
/// well-founded measure (an integer expression), not a boolean.
fn emit_trivially_true_lint(clause: &RefinementClause, diags: &mut Diagnostics) {
    let keyword = match clause.kind {
        RefinementKind::Requires => "requires",
        RefinementKind::Ensures => "ensures",
        RefinementKind::Where => "where",
        RefinementKind::Decreases => return,
    };
    if !matches!(clause.pred.kind, ExprKind::Literal(Literal::Bool(true))) {
        return;
    }
    let class = DiagnosticClass::RefinementTriviallyTrue;
    let diag = Diagnostic::new(
        class,
        Severity::Warn,
        clause.span,
        format!("`{keyword} true` carries no verification information"),
    )
    .with_note(
        "remove the clause, or replace the literal with a real predicate over the function's bindings",
    );
    diags.push(diag);
}
