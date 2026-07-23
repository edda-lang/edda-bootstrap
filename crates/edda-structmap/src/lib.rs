//! Compiler-emitted structure map — codex `06-tooling.md` §5 `schema_version = 4`.
//!
//! For every directory containing Edda source the compiler writes one
//! `index.toon` file. The root file is a project-wide index linking
//! downward through `children[]`; each intermediate file carries a
//! `parent` header pointing upward. Item-level tables (`types[]`,
//! `functions[]`, `invariants[]`, `patterns[]`, `trust_points[]`) are
//! populated from the resolver + typecheck output rather than from
//! comment scraping — Edda's syntax already carries every structural
//! fact the map needs (codex §5.3 "Why this lives in the compiler").
//! The doc-comment tables (`doc_high[]` / `doc_med[]` / `doc_low[]`)
//! were retired at `schema_version = 4` (D-17):
//! the locked V1.0 design rule forbids comments
//! in `.ea` source, so those tables had no source to derive from.
//!
//! The emitted filename (`index.toon`) deliberately diverges from the
//! language-agnostic structure map an external comment-scraper produces
//! for non-Edda code, so the two protocols coexist in the same tree
//! without ambiguity: the external scan is any-language and comment-
//! derived, `index.toon` is Edda-compiler-native and fact-derived.
//!
//! # Public surface
//!
//! - [`emit`] — entry point. Takes an [`EmitInput`] (project metadata
//!   plus the resolved + typed package) and returns an [`EmitOutput`]
//!   mapping every emitted file's absolute path to its TOON contents.
//!
//! # Reusability
//!
//! The emitter is a pure transform with no I/O of its own. `edda-driver`
//! wraps it for the batch `edda structmap` verb; `edda-daemon` will
//! later wrap it for the `inspect.structmap_for_directory` MCP query.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use edda_intern::Interner;
use edda_resolve::ResolvedPackage;
use edda_span::SourceMap;
use edda_types::TypedPackage;
use edda_types::TyInterner;

mod analyze;
mod budget;
mod cluster;
mod cohesion;
mod density;
mod emit;
mod model;
mod token;
mod walk;

pub use budget::{
    Band, BudgetConfig, BudgetReport, DEFAULT_MODEL_CALIBRATION, Directive, FatFile, Gate,
    MarginalContributor, NodeMeasure, WorklistEntry, analyze_budget,
};
pub use cluster::{ClusterReport, FilenameCluster};
pub use cohesion::{
    FileCohesionReport, LOC_FLOOR as FILE_COHESION_LOC_FLOOR,
    MIN_COMPONENT_COUNT as FILE_COHESION_MIN_COMPONENT_COUNT,
    MIN_COMPONENT_SIZE as FILE_COHESION_MIN_COMPONENT_SIZE,
};
pub use density::{DEFAULT_DENSITY_WARN_LOC, DensityReport, TopContributor};
pub use model::{INDEX_FILENAME, SCHEMA_VERSION};
pub use token::{TokenEncoding, TokenizerKind};

/// Cap on public item names listed in a [`PackageSummary::public`]
/// headline before truncating with `…` — mirrors the per-directory
/// `children[]` headline cap.
const PACKAGE_PUBLIC_HEADLINE_CAP: usize = 12;

