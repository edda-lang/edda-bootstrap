//! Per-module import leaf-name table.
//!
//! Per `declarations.md §301`, an imported module is accessed in
//! source by its *leaf name* — the last segment of the import's dot
//! path, or the bare-leaf segment for sibling-leaf imports. This
//! module builds the per-module `leaf → ModuleId` map that the
//! intra-function resolver consults when resolving free identifiers
//! in expressions and type references.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_intern::Symbol;
use edda_span::Span;
use edda_syntax::ast::{File, Import, ItemKind};

use crate::graph::{ModuleId, ResolvedSourceGraph};
use crate::layout::owning_layout_for_file;
use crate::path::ModulePath;
use crate::resolve::{is_recovery_path, ResolveCx};

/// File extension of an Edda source file. Lowercase; mirrors the
/// `EDDA_EXT` constant inside `crate::resolve` — kept separate so this
/// module does not have to expose it across the crate boundary.
const EDDA_EXT: &str = "ea";
const SRC_DIR: &str = "src";

/// One resolved import — the source-level leaf name plus the
/// [`ModuleId`] it points at.
#[derive(Copy, Clone, Debug)]
pub struct ImportLeafBinding {
    /// Source-level leaf name (last segment of the import path, an
    /// `as <alias>` name, or — for `is_selected` entries — one of the
    /// `.{name, ...}` selected names).
    pub leaf: Symbol,
    /// Resolved target module in the source graph. For `is_selected`
    /// entries this is the module `leaf` lives INSIDE, not a module
    /// `leaf` itself names.
    pub module: ModuleId,
    /// Span of the original `import` statement (or, for a selected
    /// name, that name's own span).
    pub source_span: Span,
    /// `true` when this entry came from a `.{...}` selected-name
    /// import — `leaf` then resolves directly to an item inside
    /// `module` (`resolve_cross_module_item`) rather than to `module`
    /// itself (`Resolved::Module`).
    pub is_selected: bool,
}

/// Per-module mapping from import leaf name to resolved target
/// module. Built by [`build_import_leaf_table`] over a module's
/// `import` items, deduplicated by leaf name.
#[derive(Clone, Debug, Default)]
pub struct ImportLeafTable {
    entries: Vec<ImportLeafBinding>,
    by_leaf: HashMap<Symbol, usize>,
}

impl ImportLeafTable {
    /// Borrow every leaf binding in declaration order.
    pub fn entries(&self) -> &[ImportLeafBinding] {
        &self.entries
    }

    /// Look up the [`ModuleId`] that `leaf` resolves to.
    pub fn lookup(&self, leaf: Symbol) -> Option<&ImportLeafBinding> {
        self.by_leaf.get(&leaf).map(|i| &self.entries[*i])
    }

