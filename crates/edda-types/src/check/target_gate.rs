//! Per-target `@target_requires(T)` whole-function gate.
//!
//! Ports the self-hosted native compiler's design
//! (`compiler/lib/types/src/check/pass/avail/gate.ea`)
//! into the bootstrap: a function whose
//! `@target_requires(T)` capability is unsupported on the compile target
//! is treated as absent. [`collect_target_gated`] finds those bindings
//! from the raw resolved AST — before signature/body typecheck runs, so
//! [`super::check_package`] can skip lowering a gated-absent function's
//! body into [`super::TypedPackage::fn_bodies`] /
//! [`super::TypedPackage::extern_decls`]. [`check_target_requires_gate`]
//! then walks every surviving function body for `Call` sites that still
//! reference a gated-absent binding and diagnoses them.

use ahash::AHashMap;
use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_intern::{Interner, Symbol};
use edda_resolve::{BindingId, Resolutions, Resolved, ResolvedPackage};
use edda_span::Span;
use edda_syntax::ast::visit::{self as ast_visit, Visitor};
use edda_syntax::ast::{AttrArg, Attribute, Expr, ExprKind, FnBody, ItemKind};
use edda_target::TargetTriple;

/// Collect every Function binding whose `@target_requires(T)` capability
/// is unavailable on `target`, scanning the raw resolved AST directly
/// (ahead of the [`super::check_package`] item walk that consumes this
/// map's decisions).
pub(crate) fn collect_target_gated(
    package: &ResolvedPackage,
    interner: &Interner,
    target: &TargetTriple,
) -> AHashMap<BindingId, Symbol> {
    let mut gated = AHashMap::default();
    for module_entry in package.graph().modules() {
        let module_resolved = package.module(module_entry.id);
        for item in &module_entry.ast.items {
            let ItemKind::Function(fn_decl) = &item.kind else {
                continue;
            };
            let Some(cap) = first_unavailable_capability(&item.attributes, interner, target)
            else {
                continue;
            };
            if let Some(binding) = module_resolved.items.lookup(fn_decl.name.name) {
                gated.insert(binding, cap);
            }
        }
    }
    gated
}

fn first_unavailable_capability(
    attrs: &[Attribute],
    interner: &Interner,
    target: &TargetTriple,
) -> Option<Symbol> {
    for attr in attrs {
        let Some(name) = interner.try_resolve(attr.name.name) else {
            continue;
        };
        if name != "target_requires" {
            continue;
        }
        for arg in &attr.args {
            if let AttrArg::Ident(id) = arg
                && !target.supports_capability(interner.resolve(id.name))
            {
                return Some(id.name);
            }
        }
    }
    None
}

/// Walk every function body in `package` for `Call` sites whose resolved
/// callee is in `gated`, emitting
/// [`DiagnosticClass::CapabilityNotAvailableOnTarget`] at each one. The
/// message text matches the native self-host's diagnostic
/// verbatim for bootstrap/native parity.
pub(crate) fn check_target_requires_gate(
    package: &ResolvedPackage,
    interner: &Interner,
    gated: &AHashMap<BindingId, Symbol>,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    if gated.is_empty() {
        return;
    }
    let resolutions = package.resolutions();
    for module_entry in package.graph().modules() {
        let module_resolved = package.module(module_entry.id);
        for item in &module_entry.ast.items {
            let ItemKind::Function(fn_decl) = &item.kind else {
                continue;
            };
            let FnBody::Block(body) = &fn_decl.body else {
                continue;
            };
            // A gated-absent function's own body is never lowered
            // (`check_package` skips it) — a call from inside it to
            // another gated-absent function is consistent, not a
            // reachable-on-this-target violation, so its body is not
            // walked for diagnostics either (mirrors the native
            // self-host's `walk_item`'s `!is_gated` guard).
            if module_resolved
                .items
                .lookup(fn_decl.name.name)
                .is_some_and(|b| gated.contains_key(&b))
            {
                continue;
            }
            let mut collector = TargetGateCollector {
                resolutions,
                gated,
                interner,
                lint_cfg,
                diags,
            };
            collector.visit_block(body);
        }
    }
}

struct TargetGateCollector<'a> {
    resolutions: &'a Resolutions,
    gated: &'a AHashMap<BindingId, Symbol>,
    interner: &'a Interner,
    lint_cfg: &'a LintConfig,
    diags: &'a mut Diagnostics,
}

impl<'a, 'ast> Visitor<'ast> for TargetGateCollector<'a> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        if let ExprKind::Call { callee, args } = &expr.kind {
            if let Some(cap) = resolve_gated_callee(callee, self.resolutions, self.gated) {
                emit_call_unavailable(self.diags, self.lint_cfg, expr.span, self.interner.resolve(cap));
            }
            self.visit_expr(callee);
            for arg in args.iter() {
                self.visit_expr(&arg.expr);
            }
            return;
        }
        ast_visit::walk_expr(self, expr);
    }
}

fn resolve_gated_callee(
    callee: &Expr,
    resolutions: &Resolutions,
    gated: &AHashMap<BindingId, Symbol>,
) -> Option<Symbol> {
    let ExprKind::Path(path) = &callee.kind else {
        return None;
    };
    let resolved = resolutions.lookup_path(path.span)?;
    let Resolved::Binding(binding) = resolved else {
        return None;
    };
    gated.get(&binding).copied()
}

fn emit_call_unavailable(diags: &mut Diagnostics, lint_cfg: &LintConfig, span: Span, cap_name: &str) {
    let sev = lint_cfg.effective(DiagnosticClass::CapabilityNotAvailableOnTarget);
    let message = format!(
        "call to a function gated by `@target_requires({})` that does not exist on the build target",
        cap_name
    );
    diags.push(Diagnostic::new(
        DiagnosticClass::CapabilityNotAvailableOnTarget,
        sev,
        span,
        message,
    ));
}
