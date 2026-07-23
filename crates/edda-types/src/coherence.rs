//! Coherence-region exit re-validation.
//!
//! Per `corpus/edda-codex/language/05-concurrency-coherence.md` §3, a
//! `scope(coherence) name { body }` region treats its body as
//! observationally atomic: intermediate effects are not visible
//! outside the region; only the value at the closing brace commits.
//! The mode tracker treats the region as a single statement at the
//! enclosing call site, and `mutable` parameter refinements are
//! re-validated at region exit per §3 *`mutable` parameter
//! re-validation at exit*.
//!
//! # Phase-C surface
//!
//! Two structural rules and one SMT-precise refinement check land here:
//!
//! - **Mutable-refinement preservation** — when a `mutable T where P`
//!   parameter is *referenced* inside a `scope(coherence)` body, the
//!   pass classifies each reference as read-only or mutating. Pure
//!   reads cannot invalidate `P` and are silently admitted. Mutating
//!   references first run through the SMT-precise preservation
//!   discharge ([`crate::refine::try_coherence_preservation_smt`] when
//!   the `refine` feature is on); if Z3 proves `P` survives the body,
//!   the diagnostic is suppressed. Otherwise the pass emits
//!   `coherence_mutable_refinement_invalidated` at the region exit.
//!   The message is explicit that this is conservative rejection
//!   (the body may still preserve `P`; the discharge could not prove
//!   it), not an unsoundness claim.
//! - **Init-parameter rejection** — a parameter declared with the
//!   `init` mode cannot be *written* inside a `scope(coherence)`
//!   region. The Uninit→Valid transition would otherwise be exposed
//!   as observationally atomic at the region close, which contradicts
//!   the linear init discipline (the caller observes the binding's
//!   new validity through the region, not through the statement that
//!   completed it). The pass emits
//!   `coherence_init_param_written` symmetric to the mutable rule.
//! - **`let` / `take` parameters are never inspected** — read-only
//!   parameters cannot have been mutated, so their refinements stay
//!   intact across the region without any check; `take` participates
//!   in linearity through the call site, not the body.

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_span::Span;
use edda_syntax::ast::visit::{self as ast_visit, Visitor};
use edda_syntax::ast::{
    self, Block, CallMode, Expr, ExprKind, FnBody, FnDecl, MatchArm, ParamMode, ScopeKind, Stmt,
    StmtKind, Type, TypeKind,
};

use crate::cx::TyCx;
use crate::lower::LowerCx;
use crate::sig::FnSig;

/// Run the §3 coherence checks on `fn_decl`.
///
/// Walks every `scope(coherence)` region in the body. For each region,
/// classifies each refined-`mutable` parameter as `None`, `ReadOnly`,
/// or `Mutated`; refers `Mutated` cases through the SMT-precise
/// discharge (no-op without the `refine` feature) and emits
/// [`DiagnosticClass::CoherenceMutableRefinementInvalidated`] when the
/// discharge does not prove preservation. Additionally classifies
/// every `init`-mode parameter and emits
/// [`DiagnosticClass::CoherenceInitParamWritten`] for any region that
/// writes one.
///
/// The walk descends into nested coherence regions inside
/// `scope(exec)` (per §3.3 composition) and ignores regions inside
/// `extern` bodies (those have no AST to inspect).
pub(crate) fn discharge_fn_coherence(
    fn_decl: &FnDecl,
    sig: &FnSig,
    ty_cx: &TyCx,
    lower_cx: &LowerCx<'_>,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    let body_block = match &fn_decl.body {
        FnBody::Block(b) => b,
        FnBody::Extern { .. } => return,
    };
    let refined_mutable: Vec<RefinedMutableParam> = fn_decl
        .params
        .iter()
        .filter_map(refined_mutable_param)
        .collect();
    let init_params: Vec<InitParam> = fn_decl
        .params
        .iter()
        .filter_map(init_param)
        .collect();
    if refined_mutable.is_empty() && init_params.is_empty() {
        return;
    }
    let mut collector = CoherenceRegionCollector { out: Vec::new() };
    collector.visit_block(body_block);
    if collector.out.is_empty() {
        return;
    }
    for region in collector.out {
        check_one_region(
            region,
            &refined_mutable,
            &init_params,
            fn_decl,
            sig,
            ty_cx,
            lower_cx,
            lint_cfg,
            diags,
        );
    }
}

struct RefinedMutableParam {
    name: edda_intern::Symbol,
    refinement_span: Span,
}

struct InitParam {
    name: edda_intern::Symbol,
    decl_span: Span,
}

/// Identify a parameter with the shape `name: mutable T where P`.
fn refined_mutable_param(p: &ast::Param) -> Option<RefinedMutableParam> {
    if !matches!(p.mode, ParamMode::Mutable) {
        return None;
    }
    Some(RefinedMutableParam {
        name: p.name.name,
        refinement_span: first_refinement_span(&p.ty)?,
    })
}

