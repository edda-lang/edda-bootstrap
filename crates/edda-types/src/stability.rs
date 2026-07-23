//! Stability structural check.
//!
//! Per `corpus/edda-codex/language/03-verification.md` §7, a function
//! declared `stable function name(...)` carries four structural
//! obligations:
//!
//! 1. **Effect-row whitelist** — the row may contain entries only from
//!    `{err: T, panic, alloc, yield: T}` plus graded `alloc(bytes <= N)`
//!    and `time(ops <= N)`. Capability entries whose parameter type is
//!    `Clock`, `MonotonicClock`, `Stdin`, `Stdout`, `Stderr`,
//!    `Filesystem`, `Network`, or `Random` (the v0.1
//!    [`CapabilityType`] catalogue) are rejected; `cancellation` is
//!    rejected too (spawn-and-await timing is exactly the observable
//!    non-determinism `stable` forbids — the companion `scope(exec)`
//!    rejection below already denies this indirectly, this closes the
//!    same gap for a hand-written `with {cancellation}` row entry);
//!    `nondet` is rejected on the same axis (it marks observable
//!    non-determinism — `group.race` / `group.any`, ambient `Random`).
//!    `DeterministicRandom` is one of the
//!    capability carve-outs admitted here (alongside `Allocator` / `BoundedAllocator`) (D-20):
//!    a seeded deterministic RNG is
//!    reproducible by construction — bit-identical across runs — so it
//!    upholds stability's equal-inputs-equal-outputs guarantee, unlike
//!    ambient `Random`. The row check applies uniformly to bodied and
//!    bodyless `@abi` functions.
//!
//! 2. **Callee whitelist** — every direct **public** callee must
//!    itself be refinement-stable. Non-public callees (spec-private
//!    helpers, module-local helpers) carry no stability obligation
//!    per CLAUDE.md "Stability modifiers" and are admitted unchecked.
//!    The check descends into `scope(coherence)` regions (Phase C adds
//!    the discriminator) but rejects `scope(exec)` outright.
//!
//! 3. **Hash-iteration ban** — direct iteration over hashed
//!    containers is forbidden. v0.1 enforces this by qualified-name
//!    rejection of `std.hashmap.iter` / `std.hashset.iter` / their
//!    `keys` / `values` siblings at both path-form call sites
//!    (`std.hashmap.iter(m)`) and method-form call sites (`m.iter()`).
//!    The method-form check consults the typechecker's
//!    [`method_resolutions`] map to recover the resolved free function.
//!
//! 4. **`@unverified` rejection** — a function declared `stable function`
//!    cannot also carry an `@unverified(reason = "...")` attribute.
//!    Stability is itself a verification claim, and the trust hatch
//!    would dodge the structural checks. Rejected with
//!    `stability_unverified`.
//!
//! All four rules emit dedicated diagnostic classes (`stability_callee`,
//! `stability_effect`, `stability_hash_iter`, `stability_unverified`)
//! so authors and reviewers can grep their builds for any single
//! category.

use ahash::AHashMap;
use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_resolve::{BindingId, Resolved};
use edda_span::Span;
use edda_syntax::ast::visit::{self as ast_visit, Visitor};
use edda_syntax::ast::{Expr, ExprKind, FnBody, FnDecl, ScopeKind, Visibility};

use crate::attr::AttrSet;
use crate::capability::CapabilityType;
use crate::cx::TyCx;
use crate::effect::{EffectEntry, GradedKind, PureEffect};
use crate::lower::LowerCx;
use crate::sig::FnSig;
use crate::ty::TyKind;

/// Run the §7 structural check on `fn_decl`.
///
/// Short-circuits when `sig.refinement_stable` is `false`. When `true`,
/// validates the effect row, walks every body call/scope, checks the
/// `@unverified` rejection rule, and emits per-class diagnostics. The
/// function still type-checks regardless of stability violations;
/// these diagnostics surface alongside the function's other failures.
///
/// `method_resolutions` is the per-function method-resolution map
/// built by `infer::method::synth_method_call` (call-site span →
/// resolved free-function `BindingId`). The hash-iteration check
/// reads it to classify method-form receivers (`m.iter()`).
pub(crate) fn discharge_fn_stability(
    fn_decl: &FnDecl,
    sig: &FnSig,
    attrs: &AttrSet,
    ty_cx: &TyCx,
    lower_cx: &LowerCx<'_>,
    method_resolutions: &AHashMap<Span, BindingId>,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    if !sig.refinement_stable {
        return;
    }
    let Some(package) = lower_cx.package else {
        return;
    };

    check_effect_row(fn_decl, sig, lower_cx, lint_cfg, diags);
    check_unverified_attr(attrs, lint_cfg, diags);

    let body_block = match &fn_decl.body {
        FnBody::Block(b) => b,
        FnBody::Extern { .. } => return,
    };
    let mut walker = StabilityWalker {
        ty_cx,
        lower_cx,
        package,
        method_resolutions,
        lint_cfg,
        diags,
    };
    walker.visit_block(body_block);
}