/// Input bundle for [`emit`]. The emitter is pure — every effective
/// input is on this struct.
pub struct EmitInput<'a> {
    /// `package` field from `package.toml`. Surfaces in the emitted
    /// header.
    pub project_name: &'a str,
    /// Absolute path of the package root (the directory containing
    /// `package.toml`). Per-directory `index.toon` paths are
    /// produced under this root.
    pub package_root: &'a Path,
    /// Compiler version string emitted into the root header.
    pub compiler_version: &'a str,
    /// Resolved + parsed source graph. Provides per-module ASTs and
    /// canonical paths.
    pub resolved: &'a ResolvedPackage,
    /// Typecheck result. Provides function signatures, type-decl
    /// layouts, spec invocations.
    pub typed: &'a TypedPackage,
    /// String interner the resolver / typechecker ran against.
    pub interner: &'a Interner,
    /// Type interner the typechecker ran against. Reserved for the
    /// follow-up wave that prints type-level signatures from `TyId`s.
    #[allow(dead_code)]
    pub ty_interner: &'a TyInterner,
    /// Source map that issued the spans on the resolved + typed package.
    /// Used to translate spans to 1-based line numbers in emitted entries.
    pub source_map: &'a SourceMap,
    /// `[structmap] descendant_tree` knob from `package.toml`. When
    /// `true`, the root `index.toon` carries the full recursive tree of
    /// every descendant directory's items inlined into the root file —
    /// each descendant block is preceded by a `# directory: <rel>`
    /// header so readers can locate the per-directory section without
    /// walking the per-directory files. Per-directory `index.toon`
    /// files are still emitted unchanged. Defaults to `false`.
    pub descendant_tree: bool,
    /// Token-budget thresholds + consuming-model calibration. Drives the
    /// budget gate (Gate A per-node + Gate B structure) run inside
    /// [`emit`]; the projection subtracts exempt-atomic files.
    pub budget_config: BudgetConfig,
}

/// Result bundle from [`emit`]. Carries the rendered `index.toon` text
/// keyed by on-disk path plus the structural-density analyses the
/// driver maps onto `structure_map_too_dense`,
/// `filename_encodes_hierarchy`, and `file_low_cohesion`
/// diagnostics (codex §5.6).
pub struct EmitOutput {
    /// Absolute path → TOON text. The driver writes each entry to
    /// disk; the daemon may serve them directly.
    pub files: BTreeMap<PathBuf, String>,
    /// Per-directory density projection. The driver converts entries
    /// whose `projected_loc` exceeds the configured threshold (default
    /// [`DEFAULT_DENSITY_WARN_LOC`]) into `structure_map_too_dense`
    /// diagnostics.
    pub density_reports: Vec<DensityReport>,
    /// Per-directory filename-cluster findings. Non-empty entries map
    /// to `filename_encodes_hierarchy` diagnostics.
    pub cluster_reports: Vec<ClusterReport>,
    /// Per-file call-graph cohesion findings. Each entry maps to one
    /// `file_low_cohesion` diagnostic — the file has ≥
    /// [`FILE_COHESION_MIN_COMPONENT_COUNT`] disjoint clusters of ≥
    /// [`FILE_COHESION_MIN_COMPONENT_SIZE`] functions and is ≥
    /// [`FILE_COHESION_LOC_FLOOR`] lines long.
    pub file_cohesion_reports: Vec<FileCohesionReport>,
    /// Structure-budget gate result over the rendered `index.toon` set:
    /// per-node measures + the overage-sorted worklist of self-classifying
    /// [`Directive`]s. The driver renders each directive as a
    /// `structure_map_too_dense` diagnostic. Non-partitionable files are
    /// subtracted from the per-node projection (rendered in full, never
    /// deferred); ≥2 call-disjoint such files over the cap surface as a
    /// Gate B `AtomicHoard`.
    pub budget: BudgetReport,
    /// Whole-package summary — total item counts + a public-surface
    /// headline. The driver lifts this into a [`MemberSummary`] so the
    /// workspace aggregator can show every member's public API in one
    /// read (schema v6).
    pub summary: PackageSummary,
    /// Every directory's rendered item-table body (types/functions/
    /// invariants/patterns/trust_points — no header, no `loc:` line),
    /// keyed by absolute directory path. Always populated (cheap — reuses
    /// the same per-directory render pass as `files`); a standalone
    /// single-package build never reads it. `run_workspace_in_process`
    /// collects it per member so `emit_workspace_descendant_tree` can
    /// inline every member's own directories under the workspace root
    /// when `[structmap] descendant_tree` resolves `true`.
    pub descendant_bodies: BTreeMap<PathBuf, String>,
}

