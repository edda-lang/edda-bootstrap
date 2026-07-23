//! Per-module item table — top-level Function / TypeDecl / Spec
//! bindings, with duplicate-decl detection.

use std::collections::HashMap;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_intern::{Interner, Symbol};
use edda_span::Span;
use edda_syntax::ast::{File, ItemKind, Visibility};

use crate::binding::{BindingEntry, BindingId, BindingKind};
use crate::graph::ModuleId;
use crate::resolve::ResolveCx;
use crate::spec_mangling::mangle_spec_invocation_name;

/// Top-level items declared in one module: every `function`,
/// `type`, and `spec` exposed at the module's surface, indexed by
/// their interned name.
#[derive(Clone, Debug, Default)]
pub struct ItemTable {
    entries: Vec<BindingEntry>,
    by_name: HashMap<Symbol, BindingId>,
}

impl ItemTable {
    /// Borrow the binding entries in declaration order.
    pub fn entries(&self) -> &[BindingEntry] {
        &self.entries
    }

    /// Look up a binding by its interned name.
    pub fn lookup(&self, name: Symbol) -> Option<BindingId> {
        self.by_name.get(&name).copied()
    }

    /// Borrow a binding entry by id. Panics if `id` does not belong
    /// to this table's module.
    pub fn get(&self, id: BindingId) -> &BindingEntry {
        &self.entries[id.index as usize]
    }

    /// Number of items in this module.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when the module declares no top-level items.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Build the [`ItemTable`] for one module. Duplicate top-level
/// declarations emit `import_resolution_error` (the class used here
/// covers all name-resolution failures during the import-resolve
/// pass; `build-system.md §8` reserves the broader class set) with
/// a secondary label at the earlier declaration.
pub fn build_item_table(
    module: ModuleId,
    file: &File,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> ItemTable {
    let mut table = ItemTable::default();
    for item in &file.items {
        if let ItemKind::Derive(derive) = &item.kind {
            register_derive_bindings(&mut table, module, derive, cx, diags, lint_cfg);
            continue;
        }
        let Some((kind, name, span, visibility)) = classify_item(&item.kind, cx.interner) else {
            continue;
        };
        if let Some(existing_id) = table.lookup(name) {
            let existing = table.get(existing_id);
            emit_duplicate_item(diags, lint_cfg, cx, name, span, existing.span);
            continue;
        }
        let id = BindingId::new(module, table.entries.len() as u32);
        table.entries.push(BindingEntry {
            kind,
            name,
            span,
            module,
            visibility,
        });
        table.by_name.insert(name, id);
    }
    table
}

fn register_derive_bindings(
    table: &mut ItemTable,
    module: ModuleId,
    derive: &edda_syntax::ast::Derive,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) {
    let target_leaf = match derive.target.segments.last() {
        Some(seg) if seg.name != Symbol::DUMMY => seg.name,
        _ => return,
    };
    let target_leaf_text = cx.interner.resolve(target_leaf).to_owned();
    for item in &derive.items {
        if item.name == Symbol::DUMMY {
            continue;
        }
        let item_text = cx.interner.resolve(item.name);
        if crate::derive_specs::derive_spec_target(item_text).is_none() {
            // Unknown derive vocab — the resolver's `walk_derive` will
            // emit `derive_unknown`; we skip the binding so downstream
            // code never sees a half-resolved entry for it.
            continue;
        }
        let mangled = format!("{item_text}_{target_leaf_text}");
        let name = cx.interner.intern(&mangled);
        if let Some(existing_id) = table.lookup(name) {
            let existing = table.get(existing_id);
            emit_duplicate_item(diags, lint_cfg, cx, name, item.span, existing.span);
            continue;
        }
        let id = BindingId::new(module, table.entries.len() as u32);
        table.entries.push(BindingEntry {
            kind: BindingKind::SpecInvocation,
            name,
            span: item.span,
            module,
            visibility: Visibility::Module,
        });
        table.by_name.insert(name, id);
        let _ = id;
    }
}

/// Extract `(kind, name, span, visibility)` for the item kinds that
/// produce a top-level binding. Returns `None` for Import / Module
/// items (those build the leaf-import table in `imports.rs`) and for
/// SpecInvocations whose argument shape the syntactic mangler does not
/// yet admit (the codegen-side argument lowering will surface the
/// diagnostic at the same site).
fn classify_item(
    kind: &ItemKind,
    interner: &Interner,
) -> Option<(BindingKind, Symbol, Span, Visibility)> {
    match kind {
        ItemKind::Function(f) => Some((BindingKind::Function, f.name.name, f.name.span, f.visibility)),
        ItemKind::TypeDecl(t) => Some((BindingKind::TypeDecl, t.name.name, t.name.span, t.visibility)),
        ItemKind::Spec(s) => Some((BindingKind::Spec, s.name.name, s.name.span, s.visibility)),
        ItemKind::Let(l) => Some((BindingKind::Const, l.name.name, l.name.span, l.visibility)),
        ItemKind::SpecInvocation(si) => {
            let name = mangle_spec_invocation_name(si, interner)?;
            Some((BindingKind::SpecInvocation, name, si.span, Visibility::Module))
        }
        ItemKind::Import(_) | ItemKind::Module(_) => None,
        // `derive` declarations produce no direct top-level binding; their
        // synthesised `spec std.<path>(<target>)` invocations get their
        // bindings through the SpecInvocation arm at codegen-expansion
        // time. Real resolver handling is implemented in `resolver.rs` (`walk_derive`).
        ItemKind::Derive(_) => None,
    }
}

fn emit_duplicate_item(
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
    cx: &ResolveCx<'_>,
    name: Symbol,
    span: Span,
    earlier_span: Span,
) {
    let class = DiagnosticClass::ImportResolutionError;
    let severity = lint_cfg.effective(class);
    let text = cx.interner.resolve(name);
    let diag = Diagnostic::new(
        class,
        severity,
        span,
        format!("duplicate top-level declaration `{text}`"),
    )
    .with_label(earlier_span, "first declared here")
    .with_note("each top-level name may only be declared once per module");
    diags.push(diag);
}
