//! The `dead_private_function` lint: private fns never referenced anywhere.

use std::collections::HashSet;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_syntax::ast::{FnBody, ItemKind};

use crate::ResolvedPackage;
use crate::binding::{BindingId, BindingKind};
use crate::resolutions::Resolved;
use crate::resolve::ResolveCx;

/// Emit `dead_private_function` for every non-`public` function whose
/// binding is never referenced in the package's resolutions map.
///
/// Skips:
///   - `public` functions (they're part of the module's surface)
///   - extern declarations (the linker, not Edda code, is the user)
///   - functions carrying an `@property` attribute (called by the PBT
///     test harness, never directly referenced in source)
///   - parser-recovery functions with `Symbol::DUMMY` names
pub fn emit_dead_private_function_lints(
    package: &ResolvedPackage,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) {
    let severity = lint_cfg.effective(DiagnosticClass::DeadPrivateFunction);
    // Build the set of every BindingId that appears as the target of any
    // Path resolution anywhere in the package. Spans aren't relevant —
    // only the resolved identity.
    let mut referenced: HashSet<BindingId> = HashSet::new();
    for resolved in package.resolutions.values() {
        if let Resolved::Binding(id) = resolved {
            referenced.insert(id);
        }
    }
    for module in &package.modules {
        let entry = package.graph.module(module.id);
        for item in &entry.ast.items {
            let ItemKind::Function(fn_decl) = &item.kind else {
                continue;
            };
            // Public: part of the module's surface, skip.
            if matches!(fn_decl.visibility, edda_syntax::ast::Visibility::Public) {
                continue;
            }
            // Extern declarations: linker is the consumer, not Edda code.
            if !matches!(fn_decl.body, FnBody::Block(_)) {
                continue;
            }
            // `@property` functions are called by the PBT harness.
            if item.attributes.iter().any(|a| cx.interner.resolve(a.name.name) == "property") {
                continue;
            }
            // Find the function's BindingId in the module's item table.
            // The BindingId is the entry's index in declaration order.
            let target_id = module
                .items
                .entries()
                .iter()
                .enumerate()
                .find(|(_, e)| {
                    e.name == fn_decl.name.name && matches!(e.kind, BindingKind::Function)
                })
                .map(|(i, _)| BindingId::new(module.id, i as u32));
            let Some(id) = target_id else { continue; };
            if referenced.contains(&id) {
                continue;
            }
            let name_text = cx.interner.resolve(fn_decl.name.name);
            let diag = Diagnostic::new(
                DiagnosticClass::DeadPrivateFunction,
                severity,
                fn_decl.name.span,
                format!("private function `{name_text}` is never referenced"),
            )
            .with_note(
                "remove the declaration, or expose the function with `public` if it should be part of the module's surface",
            );
            diags.push(diag);
        }
    }
}