/// Whole-package structure summary returned on [`EmitOutput`]. Feeds the
/// workspace aggregator's rolled-up `children[]` rows.
#[derive(Debug, Clone, Default)]
pub struct PackageSummary {
    /// Total `type`/`spec` declarations across the whole package.
    pub types: u32,
    /// Total functions across the whole package.
    pub functions: u32,
    /// Space-joined headline of the package's public item names (public
    /// types then public functions), capped + `…`-truncated.
    pub public: String,
}

/// One workspace member's rolled-up summary, supplied by the driver to
/// [`emit_workspace_aggregator`] so the aggregator's `children[]` rows
/// carry real totals + the member's public-surface headline.
#[derive(Debug, Clone)]
pub struct MemberSummary {
    /// Absolute manifest-dir path of the member.
    pub dir: PathBuf,
    /// Total `type`/`spec` declarations in the member package.
    pub types: u32,
    /// Total functions in the member package.
    pub functions: u32,
    /// The member's public-surface headline.
    pub public: String,
}

/// Build the per-directory structure map for one package. Returns the
/// on-disk path → TOON text mapping alongside the structural-density
/// analyses (codex §5.6) the driver consumes to emit
/// `structure_map_too_dense` and `filename_encodes_hierarchy`
/// diagnostics.
pub fn emit(input: &EmitInput) -> EmitOutput {
    let mut tree = walk::build_tree(input);
    analyze::compute_effect_cones(&mut tree);
    let cluster_reports = cluster::analyze_clusters(&tree);
    // Cohesion + section-density analyses read source files from disk
    // via the closures below — same I/O policy as `drive_structmap`'s
    // write side. Failures silently skip the file so a single unreadable
    // source doesn't cascade-kill the whole emit pass.
    let file_cohesion_reports = cohesion::analyze_file_cohesion(&tree, &|p| {
        std::fs::read_to_string(p).ok().map(|s| s.lines().count())
    });

    // Single-pass budget (schema v7): render once, then gate. There
    // is no deferral/fix-point — a non-partitionable file is rendered in
    // full and subtracted from the Gate-A projection inside
    // [`analyze_budget`], so node costs and the worklist already agree
    // after one render.
    let files = emit::render(input, &tree);
    let budget = analyze_budget(&files, input.package_root, &input.budget_config, Some(&tree));

    let density_reports = density::compute_density(&tree, &files);
    let summary = compute_package_summary(&tree);
    let descendant_bodies: BTreeMap<PathBuf, String> = tree
        .dirs
        .iter()
        .map(|(dir, entry)| (dir.clone(), emit::render_dir_body_owned(entry)))
        .collect();
    EmitOutput {
        files,
        density_reports,
        cluster_reports,
        file_cohesion_reports,
        budget,
        summary,
        descendant_bodies,
    }
}

/// Roll a whole package's tree into a [`PackageSummary`] for the
/// workspace aggregator.
fn compute_package_summary(tree: &model::Tree) -> PackageSummary {
    let mut types: u32 = 0;
    let mut functions: u32 = 0;
    let mut names: Vec<String> = Vec::new();
    for entry in tree.dirs.values() {
        types = types.saturating_add(entry.types.len() as u32);
        functions = functions.saturating_add(entry.functions.len() as u32);
        for t in &entry.types {
            if t.visibility == model::Visibility::Public {
                names.push(t.name.clone());
            }
        }
        for f in &entry.functions {
            if f.visibility == model::Visibility::Public {
                let bare = f
                    .qualified_name
                    .rsplit_once('.')
                    .map(|(_, leaf)| leaf.to_string())
                    .unwrap_or_else(|| f.qualified_name.clone());
                names.push(bare);
            }
        }
    }
    let public = if names.len() > PACKAGE_PUBLIC_HEADLINE_CAP {
        format!("{} …", names[..PACKAGE_PUBLIC_HEADLINE_CAP].join(" "))
    } else {
        names.join(" ")
    };
    PackageSummary {
        types,
        functions,
        public,
    }
}

