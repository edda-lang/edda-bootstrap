//! The `unused_import` lint: import leaves no pass has marked used.

use edda_diag::{Diagnostics, LintConfig};

use crate::ResolvedPackage;
use crate::graph::ModuleId;
use crate::resolve::ResolveCx;

/// Emit `unused_import` diagnostics for every import-leaf in
/// `package` that no pass has marked used.
///
/// The driver calls this after the typechecker has finished;
/// `edda_types`-side passes like method-call resolution have already
/// fed back their "import was effectively referenced" signals through
/// [`ResolvedPackage::mark_leaf_used`].
///
/// Bug C12 fix surface — the inline emission in
/// [`crate::resolver::resolve_module`] was removed in favour of this
/// driver-orchestrated call so the lint sees the full "used" set.
pub fn emit_unused_import_lints(
    package: &ResolvedPackage,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) {
    let severity = lint_cfg.effective(edda_diag::DiagnosticClass::UnusedImport);
    for (idx, module) in package.modules.iter().enumerate() {
        let self_id = ModuleId::new(idx as u32);
        let used = match package.used_leaves.get(idx) {
            Some(set) => set,
            None => continue,
        };
        for leaf in module.leaf_imports.entries() {
            // B-014: synthetic self-aliases (`module a.b.c` auto-binds
            // leaf `c` to this module's own id) are never user-written
            // imports — they cannot be "unused" in any actionable sense
            // and have no source span to underline. Skip silently.
            if leaf.module == self_id {
                continue;
            }
            if used.contains(&leaf.leaf) {
                continue;
            }
            let name_text = cx.interner.resolve(leaf.leaf).to_string();
            let diag = edda_diag::Diagnostic::new(
                edda_diag::DiagnosticClass::UnusedImport,
                severity,
                leaf.source_span,
                format!("unused import `{name_text}`"),
            )
            .with_note("remove this import or reference its leaf name in the module");
            diags.push(diag);
        }
    }
}