// --- Rule 4: `@unverified` rejection on stable functions -----------------

fn check_unverified_attr(
    attrs: &AttrSet,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    let Some(unverified) = attrs.unverified else {
        return;
    };
    let sev = lint_cfg.effective(DiagnosticClass::StabilityUnverified);
    let msg = "stable function carries `@unverified` — stability is itself a verification claim; \
               either drop `@unverified` or drop the `stable` modifier"
        .to_string();
    diags.push(Diagnostic::new(
        DiagnosticClass::StabilityUnverified,
        sev,
        unverified.attr_span,
        msg,
    ));
}

// --- Rule 1: effect-row whitelist -----------------------------------------

fn check_effect_row(
    fn_decl: &FnDecl,
    sig: &FnSig,
    lower_cx: &LowerCx<'_>,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    let sev = lint_cfg.effective(DiagnosticClass::StabilityEffect);
    let row_span = fn_decl
        .effects
        .as_ref()
        .map(|r| r.span)
        .unwrap_or(fn_decl.span);

    // Pure-effect entries: only Panic / Err / Yield / Divergence
    // pass. (Divergence is admissible in stable rows — it documents
    // possible non-termination but is reproducible.) Cancellation and
    // Nondet are rejected — cancellation can only originate from
    // `.await`, which observes spawn timing; nondet marks observable
    // non-determinism (`group.race` / `group.any`, ambient `Random`).
    // Both break the equal-inputs-equal-outputs guarantee the same way
    // `scope(exec)` does.
    for entry in sig.effects.entries() {
        match entry {
            EffectEntry::Pure(PureEffect::Cancellation) => {
                let msg = "stable function's row contains `cancellation`, which is not \
                           in the §7 whitelist"
                    .to_string();
                diags.push(Diagnostic::new(
                    DiagnosticClass::StabilityEffect,
                    sev,
                    row_span,
                    msg,
                ));
            }
            EffectEntry::Pure(PureEffect::Nondet) => {
                let msg = "stable function's row contains `nondet`, which is not \
                           in the §7 whitelist"
                    .to_string();
                diags.push(Diagnostic::new(
                    DiagnosticClass::StabilityEffect,
                    sev,
                    row_span,
                    msg,
                ));
            }
            EffectEntry::Capability(sym) => {
                // Locate the parameter to check its declared type against
                // the locked CapabilityType blocklist.
                if let Some(param) = sig.params.iter().find(|p| p.name == *sym) {
                    if let TyKind::Capability(cap) = lower_cx.ty_interner.kind(param.ty) {
                        if is_unstable_capability(*cap) {
                            let name = lower_cx.interner.resolve(*sym);
                            let msg = format!(
                                "stable function's row contains `{name}` (capability `{cap:?}`), \
                                 which is not in the §7 whitelist"
                            );
                            diags.push(Diagnostic::new(
                                DiagnosticClass::StabilityEffect,
                                sev,
                                row_span,
                                msg,
                            ));
                        }
                    }
                }
            }
            EffectEntry::Pure(_) => {
                // Panic / Err / Yield / Divergence — all admitted.
                // Cancellation and Nondet are matched above and rejected.
            }
        }
    }

    // Graded entries: alloc and time admitted; io rejected.
    for gb in sig.graded_bounds.iter() {
        if let GradedKind::Io = gb.kind {
            let msg = "stable function's row contains graded `io(...)`, which is not in the §7 whitelist"
                .to_string();
            diags.push(Diagnostic::new(
                DiagnosticClass::StabilityEffect,
                sev,
                gb.span,
                msg,
            ));
        }
    }
}