/// Input bundle for [`emit_workspace_aggregator`].
pub struct WorkspaceAggregatorInput<'a> {
    /// `package` field from the workspace-root `package.toml` (or
    /// project name fallback). Surfaces in the aggregator root header so
    /// the chain-walk reader sees a consistent project label.
    pub project_name: &'a str,
    /// Absolute path of the workspace root (the directory containing
    /// the `[workspace]` manifest). Aggregator files are produced
    /// under this root.
    pub workspace_root: &'a Path,
    /// Compiler version string emitted into the root header.
    pub compiler_version: &'a str,
    /// Per-member summaries (manifest dir + rolled-up totals + public
    /// headline). The aggregator emits one `index.toon` at the workspace
    /// root and one at every intermediate directory between the workspace
    /// root and any member dir (the member dirs themselves get their own
    /// `index.toon` via the per-package [`emit`] call and are NOT emitted
    /// here); each emitted `children[]` row carries the rolled-up totals
    /// of the members under it plus, when the child IS a member dir, that
    /// member's public-surface headline.
    pub members: &'a [MemberSummary],
}

/// Emit aggregator `index.toon` files for a workspace.
///
/// The per-package [`emit`] call only walks within one package's
/// `package_root`; in workspace layouts (`[workspace] members = [...]`
/// in the top-level `package.toml`), nothing emits an `index.toon` at
/// the workspace root or at intermediate `lib/`-like directories. That
/// breaks the chain-walk
/// discipline: an LLM agent reading the structure map from project
/// root down to a leaf hits a missing intermediate level, and the
/// "always read the next index.toon" expectation breaks.
///
/// This function fills the gap. The driver calls it after every
/// workspace member has emitted its own per-package `index.toon`,
/// passing each member's [`MemberSummary`]; the returned files are
/// walkable routing nodes (slim header + `children[]` with real
/// rolled-up totals + the member public headline) at every
/// intermediate directory.
pub fn emit_workspace_aggregator(
    input: &WorkspaceAggregatorInput,
) -> BTreeMap<PathBuf, String> {
    match build_aggregate_tree(input) {
        Some(tree) => emit::render_aggregator_tree(
            input.project_name,
            input.compiler_version,
            input.workspace_root,
            &tree,
        ),
        None => BTreeMap::new(),
    }
}

/// Build the aggregate-directory tree shared by [`emit_workspace_aggregator`]
/// and [`emit_workspace_descendant_tree`]. Returns `None` when there is
/// nothing to emit (mirrors the pre-refactor early return).
fn build_aggregate_tree(input: &WorkspaceAggregatorInput) -> Option<model::Tree> {
    let member_dirs: Vec<PathBuf> = input.members.iter().map(|m| m.dir.clone()).collect();
    let aggregate_dirs = compute_aggregate_dirs(input.workspace_root, &member_dirs);
    if aggregate_dirs.is_empty() {
        return None;
    }
    let mut tree = model::Tree { dirs: BTreeMap::new() };
    let aggregate_set: std::collections::BTreeSet<&Path> =
        aggregate_dirs.iter().map(|p| p.as_path()).collect();
    let member_set: std::collections::BTreeSet<&Path> =
        member_dirs.iter().map(|p| p.as_path()).collect();
    for agg in &aggregate_dirs {
        let mut entry = model::DirEntry::default();
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for cand in aggregate_set.iter().chain(member_set.iter()) {
            if cand.as_os_str() == agg.as_os_str() {
                continue;
            }
            let Some(parent) = cand.parent() else { continue };
            if parent != agg.as_path() {
                continue;
            }
            let rel = match cand.strip_prefix(agg) {
                Ok(r) => r.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };
            if !seen.insert(rel.clone()) {
                continue;
            }
            // Roll up every member whose dir lies under this child, and
            // borrow the member's own public headline when the child IS a
            // member dir directly.
            let (mut types, mut functions) = (0u32, 0u32);
            let mut public = String::new();
            for m in input.members {
                if m.dir.starts_with(cand) {
                    types = types.saturating_add(m.types);
                    functions = functions.saturating_add(m.functions);
                }
                if m.dir.as_path() == *cand {
                    public = m.public.clone();
                }
            }
            entry.children.push(model::ChildIndex {
                path: rel,
                types,
                functions,
                public,
            });
        }
        entry.children.sort_by(|a, b| a.path.cmp(&b.path));
        tree.dirs.insert(agg.clone(), entry);
    }
    Some(tree)
}

