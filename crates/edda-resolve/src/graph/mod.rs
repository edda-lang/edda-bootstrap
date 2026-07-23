//! Source-import graph — the parse-and-resolve pass per
//! `build-system.md §2`.
//!
//! Starting from a set of entry `.ea` files, the driver parses
//! each file, walks every `import` statement (resolved via
//! [`resolve_import_path`]), and enqueues the imported file for the
//! next iteration. The result is a [`ResolvedSourceGraph`] —
//! ModuleId-indexed entries, an adjacency list of imports, and a
//! topological order; module cycles are rejected with `import_cycle`
//! diagnostics, and two files resolving to the same canonical module
//! path are rejected with `import_resolution_error`.

mod driver;
mod topo;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_span::{BytePos, FileId, SourceMap, Span};
use edda_syntax::ast::{File, ItemKind, ModuleDecl};

use crate::loader::SourceLoader;
use crate::path::ModulePath;
use crate::resolve::ResolveCx;

use driver::GraphDriver;

/// Dense, monotonically-assigned identifier for a module within a
/// single [`ResolvedSourceGraph`].
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash, Debug)]
pub struct ModuleId(u32);

impl ModuleId {
    /// Construct from a raw `u32`.
    ///
    /// Production code obtains [`ModuleId`]s by registering modules in
    /// [`build_source_graph`], which assigns IDs in BFS discovery order
    /// — this constructor is the escape hatch for downstream consumers
    /// (test scaffolding, parallel `BindingId`-keyed indexes such as
    /// `edda-types::TyCx`) that need to mint a handle without driving
    /// the resolver.
    pub fn new(value: u32) -> Self {
        Self(value)
    }

    /// Raw `u32` representation.
    pub fn as_u32(self) -> u32 {
        self.0
    }

    /// Index into a parallel `Vec` keyed by [`ModuleId`].
    pub fn as_usize(self) -> usize {
        self.0 as usize
    }
}

/// One resolved module within the source graph: its filesystem
/// location, its canonical [`ModulePath`], and the parsed AST.
#[derive(Clone, Debug)]
pub struct ModuleEntry {
    /// Stable identifier within the graph.
    pub id: ModuleId,
    /// File-level handle into the [`SourceMap`].
    pub file_id: FileId,
    /// On-disk path that produced this entry.
    pub file_path: PathBuf,
    /// Canonical dot-path the module is exposed under.
    pub canonical_path: ModulePath,
    /// Parsed AST. Wrapped in [`Arc`] so downstream passes can share
    /// `&File` cheaply across threads (Charter Article VIII; the
    /// daemon needs to hand AST references into parallel worker
    /// jobs).
    pub ast: Arc<File>,
    /// `true` when [`canonical_path`] was derived from a `module`
    /// keyword override (`declarations.md §286`) rather than from
    /// filesystem position.
    pub overrides_path: bool,
}

/// The source-import graph driver's output. Owns every parsed AST plus the directed-acyclic
/// import edges and a topological order; downstream passes (per-module
/// resolution and the typecheck pass) consume this.
#[derive(Clone, Debug)]
pub struct ResolvedSourceGraph {
    modules: Vec<ModuleEntry>,
    by_path: HashMap<ModulePath, ModuleId>,
    by_file: HashMap<FileId, ModuleId>,
    by_file_path: HashMap<PathBuf, ModuleId>,
    imports: Vec<Vec<ModuleId>>,
    topo_order: Vec<ModuleId>,
}

impl ResolvedSourceGraph {
    /// All resolved modules, in BFS discovery order.
    pub fn modules(&self) -> &[ModuleEntry] {
        &self.modules
    }

    /// Borrow a module by id.
    pub fn module(&self, id: ModuleId) -> &ModuleEntry {
        &self.modules[id.as_usize()]
    }

    /// Look up a module by its canonical path.
    pub fn lookup_by_path(&self, path: &ModulePath) -> Option<ModuleId> {
        self.by_path.get(path).copied()
    }

    /// Look up a module by the [`FileId`] under which it was
    /// registered in the [`SourceMap`].
    pub fn lookup_by_file(&self, file: FileId) -> Option<ModuleId> {
        self.by_file.get(&file).copied()
    }

    /// Look up a module by its on-disk file path. Used as a fallback
    /// by import-edge resolution when the import-derived canonical
    /// path (e.g. `local.<leaf>` rewritten through the importer's
    /// `root_namespace`) does not match the target file's actual
    /// canonical path because the target file declared its identity
    /// with a `module` keyword override (`declarations.md §286`).
    pub fn lookup_by_file_path(&self, path: &Path) -> Option<ModuleId> {
        self.by_file_path.get(&canonical_key(path)).copied()
    }

    /// Adjacency list — modules that `id` imports, in source order.
    pub fn imports_of(&self, id: ModuleId) -> &[ModuleId] {
        &self.imports[id.as_usize()]
    }

    /// Topological order over the import graph. Cycles, if any, are
    /// emitted as `import_cycle` diagnostics; their members still
    /// appear in this order (with one arbitrarily-chosen position
    /// per cycle member).
    pub fn topo_order(&self) -> &[ModuleId] {
        &self.topo_order
    }

    /// Number of resolved modules.
    pub fn len(&self) -> usize {
        self.modules.len()
    }

    /// `true` when no module was resolved (every entry file failed
    /// to load, or none were supplied).
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }
}

/// Build the source-import graph rooted at `entry_files`. The driver
/// loads each entry, parses it, resolves every `import` statement via
/// the path-resolution rules, and recursively enqueues the resulting files; loops on the
/// resulting graph are reported and the topological order is computed
/// over the residual DAG.
pub fn build_source_graph(
    entry_files: &[PathBuf],
    cx: &ResolveCx<'_>,
    loader: &dyn SourceLoader,
    source_map: &SourceMap,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> ResolvedSourceGraph {
    let mut driver = GraphDriver::new(cx, loader, source_map, diags, lint_cfg);
    for path in entry_files {
        driver.enqueue(path.clone());
    }
    driver.drive();
    driver.finalize()
}

pub(super) fn find_module_decl(parsed: &File) -> Option<&ModuleDecl> {
    parsed.items.iter().find_map(|item| match &item.kind {
        ItemKind::Module(m) => Some(m),
        _ => None,
    })
}

pub(super) fn whole_file_span(file_id: FileId, parsed: &File) -> Span {
    if parsed.span.is_dummy() {
        Span::new(file_id, BytePos(0), BytePos(0))
    } else {
        parsed.span
    }
}

pub(super) fn canonical_key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

pub(super) fn emit_diag(
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
    class: DiagnosticClass,
    span: Span,
    message: String,
    note: &'static str,
) {
    let severity = lint_cfg.effective(class);
    diags.push(Diagnostic::new(class, severity, span, message).with_note(note));
}