fn is_unstable_capability(c: CapabilityType) -> bool {
    matches!(
        c,
        CapabilityType::Clock
            | CapabilityType::MonotonicClock
            | CapabilityType::Stdin
            | CapabilityType::Stdout
            | CapabilityType::Stderr
            | CapabilityType::Filesystem
            | CapabilityType::Network
            | CapabilityType::Random
            | CapabilityType::Executor
            | CapabilityType::ReadOnlyFilesystem
            | CapabilityType::SandboxedFilesystem
            | CapabilityType::LocalhostNetwork
            | CapabilityType::RestrictedNetwork
            | CapabilityType::Subprocess
            | CapabilityType::Debugger
    )
    // `Allocator` and `BoundedAllocator` are intentionally excluded:
    // stable functions admit `allocator: Allocator` / `allocator:
    // BoundedAllocator` in their row (the row whitelist explicitly
    // allows `alloc` and graded `alloc(bytes <= N)` per §7 Rule 1;
    // `BoundedAllocator` is the typed surface of the bounded form).
    // `DeterministicRandom` is also excluded (admitted) per D-20:
    // a seeded deterministic RNG is
    // reproducible by construction — bit-identical across runs — so it
    // upholds the equal-inputs-equal-outputs guarantee, unlike ambient
    // `Random`.
}

// --- Rule 2 + 3: callee whitelist and hash-iteration ban -------------------

struct StabilityWalker<'a> {
    ty_cx: &'a TyCx,
    lower_cx: &'a LowerCx<'a>,
    package: &'a edda_resolve::ResolvedPackage,
    /// Per-function method-call resolution map (call-site span →
    /// resolved free-function `BindingId`). Read by
    /// [`StabilityWalker::check_method_call`] to recover the
    /// qualified name of the resolved callee for the hash-iteration
    /// ban; method calls that the typechecker did not resolve (e.g.
    /// because typing failed earlier) are skipped silently.
    method_resolutions: &'a AHashMap<Span, BindingId>,
    lint_cfg: &'a LintConfig,
    diags: &'a mut Diagnostics,
}

impl<'ast> Visitor<'ast> for StabilityWalker<'_> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        match &expr.kind {
            ExprKind::Call { callee, .. } => {
                self.check_call_site(callee, expr.span);
            }
            ExprKind::MethodCall { receiver, name, .. } => {
                self.check_method_call(receiver, name, expr.span);
            }
            ExprKind::Scope { kind: ScopeKind::Exec, .. } => {
                self.emit_exec_scope(expr.span);
            }
            _ => {}
        }
        ast_visit::walk_expr(self, expr);
    }
}

impl<'a> StabilityWalker<'a> {
    /// Check a `callee(args)` call site against the callee whitelist
    /// and the hash-iteration ban.
    fn check_call_site(&mut self, callee: &Expr, call_span: Span) {
        let ExprKind::Path(path) = &callee.kind else {
            return;
        };
        // Hash-iteration ban — match by qualified name first so the
        // diagnostic-class projection is precise even when the callee
        // is also marked non-stable.
        if let Some(qname) = qualified_path(path, self.lower_cx) {
            if is_hash_iter_function(&qname) {
                self.emit_hash_iter(call_span, &qname);
                return;
            }
        }
        // Callee whitelist — must be refinement-stable.
        let Some(Resolved::Binding(callee_binding)) =
            self.package.resolutions().lookup_path(path.span)
        else {
            return;
        };
        let Some(callee_sig) = self.ty_cx.sig(callee_binding) else {
            return;
        };
        if !callee_sig.refinement_stable {
            let callee_entry = self.package.binding(callee_binding);
            // CLAUDE.md "Stability modifiers" — a `stable` function may call
            // its module-local / spec-private helpers without those helpers
            // having to declare themselves `stable`
            if callee_entry.visibility == Visibility::Public {
                let callee_name =
                    self.lower_cx.interner.resolve(callee_entry.name).to_string();
                self.emit_nonstable_callee(call_span, &callee_name);
            }
        }
    }