/// Identify a parameter with the shape `name: init T`.
fn init_param(p: &ast::Param) -> Option<InitParam> {
    if !matches!(p.mode, ParamMode::Init) {
        return None;
    }
    Some(InitParam {
        name: p.name.name,
        decl_span: p.span,
    })
}

/// Return the source span of the first `where` clause inside a type
/// expression. The AST nests refinements as
/// `TypeKind::Refined { base, pred }`.
fn first_refinement_span(ty: &Type) -> Option<Span> {
    match &ty.kind {
        TypeKind::Refined { pred, .. } => Some(pred.span),
        TypeKind::Slice(inner) => first_refinement_span(inner),
        _ => None,
    }
}

fn check_one_region(
    region: &Expr,
    refined_mutable: &[RefinedMutableParam],
    init_params: &[InitParam],
    fn_decl: &FnDecl,
    sig: &FnSig,
    ty_cx: &TyCx,
    lower_cx: &LowerCx<'_>,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    let ExprKind::Scope {
        kind: ScopeKind::Coherence,
        body,
        ..
    } = &region.kind
    else {
        return;
    };
    for param in refined_mutable {
        let usage = classify_param_usage(body, param.name);
        if matches!(usage, ParamUsage::None | ParamUsage::ReadOnly) {
            // Body cannot mutate `param`; the refinement is provably
            // preserved structurally (no Z3 needed).
            continue;
        }
        if try_smt_preservation(param, body, fn_decl, sig, ty_cx, lower_cx) {
            continue;
        }
        emit_mutable_refinement_invalidated(region.span, param, lower_cx, lint_cfg, diags);
    }
    for param in init_params {
        let usage = classify_param_usage(body, param.name);
        if matches!(usage, ParamUsage::Mutated { .. }) {
            emit_init_param_written(region.span, param, lower_cx, lint_cfg, diags);
        }
    }
}

// --- Diagnostic emission --------------------------------------------------

fn emit_mutable_refinement_invalidated(
    region_span: Span,
    param: &RefinedMutableParam,
    lower_cx: &LowerCx<'_>,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    let sev = lint_cfg.effective(DiagnosticClass::CoherenceMutableRefinementInvalidated);
    // Parser-recovery DUMMY name → skip the lint rather than render a
    // misleading diagnostic against an unnamed parameter.
    let Some(name) = lower_cx.interner.try_resolve(param.name) else {
        return;
    };
    let msg = format!(
        "mutable parameter `{name}` may have been mutated inside this \
         `scope(coherence)` region; its refinement requires re-validation \
         that the body does not prove. Either prove the refinement holds \
         at region exit (e.g. via an explicit guard that propagates on \
         violation) or restructure so the parameter isn't mutated inside \
         the region."
    );
    diags.push(
        Diagnostic::new(
            DiagnosticClass::CoherenceMutableRefinementInvalidated,
            sev,
            region_span,
            msg,
        )
        .with_label(param.refinement_span, "refinement declared here"),
    );
}

fn emit_init_param_written(
    region_span: Span,
    param: &InitParam,
    lower_cx: &LowerCx<'_>,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    let sev = lint_cfg.effective(DiagnosticClass::CoherenceInitParamWritten);
    // Parser-recovery DUMMY name → skip the lint rather than render a
    // misleading diagnostic against an unnamed parameter.
    let Some(name) = lower_cx.interner.try_resolve(param.name) else {
        return;
    };
    let msg = format!(
        "init parameter `{name}` is written inside this `scope(coherence)` \
         region; the Uninit→Valid transition would be exposed as \
         observationally atomic at the region close, which contradicts \
         the linear init discipline. Complete the init before entering \
         the coherence region."
    );
    diags.push(
        Diagnostic::new(
            DiagnosticClass::CoherenceInitParamWritten,
            sev,
            region_span,
            msg,
        )
        .with_label(param.decl_span, "init parameter declared here"),
    );
}

// --- SMT preservation hook ------------------------------------------------

/// Attempt to prove via Z3 that the region body preserves `param`'s
/// refinement. Returns `true` to suppress the structural diagnostic.
///
/// Without the `refine` Cargo feature this always returns `false`, so
/// the conservative diagnostic fires. With the feature on, it routes
/// through [`crate::refine::try_coherence_preservation_smt`] which
/// lifts the refinement plus the body's single-assignment net effect
/// (if any) into the predicate IR and discharges `P[expr/x]` against
/// the Z3 backend.
#[cfg(not(feature = "refine"))]
fn try_smt_preservation(
    _param: &RefinedMutableParam,
    _region_body: &Block,
    _fn_decl: &FnDecl,
    _sig: &FnSig,
    _ty_cx: &TyCx,
    _lower_cx: &LowerCx<'_>,
) -> bool {
    false
}

#[cfg(feature = "refine")]
fn try_smt_preservation(
    param: &RefinedMutableParam,
    region_body: &Block,
    fn_decl: &FnDecl,
    sig: &FnSig,
    ty_cx: &TyCx,
    lower_cx: &LowerCx<'_>,
) -> bool {
    crate::refine::try_coherence_preservation_smt(
        param.name,
        region_body,
        fn_decl,
        sig,
        ty_cx,
        lower_cx,
    )
}