/// Emit the workspace-root `index.toon` in full-descendant-tree form:
/// every aggregate/intermediate directory keeps its ordinary routing
/// file, but the workspace root's own file additionally inlines every
/// aggregate directory's body plus every member's own per-directory item
/// tables (`member_bodies` — the caller's union of every member's
/// [`EmitOutput::descendant_bodies`]) behind `# directory: <rel>` section
/// headers, exactly like the per-package `descendant_tree` root
/// ([`emit::render_root_descendant_tree`]) does for a single package.
///
/// An empty `member_bodies` still produces a valid (if member-item-empty)
/// descendant-tree root rather than panicking — the caller is expected to
/// backfill it once the per-member data is available end-to-end.
pub fn emit_workspace_descendant_tree(
    input: &WorkspaceAggregatorInput,
    member_bodies: &BTreeMap<PathBuf, String>,
) -> BTreeMap<PathBuf, String> {
    let Some(tree) = build_aggregate_tree(input) else {
        return BTreeMap::new();
    };
    let mut files = emit::render_aggregator_tree(
        input.project_name,
        input.compiler_version,
        input.workspace_root,
        &tree,
    );
    let mut bodies: BTreeMap<PathBuf, String> = BTreeMap::new();
    for (dir, entry) in &tree.dirs {
        bodies.insert(dir.clone(), emit::render_dir_body_owned(entry));
    }
    for (dir, body) in member_bodies {
        bodies.insert(dir.clone(), body.clone());
    }
    let root_text = emit::render_workspace_descendant_tree(
        input.project_name,
        input.compiler_version,
        input.workspace_root,
        &bodies,
    );
    files.insert(input.workspace_root.join(INDEX_FILENAME), root_text);
    files
}

fn compute_aggregate_dirs(
    workspace_root: &Path,
    member_dirs: &[PathBuf],
) -> Vec<PathBuf> {
    let mut set: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    set.insert(workspace_root.to_path_buf());
    for member in member_dirs {
        let Some(mut cur) = member.parent().map(Path::to_path_buf) else { continue };
        loop {
            if !cur.starts_with(workspace_root) {
                break;
            }
            set.insert(cur.clone());
            if cur.as_path() == workspace_root {
                break;
            }
            cur = match cur.parent() {
                Some(p) => p.to_path_buf(),
                None => break,
            };
        }
    }
    set.into_iter().collect()
}

#[cfg(test)]
mod aggregator_tests {
    use super::*;

    fn member(dir: PathBuf, types: u32, functions: u32, public: &str) -> MemberSummary {
        MemberSummary {
            dir,
            types,
            functions,
            public: public.to_string(),
        }
    }

