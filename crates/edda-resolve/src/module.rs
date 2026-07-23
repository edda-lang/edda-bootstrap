//! Per-module resolution driver, stacked on the source-import graph's
//! [`ResolvedSourceGraph`].
//!
//! The top-level pass builds the [`ItemTable`] and [`ImportLeafTable`] per
//! module. The intra-function pass then walks each module's AST, declaring Param /
//! Local [`crate::BindingEntry`]s and resolving every Path AST node
//! to a [`Resolved`]. Both passes share the same [`ResolveCx`] and
//! diagnostic take.

use std::collections::{HashMap, HashSet};

use edda_diag::{Diagnostics, LintConfig};
use edda_intern::Symbol;

use crate::binding::{BindingEntry, BindingId};
use crate::graph::{ModuleEntry, ModuleId, ResolvedSourceGraph};
use crate::imports::{ImportLeafTable, build_import_leaf_table};
use crate::items::{ItemTable, build_item_table};
use crate::resolutions::Resolutions;
use crate::resolve::ResolveCx;
use crate::resolver::resolve_module;

/// Per-module resolution output: the top-level item table and the
/// resolved import leaf table. The intra-function pass layers lexical scopes
/// on top of these.
#[derive(Clone, Debug)]
pub struct ResolvedModule {
    /// Stable identifier within the source graph.
    pub id: ModuleId,
    /// Top-level Function / TypeDecl / Spec bindings.
    pub items: ItemTable,
    /// Per-module `import leaf -> ModuleId` map.
    pub leaf_imports: ImportLeafTable,
}

/// Combined resolution output. Owns the [`ResolvedSourceGraph`]
/// produced by `build_source_graph`, one [`ResolvedModule`] per
/// module entry, the appended Param / Local bindings per module,
/// the package-wide Path [`Resolutions`], and the per-module
/// "import-leaf was referenced" accounting that
/// [`emit_unused_import_lints`] consults.
#[derive(Clone, Debug)]
pub struct ResolvedPackage {
    graph: ResolvedSourceGraph,
    modules: Vec<ResolvedModule>,
    locals: Vec<Vec<BindingEntry>>,
    resolutions: Resolutions,
    used_leaves: Vec<HashSet<Symbol>>,
}

impl ResolvedPackage {
    /// Borrow the underlying source graph.
    pub fn graph(&self) -> &ResolvedSourceGraph {
        &self.graph
    }

    /// Borrow every per-module resolution entry.
    pub fn modules(&self) -> &[ResolvedModule] {
        &self.modules
    }

    /// Borrow one module's resolution entry by id.
    pub fn module(&self, id: ModuleId) -> &ResolvedModule {
        &self.modules[id.as_usize()]
    }

    /// Borrow the corresponding [`ModuleEntry`] from the source
    /// graph — convenience that pairs the source-graph view with the
    /// resolution view.
    pub fn module_entry(&self, id: ModuleId) -> &ModuleEntry {
        self.graph.module(id)
    }

    /// Borrow the span-keyed Path resolution map.
    pub fn resolutions(&self) -> &Resolutions {
        &self.resolutions
    }

    /// Borrow the Param / Local bindings appended to one module
    /// after the top-level items. Concatenated with
    /// [`ResolvedModule::items::entries`] this gives the module's
    /// full binding list addressed by [`crate::BindingId::index`].
    pub fn locals(&self, module: ModuleId) -> &[BindingEntry] {
        &self.locals[module.as_usize()]
    }

    /// Look up a binding by id. Routes between the top-level [`ItemTable`]
    /// (indices `0..items.len()`) and the intra-function locals
    /// (indices `items.len()..`).
    pub fn binding(&self, id: crate::BindingId) -> &BindingEntry {
        let m_idx = id.module.as_usize();
        let items = &self.modules[m_idx].items;
        let item_count = items.len();
        let raw = id.index as usize;
        if raw < item_count {
            &items.entries()[raw]
        } else {
            &self.locals[m_idx][raw - item_count]
        }
    }

    /// `true` when no modules were resolved.
    pub fn is_empty(&self) -> bool {
        self.modules.is_empty()
    }

    /// Number of resolved modules.
    pub fn len(&self) -> usize {
        self.modules.len()
    }

