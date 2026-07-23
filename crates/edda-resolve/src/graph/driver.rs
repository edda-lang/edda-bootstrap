//! BFS source-graph driver — loads, parses, and import-resolves each
//! `.ea` file reachable from the entry roots, registering one
//! [`ModuleEntry`] per file and recording the import edges that
//! `finalize` resolves into the adjacency list + topological order.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use edda_diag::{DiagnosticClass, Diagnostics, LintConfig};
use edda_span::{FileId, SourceMap, Span};
use edda_syntax::ast::{File, ItemKind};
use edda_syntax::{lex, parse_file};

use crate::layout::ImporterContext;
use crate::loader::SourceLoader;
use crate::path::ModulePath;
use crate::resolve::{ResolveCx, module_identity_for_file, resolve_import_path};

use super::topo::{build_adjacency, emit_import_cycle, topo_or_cycles};
use super::{
    ModuleEntry, ModuleId, ResolvedSourceGraph, canonical_key, emit_diag, find_module_decl,
    whole_file_span,
};

pub(super) struct GraphDriver<'a, 'i> {
    cx: &'a ResolveCx<'i>,
    loader: &'a dyn SourceLoader,
    source_map: &'a SourceMap,
    diags: &'a mut Diagnostics,
    lint_cfg: &'a LintConfig,
    queue: VecDeque<PathBuf>,
    seen_files: HashSet<PathBuf>,
    modules: Vec<ModuleEntry>,
    by_path: HashMap<ModulePath, ModuleId>,
    by_file: HashMap<FileId, ModuleId>,
    by_file_path: HashMap<PathBuf, ModuleId>,
    pending_edges: Vec<(ModuleId, ModulePath, PathBuf)>,
}

impl<'a, 'i> GraphDriver<'a, 'i> {
    pub(super) fn new(
        cx: &'a ResolveCx<'i>,
        loader: &'a dyn SourceLoader,
        source_map: &'a SourceMap,
        diags: &'a mut Diagnostics,
        lint_cfg: &'a LintConfig,
    ) -> Self {
        Self {
            cx,
            loader,
            source_map,
            diags,
            lint_cfg,
            queue: VecDeque::new(),
            seen_files: HashSet::new(),
            modules: Vec::new(),
            by_path: HashMap::new(),
            by_file: HashMap::new(),
            by_file_path: HashMap::new(),
            pending_edges: Vec::new(),
        }
    }

    pub(super) fn enqueue(&mut self, path: PathBuf) {
        self.queue.push_back(path);
    }

    pub(super) fn drive(&mut self) {
        while let Some(path) = self.queue.pop_front() {
            self.visit(path);
        }
    }

    fn visit(&mut self, path: PathBuf) {
        let dedup_key = canonical_key(&path);
        if !self.seen_files.insert(dedup_key) {
            return;
        }

        let content = match self.loader.load(&path) {
            Ok(c) => c,
            Err(err) => {
                self.emit_resolution_error(
                    Span::DUMMY,
                    format!("failed to load `{}`: {err}", path.display()),
                    "the resolver expected this file to be readable",
                );
                return;
            }
        };

        let file_id = self.source_map.add_file(path.clone(), content);
        let parsed = self.parse_file(file_id);
        let module_decl = find_module_decl(&parsed);
        let file_span = whole_file_span(file_id, &parsed);
        let canonical = match module_identity_for_file(
            &path,
            module_decl,
            file_span,
            self.cx,
            self.diags,
            self.lint_cfg,
        ) {
            Some(c) => c,
            None => return,
        };

        if let Some(existing_id) = self.by_path.get(&canonical).copied() {
            self.report_collision(file_span, &canonical, existing_id);
            return;
        }

        let module_id = ModuleId::new(self.modules.len() as u32);
        let overrides_path = module_decl.is_some();
        self.collect_imports(&parsed, &path, &canonical, module_id);
        self.register_module(module_id, file_id, path, canonical, parsed, overrides_path);
    }

    fn parse_file(&mut self, file_id: FileId) -> File {
        let content = self.source_map.file_content(file_id);
        // Generated codegen artifacts begin with a `// @generated` comment
        // header (`edda_cache::ArtifactHeader`). The source lexer rejects
        // comments per the V1.0 no-comment design lock, so blank that
        // leading header block before lexing — the bootstrap analogue of the native compiler's
        // `cache.header` body-offset strip. Hand-authored `.ea` carries no
        // such header, so its comments still surface as `comment_not_admitted`.
        let stripped = Self::strip_generated_header(content);
        let lex_input = stripped.as_deref().unwrap_or(content);
        let tokens = lex(lex_input, file_id, self.cx.interner, self.diags, self.lint_cfg);
        parse_file(&tokens, self.cx.interner, self.diags, self.lint_cfg)
    }