    #[test]
    fn aggregator_emits_workspace_root_and_intermediate_dirs() {
        // Workspace with members under `lib/<name>/` — emit aggregators
        // at workspace root + `lib/`.
        let root = PathBuf::from("/ws");
        let members = vec![
            member(root.join("lib").join("cache"), 3, 10, "Store hash_bytes"),
            member(root.join("lib").join("compile"), 2, 7, "Emitter compile"),
            member(root.join("lib").join("syntax"), 5, 20, "Token Parser parse"),
        ];
        let input = WorkspaceAggregatorInput {
            project_name: "ws",
            workspace_root: &root,
            compiler_version: "0.0.0",
            members: &members,
        };
        let files = emit_workspace_aggregator(&input);
        // Expect 2 files: <root>/index.toon and <root>/lib/index.toon.
        assert!(files.contains_key(&root.join("index.toon")));
        assert!(files.contains_key(&root.join("lib").join("index.toon")));
        // Member dirs MUST NOT be in the aggregator output — they get
        // their own per-package emit.
        assert!(!files.contains_key(&root.join("lib").join("cache").join("index.toon")));
        // Root's child `lib` carries the ROLLED-UP totals (3+2+5 types,
        // 10+7+20 functions) and no member public headline (intermediate).
        let root_text = &files[&root.join("index.toon")];
        assert!(root_text.contains("lib,10,37,"), "root_text: {root_text}");
        // Root file carries the slim root header, no `loc:`.
        assert!(root_text.contains("project: ws"));
        assert!(root_text.contains("schema_version: 8"));
        assert!(!root_text.contains("loc:"));
        assert!(!root_text.contains("parent:"));
        assert!(!root_text.contains("generated_at"));
        // lib/'s children list each member with its OWN totals + public.
        let lib_text = &files[&root.join("lib").join("index.toon")];
        assert!(lib_text.contains("loc: lib"));
        assert!(!lib_text.contains("parent:"));
        assert!(lib_text.contains("cache,3,10,Store hash_bytes"));
        assert!(lib_text.contains("compile,2,7,Emitter compile"));
        assert!(lib_text.contains("syntax,5,20,Token Parser parse"));
        // Adaptive: empty item tables are NOT emitted (pure router).
        assert!(!lib_text.contains("modules[0]"));
        assert!(!lib_text.contains("types[0]"));
        assert!(!lib_text.contains("functions[0]"));
        assert!(!lib_text.contains("invariants[0]"));
        assert!(!lib_text.contains("patterns[0]"));
        assert!(!lib_text.contains("trust_points[0]"));
        // Retired doc tables stay gone.
        assert!(!lib_text.contains("doc_high"));
    }

    #[test]
    fn aggregator_skips_member_dirs_under_workspace_root() {
        // When a member's parent IS the workspace root, only the root
        // gets an aggregator — no spurious intermediate.
        let root = PathBuf::from("/ws");
        let members = vec![
            member(root.join("foo"), 1, 2, "Foo"),
            member(root.join("bar"), 4, 8, "Bar baz"),
        ];
        let input = WorkspaceAggregatorInput {
            project_name: "ws",
            workspace_root: &root,
            compiler_version: "0.0.0",
            members: &members,
        };
        let files = emit_workspace_aggregator(&input);
        assert_eq!(files.len(), 1);
        assert!(files.contains_key(&root.join("index.toon")));
        // Root's children list foo and bar with their own totals + public
        // (these children ARE member dirs directly).
        let text = &files[&root.join("index.toon")];
        assert!(text.contains("foo,1,2,Foo"));
        assert!(text.contains("bar,4,8,Bar baz"));
    }

    #[test]
    fn aggregator_handles_nested_intermediate_dirs() {
        // Members at deeper levels — aggregator emits at every level.
        let root = PathBuf::from("/ws");
        let members = vec![member(
            root.join("a").join("b").join("leaf"),
            2,
            3,
            "Leaf go",
        )];
        let input = WorkspaceAggregatorInput {
            project_name: "ws",
            workspace_root: &root,
            compiler_version: "0.0.0",
            members: &members,
        };
        let files = emit_workspace_aggregator(&input);
        // 3 aggregator files: root, root/a, root/a/b. Member root/a/b/leaf is NOT emitted.
        assert_eq!(files.len(), 3);
        assert!(files.contains_key(&root.join("index.toon")));
        assert!(files.contains_key(&root.join("a").join("index.toon")));
        assert!(files.contains_key(&root.join("a").join("b").join("index.toon")));
        // The `a/b` aggregator's child `leaf` is a member dir → carries
        // its public headline; rolled totals equal the member's own.
        let ab_text = &files[&root.join("a").join("b").join("index.toon")];
        assert!(ab_text.contains("leaf,2,3,Leaf go"));
    }