    /// Number of registered leaves.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when the module declares no imports.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Build the [`ImportLeafTable`] for one module by walking its
/// `import` items, mapping each to the target [`ModuleId`] via the
/// already-built source graph. Imports whose target failed to
/// resolve during source-graph construction are silently skipped
/// (the error was emitted there); imports of the same canonical path more than
/// once in the same file emit `duplicate_import` (warning, lint E);
/// imports whose leaf collides with an earlier import in the same
/// module emit `import_resolution_error`.
pub fn build_import_leaf_table(
    module: ModuleId,
    file: &File,
    importer_module: &ModulePath,
    importer_file: &Path,
    graph: &ResolvedSourceGraph,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> ImportLeafTable {
    let mut table = ImportLeafTable::default();
    let mut seen_paths: HashMap<ModulePath, Span> = HashMap::new();
    for item in &file.items {
        let ItemKind::Import(import) = &item.kind else {
            continue;
        };
        // Parser-recovery imports carry a sentinel path (empty or
        // `Symbol::DUMMY`-headed) and have already produced a
        // `parse_error`; skip them so we neither panic on the
        // segment access below nor duplicate the diagnostic.
        if is_recovery_path(&import.path) {
            continue;
        }
        let (canonical, expected_file) =
            canonical_path_of_import(import, importer_module, importer_file, cx);
        let target_id = graph.lookup_by_path(&canonical).or_else(|| {
            expected_file
                .as_deref()
                .and_then(|f| graph.lookup_by_file_path(f))
        });
        let Some(target_id) = target_id else {
            // Source-graph construction emitted the resolution error already.
            continue;
        };
        if let Some(&earlier_span) = seen_paths.get(&canonical) {
            // Same canonical path imported twice in the same file:
            // the second import has no effect (the leaf is already
            // bound). Emit duplicate_import and skip the redundant
            // entry — also skips the leaf-collision check below so
            // we don't double-diagnose the trivially-shared leaf.
            emit_duplicate_import(diags, lint_cfg, cx, &canonical, import.span, earlier_span);
            continue;
        }
        seen_paths.insert(canonical.clone(), import.span);
        // Selected-name form (`import <path>.{name, ...}`):
        // register one binding PER selected name, each resolving
        // directly to an item inside `target_id` — never the single
        // path-leaf/alias qualifier entry the `else` arm below
        // registers. The parser already rejects `.{...}` combined with
        // `as <alias>`, so `import.alias` is never `Some` here.
        if let Some(selection) = &import.selection {
            for name in selection {
                register_leaf(
                    &mut table,
                    diags,
                    lint_cfg,
                    cx,
                    name.name,
                    target_id,
                    name.span,
                    true,
                );
            }
            continue;
        }
        let leaf = leaf_of_import(import);
        register_leaf(
            &mut table,
            diags,
            lint_cfg,
            cx,
            leaf,
            target_id,
            import.span,
            false,
        );
    }
    // B-014: bind the file's own canonical leaf as a self-alias so a
    // path-qualified self-reference like `err: alloc.AllocError` inside
    // `std.mem.alloc/src/alloc.ea` resolves through the leaf-import
    // path back to the file's own items. The alias is added LAST and
    // skipped silently when a real import already took the leaf (the
    // user explicitly chose to shadow themselves). DUMMY source-span so
    // the unused-import lint suppression in `emit_unused_import_lints`
    // can identify synthetic self-aliases by their `module == self` test.
    if let Some(self_leaf_sym) = importer_module.segments().last().copied() {
        if !table.by_leaf.contains_key(&self_leaf_sym) {
            let idx = table.entries.len();
            table.entries.push(ImportLeafBinding {
                leaf: self_leaf_sym,
                module,
                source_span: Span::DUMMY,
                is_selected: false,
            });
            table.by_leaf.insert(self_leaf_sym, idx);
        }
    }
    table
}

#[allow(clippy::too_many_arguments)]
fn register_leaf(
    table: &mut ImportLeafTable,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
    cx: &ResolveCx<'_>,
    leaf: Symbol,
    target_id: ModuleId,
    span: Span,
    is_selected: bool,
) {
    if let Some(existing) = table.by_leaf.get(&leaf).copied() {
        emit_duplicate_leaf(diags, lint_cfg, cx, leaf, span, table.entries[existing].source_span);
        return;
    }
    let idx = table.entries.len();
    table.entries.push(ImportLeafBinding {
        leaf,
        module: target_id,
        source_span: span,
        is_selected,
    });
    table.by_leaf.insert(leaf, idx);
}

/// Compute the canonical [`ModulePath`] an `import` statement
/// resolves to, plus — for filesystem-position-derived imports — the
/// expected on-disk path of the target file. Mirrors the path-resolution
/// rules' `resolve_sibling_leaf` / `resolve_dot_path` canonical computation
/// without re-emitting diagnostics.
fn canonical_path_of_import(
    import: &Import,
    importer_module: &ModulePath,
    importer_file: &Path,
    cx: &ResolveCx<'_>,
) -> (ModulePath, Option<PathBuf>) {
    if import.path.segments.len() == 1 {
        let leaf = import.path.segments[0].name;
        let canonical = match importer_module.parent() {
            Some(parent) => parent.push(leaf),
            None => ModulePath::new(vec![importer_module.first(), leaf]),
        };
        let leaf_text = cx.interner.resolve(leaf);
        let importer_dir = importer_file.parent().unwrap_or(importer_file);
        let expected = importer_dir.join(format!("{leaf_text}.{EDDA_EXT}"));
        (canonical, Some(expected))
    } else {
        let raw = ModulePath::from_ast(&import.path);
        // `local.<path>` is sugar for `<owning_root_namespace>.<path>`.
        if cx.interner.resolve(raw.first()) == "local" {
            let owning_dir = importer_file.parent().unwrap_or(importer_file);
            let owning = owning_layout_for_file(owning_dir, cx.layout, cx.deps, cx.stdlib);
            let mut segs: Vec<Symbol> = owning.canonical_root_path.segments().to_vec();
            segs.extend_from_slice(&raw.segments()[1..]);
            let canonical = ModulePath::new(segs);
            let mut expected = owning.root_dir.clone();
            expected.push(SRC_DIR);
            let rest = &raw.segments()[1..];
            for sym in &rest[..rest.len() - 1] {
                expected.push(cx.interner.resolve(*sym));
            }
            let leaf = rest.last().expect("local.<path> has ≥ 1 segment after `local`");
            expected.push(format!("{}.{EDDA_EXT}", cx.interner.resolve(*leaf)));
            (canonical, Some(expected))
        } else {
            (raw, None)
        }
    }
}

/// Source-level name an `import` introduces into the importer's
/// leaf scope. Prefers the `as <ident>` alias when present and
/// falls back to the last segment of the imported path.
fn leaf_of_import(import: &Import) -> Symbol {
    if let Some(alias) = &import.alias {
        return alias.name;
    }
    import
        .path
        .segments
        .last()
        .expect("AST: Path has ≥ 1 segment")
        .name
}

fn emit_duplicate_leaf(
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
    cx: &ResolveCx<'_>,
    leaf: Symbol,
    span: Span,
    earlier_span: Span,
) {
    let class = DiagnosticClass::ImportResolutionError;
    let severity = lint_cfg.effective(class);
    let text = cx.interner.resolve(leaf);
    let diag = Diagnostic::new(
        class,
        severity,
        span,
        format!("duplicate import leaf `{text}`"),
    )
    .with_label(earlier_span, "first imported here")
    .with_note("each import leaf name may only be introduced once per module");
    diags.push(diag);
}

fn emit_duplicate_import(
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
    cx: &ResolveCx<'_>,
    path: &ModulePath,
    span: Span,
    earlier_span: Span,
) {
    let class = DiagnosticClass::DuplicateImport;
    let severity = lint_cfg.effective(class);
    let path_text = path.to_owned_string(cx.interner);
    let diag = Diagnostic::new(
        class,
        severity,
        span,
        format!("duplicate `import {path_text}` in this file"),
    )
    .with_label(earlier_span, "first imported here")
    .with_note("the second import has no effect; remove this line");
    diags.push(diag);
}