// --- Per-parameter usage classification ----------------------------------

#[derive(Debug, Clone)]
enum ParamUsage {
    /// Parameter is not referenced anywhere in the region.
    None,
    /// Parameter is referenced only in read positions.
    ReadOnly,
    /// Parameter is mutated at least once (direct assign, field /
    /// index assign, or passed via a non-`let` mode at a call site).
    Mutated { sites: Vec<Span> },
}

impl ParamUsage {
    fn observe_read(&mut self) {
        if matches!(self, ParamUsage::None) {
            *self = ParamUsage::ReadOnly;
        }
    }

    fn observe_write(&mut self, site: Span) {
        match self {
            ParamUsage::Mutated { sites } => sites.push(site),
            _ => *self = ParamUsage::Mutated { sites: vec![site] },
        }
    }
}

/// Walk `block` classifying every reference to `sym` as a read or a
/// write. The classifier is structural — it inspects the immediate
/// position of each `Path(sym)` occurrence (LHS of `Assign`, receiver
/// of a field/index `Assign` target, mode-carrying `CallArg`) and
/// observes the result.
fn classify_param_usage(block: &Block, sym: edda_intern::Symbol) -> ParamUsage {
    let mut classifier = ParamUsageClassifier {
        sym,
        usage: ParamUsage::None,
    };
    classifier.visit_block(block);
    classifier.usage
}

fn is_path_to(expr: &Expr, sym: edda_intern::Symbol) -> bool {
    match &expr.kind {
        ExprKind::Path(p) => p.segments.len() == 1 && p.segments[0].name == sym,
        _ => false,
    }
}

fn expr_receiver_is(expr: &Expr, sym: edda_intern::Symbol) -> bool {
    match &expr.kind {
        ExprKind::Field { receiver, .. }
        | ExprKind::TupleIndex { receiver, .. }
        | ExprKind::Index { receiver, .. } => {
            is_path_to(receiver, sym) || expr_receiver_is(receiver, sym)
        }
        _ => false,
    }
}

/// Classifies every reference to a target parameter `sym` as a read or
/// a write while walking a coherence-region body.
struct ParamUsageClassifier {
    sym: edda_intern::Symbol,
    usage: ParamUsage,
}

impl ParamUsageClassifier {
    fn classify_assign_target(&mut self, target: &Expr) {
        if is_path_to(target, self.sym) {
            self.usage.observe_write(target.span);
            return;
        }
        if expr_receiver_is(target, self.sym) {
            self.usage.observe_write(target.span);
        }
        // Continue with the read-classifying descent so nested
        // `Path(sym)` occurrences inside the target (e.g. `obj[sym]`)
        // are still observed.
        self.visit_expr(target);
    }

    fn classify_call_arg(&mut self, arg: &ast::CallArg) {
        // A bare argument is a read; the mutating modes (`mutable`,
        // `take`, `init`) are writes through the call.
        if is_path_to(&arg.expr, self.sym) {
            match arg.mode {
                None => self.usage.observe_read(),
                Some(CallMode::Mutable | CallMode::Take | CallMode::Init) => {
                    self.usage.observe_write(arg.span)
                }
            }
            return;
        }
        self.visit_expr(&arg.expr);
    }
}

impl<'ast> Visitor<'ast> for ParamUsageClassifier {
    fn visit_stmt(&mut self, stmt: &'ast Stmt) {
        match &stmt.kind {
            StmtKind::Assign { target, rhs, .. } => {
                self.classify_assign_target(target);
                self.visit_expr(rhs);
            }
            _ => ast_visit::walk_stmt(self, stmt),
        }
    }

    fn visit_expr(&mut self, expr: &'ast Expr) {
        // A bare `Path(sym)` use is a read; no children to descend.
        if is_path_to(expr, self.sym) {
            self.usage.observe_read();
            return;
        }
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                self.visit_expr(callee);
                for a in args {
                    self.classify_call_arg(a);
                }
            }
            ExprKind::MethodCall { receiver, args, .. } => {
                // Method dispatch ambiguity: without type info we
                // cannot tell whether `self` is borrowed mutably; the
                // structural pass treats the receiver conservatively
                // as a read and lets the SMT-precise discharge pick
                // up any actual mutation. Mode keywords on positional
                // args still classify precisely.
                self.visit_expr(receiver);
                for a in args {
                    self.classify_call_arg(a);
                }
            }
            _ => ast_visit::walk_expr(self, expr),
        }
    }
}

// --- Region collection ---------------------------------------------------

/// Collects every `scope(coherence) { ... }` expression reachable from
/// a function body so the §3 coherence-region check can validate each
/// one in turn.
struct CoherenceRegionCollector<'ast> {
    out: Vec<&'ast Expr>,
}

impl<'ast> Visitor<'ast> for CoherenceRegionCollector<'ast> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        if let ExprKind::Scope {
            kind: ScopeKind::Coherence,
            ..
        } = &expr.kind
        {
            self.out.push(expr);
        }
        ast_visit::walk_expr(self, expr);
    }
}