    /// If `content` is a generated codegen artifact (starts with
    /// `// @generated`), return a copy with its leading consecutive
    /// `//`-comment header block blanked to spaces (newlines preserved);
    /// otherwise `None`. Length- and newline-preserving so lexer spans stay
    /// byte-accurate against the original source registered in the
    /// `SourceMap`.
    fn strip_generated_header(content: &str) -> Option<String> {
        const MARKER: &str = "// @generated";
        if !content.starts_with(MARKER) {
            return None;
        }
        // End of the leading run of `//`-comment lines (the header block).
        let mut offset = 0usize;
        for line in content.split_inclusive('\n') {
            if line.trim_start().starts_with("//") {
                offset += line.len();
            } else {
                break;
            }
        }
        // Blank every non-newline byte before `offset`. The blanked region
        // spans whole comment lines (offset sits on a line boundary), so no
        // multibyte char is partially overwritten — the result is valid UTF-8.
        let mut out: Vec<u8> = content.as_bytes().to_vec();
        for b in &mut out[..offset] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
        String::from_utf8(out).ok()
    }

    fn collect_imports(
        &mut self,
        parsed: &File,
        file_path: &Path,
        canonical: &ModulePath,
        module_id: ModuleId,
    ) {
        let importer_dir = file_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let importer = ImporterContext {
            importer_dir,
            importer_module: canonical.clone(),
        };
        for item in &parsed.items {
            match &item.kind {
                ItemKind::Import(import) => {
                    let Some(resolved) = resolve_import_path(
                        import,
                        &importer,
                        self.cx,
                        self.diags,
                        self.lint_cfg,
                    ) else {
                        continue;
                    };
                    self.queue.push_back(resolved.expected_file.clone());
                    self.pending_edges
                        .push((module_id, resolved.canonical, resolved.expected_file));
                }
                ItemKind::Derive(derive) => {
                    self.enqueue_derive_spec_imports(derive, module_id);
                }
                _ => {}
            }
        }
    }

    fn enqueue_derive_spec_imports(
        &mut self,
        derive: &edda_syntax::ast::Derive,
        module_id: ModuleId,
    ) {
        for item in &derive.items {
            if item.name == edda_intern::Symbol::DUMMY {
                continue;
            }
            let name_text = self.cx.interner.resolve(item.name);
            let Some(target) = crate::derive_specs::derive_spec_target(name_text) else {
                continue;
            };
            let mut segs = Vec::with_capacity(target.module_segments.len());
            for s in target.module_segments {
                segs.push(self.cx.interner.intern(s));
            }
            let canonical = ModulePath::new(segs);
            let Some(file) = self.cx.stdlib.get(&canonical) else {
                continue;
            };
            self.queue.push_back(file.clone());
            self.pending_edges.push((module_id, canonical, file.clone()));
        }
    }

    fn register_module(
        &mut self,
        module_id: ModuleId,
        file_id: FileId,
        file_path: PathBuf,
        canonical: ModulePath,
        parsed: File,
        overrides_path: bool,
    ) {
        self.by_path.insert(canonical.clone(), module_id);
        self.by_file.insert(file_id, module_id);
        self.by_file_path
            .insert(canonical_key(&file_path), module_id);
        self.modules.push(ModuleEntry {
            id: module_id,
            file_id,
            file_path,
            canonical_path: canonical,
            ast: Arc::new(parsed),
            overrides_path,
        });
    }

    fn report_collision(&mut self, file_span: Span, canonical: &ModulePath, existing_id: ModuleId) {
        let existing = &self.modules[existing_id.as_usize()];
        let path = canonical.display(self.cx.interner).to_string();
        let existing_path = existing.file_path.display().to_string();
        self.emit_resolution_error(
            file_span,
            format!("module-path collision: `{path}` is already provided by `{existing_path}`"),
            "two files cannot resolve to the same canonical module path",
        );
    }

    fn emit_resolution_error(&mut self, span: Span, message: String, note: &'static str) {
        emit_diag(
            self.diags,
            self.lint_cfg,
            DiagnosticClass::ImportResolutionError,
            span,
            message,
            note,
        );
    }

    pub(super) fn finalize(self) -> ResolvedSourceGraph {
        let imports = build_adjacency(
            &self.pending_edges,
            &self.by_path,
            &self.by_file_path,
            self.modules.len(),
        );
        let (topo_order, cycles) = topo_or_cycles(&imports);
        for cycle in &cycles {
            emit_import_cycle(self.diags, self.lint_cfg, &self.modules, cycle, self.cx.interner);
        }
        ResolvedSourceGraph {
            modules: self.modules,
            by_path: self.by_path,
            by_file: self.by_file,
            by_file_path: self.by_file_path,
            imports,
            topo_order,
        }
    }
}