    /// Receiver-type-aware hash-iteration check for method-form calls.
    ///
    /// Looks up the method-call span in
    /// [`StabilityWalker::method_resolutions`] to recover the resolved
    /// free-function `BindingId`. Reconstructs its qualified name
    /// (`<module-canonical-path>.<binding-name>`) and emits
    /// `stability_hash_iter` when that qname matches one of the
    /// hash-iteration sites in [`is_hash_iter_function`]. Resolutions
    /// the typechecker did not record (failed typing, intrinsic
    /// methods) are skipped silently — those produce their own
    /// upstream diagnostics.
    fn check_method_call(
        &mut self,
        _receiver: &Expr,
        _name: &edda_syntax::ast::Ident,
        call_span: Span,
    ) {
        let Some(binding_id) = self.method_resolutions.get(&call_span).copied() else {
            return;
        };
        let Some(qname) = qualified_binding_name(binding_id, self.lower_cx, self.package)
        else {
            return;
        };
        if is_hash_iter_function(&qname) {
            self.emit_hash_iter(call_span, &qname);
        }
        // Callee whitelist for method form: the resolved callee must
        // itself be refinement-stable when public. Non-public callees
        // carry no stability obligation per CLAUDE.md — match the
        // path-form semantics so the two surfaces agree.
        if let Some(callee_sig) = self.ty_cx.sig(binding_id) {
            if !callee_sig.refinement_stable {
                let callee_entry = self.package.binding(binding_id);
                if callee_entry.visibility == Visibility::Public {
                    let callee_name =
                        self.lower_cx.interner.resolve(callee_entry.name).to_string();
                    self.emit_nonstable_callee(call_span, &callee_name);
                }
            }
        }
    }

    // --- Diagnostic emitters --------------------------------------------

    fn emit_nonstable_callee(&mut self, span: Span, callee_name: &str) {
        let sev = self.lint_cfg.effective(DiagnosticClass::StabilityCallee);
        let msg = format!(
            "stable function calls non-stable function `{callee_name}` — \
             the callee must itself be declared `stable function`"
        );
        self.diags.push(Diagnostic::new(
            DiagnosticClass::StabilityCallee,
            sev,
            span,
            msg,
        ));
    }

    fn emit_exec_scope(&mut self, span: Span) {
        let sev = self.lint_cfg.effective(DiagnosticClass::StabilityCallee);
        let msg =
            "stable function contains a `scope(exec)` block — spawn-and-await timing \
             is observable and breaks the equal-inputs-equal-outputs guarantee"
                .to_string();
        self.diags.push(Diagnostic::new(
            DiagnosticClass::StabilityCallee,
            sev,
            span,
            msg,
        ));
    }

    fn emit_hash_iter(&mut self, span: Span, qname: &str) {
        let sev = self.lint_cfg.effective(DiagnosticClass::StabilityHashIter);
        let msg = format!(
            "stable function calls `{qname}` — hash-iteration order varies across \
             runs; use `iter_sorted_by_key` or `iter_in_insertion_order` instead"
        );
        self.diags.push(Diagnostic::new(
            DiagnosticClass::StabilityHashIter,
            sev,
            span,
            msg,
        ));
    }
}

/// Reconstruct a binding's fully qualified name as a dotted string
/// (`std.hashmap.iter`).
///
/// Joins the binding's owning module's canonical path with the
/// binding's own name. Returns `None` when the binding's owning module
/// is not present in the resolved source graph (defensive guard;
/// should not happen in well-formed packages).
fn qualified_binding_name(
    binding_id: BindingId,
    lower_cx: &LowerCx<'_>,
    package: &edda_resolve::ResolvedPackage,
) -> Option<String> {
    let entry = package.binding(binding_id);
    let module_path = &package.graph().module(entry.module).canonical_path;
    let mut out = module_path.to_owned_string(lower_cx.interner);
    out.push('.');
    out.push_str(lower_cx.interner.resolve(entry.name));
    Some(out)
}

/// Render an AST [`Path`](edda_syntax::ast::Path) as a dotted
/// qualified-name string.
fn qualified_path(path: &edda_syntax::ast::Path, lower_cx: &LowerCx<'_>) -> Option<String> {
    if path.segments.is_empty() {
        return None;
    }
    let parts: Vec<&str> = path
        .segments
        .iter()
        .map(|s| lower_cx.interner.resolve(s.name))
        .collect();
    Some(parts.join("."))
}

/// Hash-iteration ban list: the §7 Rule 3 enumeration of functions
/// whose iteration order depends on the hash seed.
fn is_hash_iter_function(qname: &str) -> bool {
    matches!(
        qname,
        "std.hashmap.iter"
            | "std.hashmap.keys"
            | "std.hashmap.values"
            | "std.hashmap.iter_mut"
            | "std.hashmap.values_mut"
            | "std.hashset.iter"
            | "std.hashset.iter_mut"
    )
}