    #[test]
    fn descendant_tree_emit_inlines_member_bodies_under_root() {
        // Two members under lib/ — same shape as
        // `aggregator_emits_workspace_root_and_intermediate_dirs`, but
        // now with real per-directory bodies to inline.
        let root = PathBuf::from("/ws");
        let cache_dir = root.join("lib").join("cache");
        let compile_dir = root.join("lib").join("compile");
        let members = vec![
            member(cache_dir.clone(), 1, 1, "Store"),
            member(compile_dir.clone(), 1, 1, "Emitter"),
        ];
        let input = WorkspaceAggregatorInput {
            project_name: "ws",
            workspace_root: &root,
            compiler_version: "0.0.0",
            members: &members,
        };
        let mut member_bodies: BTreeMap<PathBuf, String> = BTreeMap::new();
        member_bodies.insert(
            cache_dir.clone(),
            "types[1]{name}:\n  Store\n\n".to_string(),
        );
        member_bodies.insert(
            compile_dir.clone(),
            "types[1]{name}:\n  Emitter\n\n".to_string(),
        );

        let files = emit_workspace_descendant_tree(&input, &member_bodies);

        // Every aggregate dir still gets its normal routing file (root +
        // lib/), exactly like the shallow aggregator.
        assert!(files.contains_key(&root.join("index.toon")));
        assert!(files.contains_key(&root.join("lib").join("index.toon")));
        // Member dirs themselves are still NOT emitted here (unchanged
        // invariant — they got their own file via the per-package emit).
        assert!(!files.contains_key(&cache_dir.join("index.toon")));

        let root_text = &files[&root.join("index.toon")];
        assert!(root_text.contains("descendant_tree: true"), "{root_text}");
        // Root's own rollup row is still inlined first, unlabeled.
        assert!(root_text.contains("lib,2,2,"), "{root_text}");
        // Every member's body is inlined behind a `# directory:` header,
        // keyed relative to the WORKSPACE root (not the member's own).
        assert!(
            root_text.contains("# directory: lib/cache\n\ntypes[1]{name}:\n  Store"),
            "{root_text}"
        );
        assert!(
            root_text.contains("# directory: lib/compile\n\ntypes[1]{name}:\n  Emitter"),
            "{root_text}"
        );

        // The intermediate `lib/` file is untouched by descendant-tree
        // mode — still the plain router shape, no `descendant_tree` line.
        let lib_text = &files[&root.join("lib").join("index.toon")];
        assert!(!lib_text.contains("descendant_tree"));
        assert!(lib_text.contains("cache,1,1,Store"));
        assert!(lib_text.contains("compile,1,1,Emitter"));
    }

    #[test]
    fn descendant_tree_emit_with_empty_bodies_does_not_panic() {
        // Mirrors the per-member subprocess-isolated path
        // until a follow-up threads bodies across that process boundary
        // — an empty body map must still produce a valid
        // (member-item-empty) descendant-tree root, never panic.
        let root = PathBuf::from("/ws");
        let members = vec![member(root.join("lib").join("cache"), 3, 10, "Store")];
        let input = WorkspaceAggregatorInput {
            project_name: "ws",
            workspace_root: &root,
            compiler_version: "0.0.0",
            members: &members,
        };
        let files = emit_workspace_descendant_tree(&input, &BTreeMap::new());
        let root_text = &files[&root.join("index.toon")];
        assert!(root_text.contains("descendant_tree: true"));
        assert!(root_text.contains("lib,3,10,"));
    }
}