    /// Record that `module`'s import-leaf `leaf` was effectively
    /// referenced by some later pass (typecheck's method-call
    /// dispatch, intrinsic resolution, codegen).
    ///
    /// The resolver's own intra-module Path walk already populates
    /// this set for any leaf the user typed by name; this hook is the
    /// seam through which downstream passes report "the desugar
    /// routed through this import" so that
    /// [`emit_unused_import_lints`] does not over-fire (bug C12).
    pub fn mark_leaf_used(&mut self, module: ModuleId, leaf: Symbol) {
        let idx = module.as_usize();
        if idx >= self.used_leaves.len() {
            return;
        }
        self.used_leaves[idx].insert(leaf);
    }

    /// Borrow one module's used-leaf set — handy for diagnostics and
    /// tests that need to inspect what the resolver / later passes
    /// concluded about a module's imports.
    pub fn used_leaves(&self, module: ModuleId) -> Option<&HashSet<Symbol>> {
        self.used_leaves.get(module.as_usize())
    }
}

/// Drive per-module resolution over an already-built
/// [`ResolvedSourceGraph`]. Runs the top-level pass (item table + import-leaf
/// table) and the intra-function pass (lexical scope walker + Path resolution) for
/// every module in BFS order.
pub fn build_resolved_package(
    graph: ResolvedSourceGraph,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> ResolvedPackage {
    let modules = resolve_each_module(&graph, cx, diags, lint_cfg);
    let spec_inv_targets = build_spec_inv_targets(&graph, &modules, cx.interner);
    let module_aliases = build_module_aliases(&spec_inv_targets);
    let (locals, resolutions, used_leaves) = resolve_lexical(
        &graph,
        &modules,
        &spec_inv_targets,
        &module_aliases,
        cx,
        diags,
        lint_cfg,
    );
    ResolvedPackage {
        graph,
        modules,
        locals,
        resolutions,
        used_leaves,
    }
}

/// Invert the relevant half of `spec_inv_targets`: every generated module
/// whose canonical-path leaf is a SpecInvocation mangled name maps to its
/// alias TypeDecl. This lets the resolver dispatch qualified
/// `parent.generated_mod` paths through the spec-alias TypeDecl the same
/// way single-segment references already do.
fn build_module_aliases(
    spec_inv_targets: &HashMap<BindingId, (ModuleId, Option<BindingId>)>,
) -> HashMap<ModuleId, BindingId> {
    let mut out = HashMap::with_capacity(spec_inv_targets.len());
    for (_spec_inv_id, (gen_module_id, gen_typedecl_id)) in spec_inv_targets.iter() {
        if let Some(typedecl_id) = gen_typedecl_id {
            out.insert(*gen_module_id, *typedecl_id);
        }
    }
    out
}

/// Build a `spec_inv_binding_id → (generated_module_id, generated_typedecl_id)`
/// map. Each entry corresponds to one `spec Foo(args)` invocation that
/// has been monomorphised by codegen: the generated module hosts the
/// substituted spec body (with a top-level `type Foo` plus the spec's
/// functions); this map gives the resolver the binding ids needed to
/// route user-side references through that module.
///
/// Empty before the cascade-restart pass has populated the
/// source graph with generated modules; populated afterwards.
fn build_spec_inv_targets(
    graph: &ResolvedSourceGraph,
    modules: &[ResolvedModule],
    interner: &edda_intern::Interner,
) -> HashMap<BindingId, (ModuleId, Option<BindingId>)> {
    use edda_syntax::ast::ItemKind;
    let mut out = HashMap::new();
    for module_entry in graph.modules() {
        let resolved_mod = &modules[module_entry.id.as_usize()];
        // Build the import-leaf → "imported_module.canonical_path.Leaf" closure
        // once per source module. This is the resolver-side reciprocal of the
        // codegen-side `path_to_type_argument`'s import-leaf path: for a
        // single-segment arg like `HlirOp` imported from `hlir.op`, the codegen
        // side hashes against `"hlir.op.HlirOp"` (the fully-qualified TypeDecl
        // home-module path), so the resolver must produce the same string or
        // the hex disagrees and `find_and_record_generated_module` misses the
        // materialised module.
        // Resolve an alias / import-leaf segment to the imported module's
        // canonical_path string. The hex function appends the leaf itself
        // (for single-segment leaves) or the remaining segments
        // (for aliased multi-segment paths). This is the resolver-side
        // reciprocal of the codegen-side `path_to_type_argument`'s
        // resolution-map walk through alias prefixes.
        let resolve_imported_module = |alias: edda_intern::Symbol| -> Option<String> {
            let target_binding = resolved_mod.leaf_imports.lookup(alias)?;
            let target_entry_module = graph.module(target_binding.module);
            Some(target_entry_module.canonical_path.to_owned_string(interner))
        };
        for item in &module_entry.ast.items {
            match &item.kind {
                ItemKind::SpecInvocation(si) => {
                    // The mangled name identifies the spec_inv binding inside this module.
                    let Some(mangled) =
                        crate::spec_mangling::mangle_spec_invocation_name(si, interner)
                    else {
                        continue;
                    };
                    let Some(spec_inv_id) = resolved_mod.items.lookup(mangled) else {
                        continue;
                    };
                    // The spec's source leaf (e.g. `Box` for `spec std.alloc.Box(Expr)`).
                    let Some(spec_leaf) = si.path.segments.last().map(|s| s.name) else {
                        continue;
                    };
                    // Look for the generated module by its disambig-suffixed
                    // leaf first; fall back to leaf-only for backwards compat
                    // with caches written before the disambig-suffix fix.
                    // The generated module's last segment uses the codegen
                    // `mangle_short_name` form, where a spec-generated arg
                    // contributes its OWN generated leaf (with the inner
                    // disambig hex) — e.g. `Option_Box_HExpr_<innerhex>` —
                    // not the bare placeholder leaf `Option_Box_HExpr`. The
                    // placeholder binding stays under the bare `mangled`
                    // name (`spec_inv_id` above); only the candidate module
                    // leaf needs the generated form.
                    let gen_short = crate::spec_mangling::mangle_spec_invocation_generated_leaf(
                        si,
                        module_entry,
                        &resolved_mod.items,
                        interner,
                        &resolve_imported_module,
                    )
                    .unwrap_or(mangled);
                    let disambig_hex = crate::spec_mangling::module_disambig_hex_from_ast(
                        si,
                        module_entry,
                        &resolved_mod.items,
                        interner,
                        &resolve_imported_module,
                    );
                    let disambig_leaf = disambig_hex.as_ref().map(|hex| {
                        interner.intern(&format!("{}_{hex}", interner.resolve(gen_short)))
                    });
                    if find_and_record_generated_module(
                        graph,
                        modules,
                        disambig_leaf,
                        gen_short,
                        spec_leaf,
                        spec_inv_id,
                        &mut out,
                    ) {
                        continue;
                    }
                }
                ItemKind::Derive(d) => {
                    let target_leaf = match d.target.segments.last() {
                        Some(seg) if seg.name != edda_intern::Symbol::DUMMY => seg.name,
                        _ => continue,
                    };
                    let target_leaf_text = interner.resolve(target_leaf).to_owned();
                    // Synthesise a single-arg expression for the derive target
                    // so the disambig-hash sees the same canonical input the
                    // codegen-side produces from `derive_target_argument`
                    // (which goes through `path_to_type_argument` and yields
                    // `Argument::Type(<source_module>.<target_leaf>)` for a
                    // local typedecl target).
                    let synthetic_target_expr = edda_syntax::ast::Expr {
                        span: d.target.span,
                        kind: edda_syntax::ast::ExprKind::Path(d.target.clone()),
                    };
                    let synthetic_args = [synthetic_target_expr];
                    for derive_item in &d.items {
                        if derive_item.name == edda_intern::Symbol::DUMMY {
                            continue;
                        }
                        let item_text = interner.resolve(derive_item.name);
                        if crate::derive_specs::derive_spec_target(item_text).is_none() {
                            continue;
                        }
                        let mangled_text = format!("{item_text}_{target_leaf_text}");
                        let mangled = interner.intern(&mangled_text);
                        let Some(spec_inv_id) = resolved_mod.items.lookup(mangled) else {
                            continue;
                        };
                        let spec_leaf = derive_item.name;
                        let disambig_hex = crate::spec_mangling::module_disambig_hex_for_args(
                            item_text,
                            &synthetic_args,
                            module_entry,
                            &resolved_mod.items,
                            interner,
                            &resolve_imported_module,
                        );
                        let disambig_leaf = disambig_hex.as_ref().map(|hex| {
                            interner.intern(&format!("{mangled_text}_{hex}"))
                        });
                        if find_and_record_generated_module(
                            graph,
                            modules,
                            disambig_leaf,
                            mangled,
                            spec_leaf,
                            spec_inv_id,
                            &mut out,
                        ) {
                            continue;
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

/// Look up a generated module by candidate last-segment and record the
/// spec_inv → (module_id, typedecl_id?) mapping in `out`. Returns `true`
/// if the generated module was found (either with the disambig suffix or
/// leaf-only), regardless of whether it carries an eponymous typedecl.
fn find_and_record_generated_module(
    graph: &ResolvedSourceGraph,
    modules: &[ResolvedModule],
    disambig_leaf: Option<edda_intern::Symbol>,
    leaf_only: edda_intern::Symbol,
    spec_leaf: edda_intern::Symbol,
    spec_inv_id: BindingId,
    out: &mut HashMap<BindingId, (ModuleId, Option<BindingId>)>,
) -> bool {
    if let Some(target_leaf) = disambig_leaf {
        for candidate in graph.modules() {
            if candidate.canonical_path.last() != target_leaf {
                continue;
            }
            // The precise disambig-suffixed leaf unambiguously identifies
            // the generated module — record the module link even when it
            // has no eponymous typedecl (a function-only spec), so member
            // calls dispatch through it.
            let candidate_mod = &modules[candidate.id.as_usize()];
            let typedecl_id = candidate_mod.items.lookup(spec_leaf);
            out.insert(spec_inv_id, (candidate.id, typedecl_id));
            return true;
        }
    }
    for candidate in graph.modules() {
        if candidate.canonical_path.last() != leaf_only {
            continue;
        }
        let candidate_mod = &modules[candidate.id.as_usize()];
        let typedecl_id = candidate_mod.items.lookup(spec_leaf);
        out.insert(spec_inv_id, (candidate.id, typedecl_id));
        return true;
    }
    false
}

fn resolve_each_module(
    graph: &ResolvedSourceGraph,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Vec<ResolvedModule> {
    let mut out = Vec::with_capacity(graph.len());
    for entry in graph.modules() {
        let items = build_item_table(entry.id, &entry.ast, cx, diags, lint_cfg);
        let leaf_imports = build_import_leaf_table(
            entry.id,
            &entry.ast,
            &entry.canonical_path,
            &entry.file_path,
            graph,
            cx,
            diags,
            lint_cfg,
        );
        out.push(ResolvedModule {
            id: entry.id,
            items,
            leaf_imports,
        });
    }
    out
}

fn resolve_lexical(
    graph: &ResolvedSourceGraph,
    modules: &[ResolvedModule],
    spec_inv_targets: &HashMap<BindingId, (ModuleId, Option<BindingId>)>,
    module_aliases: &HashMap<ModuleId, BindingId>,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> (Vec<Vec<BindingEntry>>, Resolutions, Vec<HashSet<Symbol>>) {
    let n = graph.len();
    let mut all_locals: Vec<Vec<BindingEntry>> = (0..n).map(|_| Vec::new()).collect();
    let mut all_used: Vec<HashSet<Symbol>> = (0..n).map(|_| HashSet::new()).collect();
    let mut resolutions = Resolutions::new();
    for entry in graph.modules() {
        let (locals, paths, used) = resolve_module(
            entry.id,
            graph,
            modules,
            spec_inv_targets,
            module_aliases,
            cx,
            diags,
            lint_cfg,
        );
        all_locals[entry.id.as_usize()] = locals;
        all_used[entry.id.as_usize()] = used;
        for (span, r) in paths {
            resolutions.insert(span, r);
        }
    }
    (all_locals, resolutions, all_used)
}


mod lints;

pub use lints::{
    emit_binding_should_be_let_lints, emit_capability_safe_stdlib_lints,
    emit_dead_private_function_lints, emit_duplicate_spec_invocation_lints,
    emit_exec_scope_without_spawn_lints, emit_mode_overgrab_lints, emit_trust_budget_lints,
    emit_trust_hatch_too_dense_lints, emit_trust_points_listing,
    emit_unused_closure_capture_lints, emit_unused_import_lints,
};
