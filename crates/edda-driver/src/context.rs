//! Driver state — interner, source map, manifest, layout, target,
//! diagnostics, and the active [`crate::reporter::Reporter`].
//!
//! [`Driver`] is constructed once per CLI invocation and once per
//! daemon session. Both paths reach through [`crate::run`], which
//! builds the driver via [`Driver::initialize`] and hands it to the
//! cascade orchestrator.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig, LintSeverity};
use edda_intern::Interner;
use edda_manifest::{DependencySource, PackageManifest};
use edda_resolve::{DepIndex, PackageLayout, ResolvedPackage, StdlibIndex, package_layout_from_manifest};
use edda_span::{Span, SourceMap};
use edda_target::{FeatureSet, TargetCfg, TargetTriple};
use edda_types::TyInterner;

use crate::command::BuildOptions;
use crate::error::DriverError;
use crate::reporter::Reporter;
use crate::stdlib_source::{
    StdlibSource, build_stdlib_index, emit_stdlib_source_selection, env_stdlib_override,
    resolve_stdlib_source,
};
use crate::workspace::MemberHandle;

/// Shared driver state.
///
/// Single-invocation CLI builds drop the [`Driver`] after the cascade
/// returns. The daemon long-lives one [`Driver`] and re-runs cascade
/// stages as files change (`build-system.md` §5a).
pub struct Driver {
    /// Directory containing the manifest. Joined with `src/`, `tests/`,
    /// etc. to reach the §4 source subtrees.
    pub package_root: PathBuf,
    /// `<package_root>/src/` — the §4 default source root.
    pub src_root: PathBuf,
    /// CLI-supplied options carried from [`Driver::initialize`] for
    /// later cascade decisions (jobs cap, full-materialization toggle).
    pub options: BuildOptions,
    /// String interner shared across all passes.
    pub interner: Interner,
    /// Type interner shared across the typecheck / MIR / compile
    /// passes. Held on the driver so typecheck state can flow
    /// into later lowering stages without re-interning.
    pub ty_interner: TyInterner,
    /// Source map shared across all passes.
    pub source_map: SourceMap,
    /// Parsed `package.toml`.
    pub manifest: PackageManifest,
    /// `build-system.md` §4 path-resolution context.
    pub layout: PackageLayout,
    /// Resolved dependency layouts, keyed by exposed `root_namespace`.
    /// Populated from workspace siblings and `[dependencies]` path entries
    /// by `initialize_with_siblings`; registry/git sources remain deferred.
    pub deps: DepIndex,
    /// Compiler-bundled stdlib catalogue. Populated by `build_stdlib_index`
    /// from the selected [`StdlibSource`] root.
    pub stdlib: StdlibIndex,
    /// Which root populated [`Self::stdlib`] this invocation. Holds the
    /// single source-of-truth selection made by [`resolve_stdlib_source`]:
    /// either the active worktree (when the CWD looks like a stdlib
    /// worktree), the compile-time-baked vendored path, or `None` when
    /// neither is available. Downstream consumers (tier classification
    /// in [`crate::codegen`]) consult this rather than re-deriving the
    /// stdlib path so the dual-load → collision bug never recurs.
    pub stdlib_source: StdlibSource,
    /// `(triple, features)` grounding `target_has(...)` queries.
    pub target: TargetCfg,
    /// Effective lint configuration (`manifest.lints` ⊕ CLI overrides).
    pub lint_cfg: LintConfig,
    /// Accumulating diagnostics. The cascade moves these out into
    /// [`crate::outcome::Outcome::diagnostics`] at the end.
    pub diagnostics: Diagnostics,
    /// Streaming reporter (CLI stdout printer, daemon MCP forwarder,
    /// or [`crate::reporter::SilentReporter`] for library use).
    pub reporter: Arc<dyn Reporter>,
    /// Result of the import-resolve pass, populated by
    /// [`crate::cascade::run`]. `None` before the cascade runs and
    /// when the cascade aborted before import-resolve completed.
    pub resolved: Option<ResolvedPackage>,
    /// Object-file paths produced by the compile pass, in link order.
    /// Populated by [`crate::cascade::drive_compile`] via
    /// `edda_compile::Emitter::compile_program_to_object`; consumed by the
    /// link stage ([`crate::cascade::drive_link`]), which short-circuits
    /// only when the set is empty (e.g. a library with no entry point).
    pub compile_outputs: Vec<PathBuf>,
    /// DLL import sets from `extern "sym" from "dll"` declarations,
    /// grouped by DLL name with sorted,
    /// deduplicated symbol lists. Populated by
    /// [`crate::cascade::drive_compile`] from the typed package's
    /// extern declarations; consumed by [`crate::cascade::drive_link`],
    /// which synthesises one PE import library per entry and appends it
    /// to the link inputs. Empty when no extern carries a `from` clause.
    pub dll_imports: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
    /// `@abi("__edda_*")` export symbols this member's own object file
    /// defines, shadowing edda-rt.lib's like-named runtime exports.
    /// Populated by
    /// [`crate::cascade::drive_compile`] from the compiled function set;
    /// read by [`crate::cascade::drive_link`], which — when the set is
    /// non-empty — passes the linker its "first definition wins" flag so
    /// the (first-listed) object definition overrides the redundant rt
    /// member rather than colliding with it. Empty when no function
    /// claims an `__edda_*` symbol via `@abi`, in which case rt.lib is
    /// the sole provider and no override is needed.
    pub abi_rt_shadows: std::collections::BTreeSet<String>,
    /// Non-runtime `@abi("sym")` symbol-name overrides this member's
    /// object file defines — the C-ABI export surface (the complement of
    /// [`Self::abi_rt_shadows`] over the same channel; every `@abi`
    /// override whose symbol does not begin with `__edda_`). Populated by
    /// [`crate::cascade::drive_compile`]; read by
    /// [`crate::cascade::drive_link`] to emit one explicit `/EXPORT:` per
    /// name when the manifest requests `kind = "dynamic_library"` on a
    /// COFF target. Empty when no
    /// function claims a foreign symbol via `@abi`.
    pub abi_exports: std::collections::BTreeSet<String>,
    /// Whether this package's compiled program has a binary entry point
    /// (`function main`). Set by [`crate::cascade::drive_compile`] from
    /// `program.entry.is_some()` after `pick_entry`, and read by
    /// [`crate::cascade::drive_link`] to choose between linking an
    /// executable (entry present) and archiving a static library (no
    /// entry — the library-member case in a workspace build).
    /// `false` until the compile stage runs.
    pub has_entry: bool,
    /// Whole-package structure-map summary (total item counts + public
    /// headline) produced by [`crate::structmap::drive_structmap`].
    /// `None` until structmap emission runs (and stays `None` for verbs
    /// that don't emit it). `run_workspace` reads it after each member's
    /// cascade to build the workspace aggregator's rolled-up `children[]`
    /// (schema v6).
    pub structmap_summary: Option<edda_structmap::PackageSummary>,
    /// This package's per-directory structure-map bodies (types/functions/
    /// invariants/patterns/trust_points text, keyed by absolute directory
    /// path), produced alongside `structmap_summary` by
    /// [`crate::structmap::drive_structmap`]. Empty until structmap
    /// emission runs. `run_workspace_in_process` collects every member's
    /// map into one workspace-wide body set so the workspace-root
    /// `index.toon` can inline every member's items when `[structmap]
    /// descendant_tree` resolves `true`.
    pub descendant_bodies: std::collections::BTreeMap<PathBuf, String>,
    /// Standalone object compiled from `std.text.string`'s `equals`
    /// function alone, populated by
    /// [`crate::cascade::drive_compile`] only when this member's own MIR
    /// lowers a `Str`-class `Eq`/`Ne` binop AND this member's own object
    /// does not already shadow `__edda_str_eq`.
    /// `None` when Str equality is never emitted, when this member's own
    /// closure already provides the symbol, or when the scratch compile
    /// could not run (e.g. no stdlib root). Read by
    /// [`crate::cascade::drive_link`], which appends it to the executable
    /// link inputs so the pure-Edda `@abi("__edda_str_eq")` body is always
    /// available even when nothing in this member's own source imports
    /// `std.text.string` — `==`/`!=` on `String` needs no import.
    pub str_eq_support_object: Option<PathBuf>,
}

impl Driver {
    /// Build the driver: load the manifest, derive the package layout,
    /// validate the active target and its feature set, and prepare
    /// empty diagnostic / resolved-state slots.
    ///
    /// # Errors
    ///
    /// - [`DriverError::ManifestLoad`] — `package.toml` could not be
    ///   read or parsed. A parse diagnostic has already been pushed
    ///   into the (lost) bootstrap take; the CLI surfaces the error
    ///   variant directly.
    /// - [`DriverError::SourceRootMissing`] — `<package_root>/src/`
    ///   does not exist.
    /// - [`DriverError::TargetUnspecified`] — neither
    ///   `manifest.build.default_target` nor `options.target_override`
    ///   is set.
    /// - [`DriverError::InvalidTriple`] — `options.target_override`
    ///   did not parse against `build-system.md` §9.
    pub fn initialize(
        options: BuildOptions,
        reporter: Arc<dyn Reporter>,
    ) -> Result<Self, DriverError> {
        Self::initialize_with_siblings(options, reporter, &[])
    }

    /// Build a driver instance for one workspace member. The `siblings`
    /// list registers every other member as an implicit dependency
    /// keyed by its `root_namespace`, so `import other_member.foo` from
    /// this member resolves through the resolver's third-party-package
    /// path without an explicit `[dependencies]` entry.
    ///
    /// Pass `siblings = &[]` for single-package builds; that path is
    /// equivalent to the public [`Driver::initialize`].
    pub(crate) fn initialize_with_siblings(
        options: BuildOptions,
        reporter: Arc<dyn Reporter>,
        siblings: &[MemberHandle],
    ) -> Result<Self, DriverError> {
        let interner = Interner::new();
        let ty_interner = TyInterner::new();
        let source_map = SourceMap::new();
        let mut diagnostics = Diagnostics::new();
        let bootstrap_lint_cfg = LintConfig::new();

        let manifest = edda_manifest::load(
            &options.manifest_path,
            &source_map,
            &mut diagnostics,
            &bootstrap_lint_cfg,
        )
        .ok_or_else(|| DriverError::ManifestLoad {
            path: options.manifest_path.clone(),
        })?;

        let package_root = absolute_package_root(&options.manifest_path);
        let src_root = package_root.join("src");
        if !src_root.is_dir() {
            return Err(DriverError::SourceRootMissing {
                path: src_root,
                source: std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "src/ directory not found",
                ),
            });
        }

        let layout = package_layout_from_manifest(&manifest, package_root.clone(), &interner);
        let lint_cfg = compose_lint_cfg(&manifest, &options);
        let target = resolve_target(&manifest, &options, &mut diagnostics, &lint_cfg)?;
        let stdlib_source = resolve_stdlib_source(&package_root, env_stdlib_override());
        let stdlib = build_stdlib_index(&interner, &stdlib_source);
        emit_stdlib_source_selection(
            &stdlib_source,
            stdlib.modules_len(),
            &mut diagnostics,
            &lint_cfg,
        );

        let mut deps = DepIndex::new();
        for sibling in siblings {
            let sibling_layout = package_layout_from_manifest(
                &sibling.manifest,
                sibling.manifest_dir.clone(),
                &interner,
            );
            deps.insert(sibling_layout);
        }
        register_path_dependencies(
            &manifest,
            &package_root,
            &interner,
            &mut deps,
            &mut diagnostics,
            &lint_cfg,
        );

        Ok(Self {
            package_root,
            src_root,
            options,
            interner,
            ty_interner,
            source_map,
            manifest,
            layout,
            deps,
            stdlib,
            stdlib_source,
            target,
            lint_cfg,
            diagnostics,
            reporter,
            resolved: None,
            compile_outputs: Vec::new(),
            dll_imports: std::collections::BTreeMap::new(),
            abi_rt_shadows: std::collections::BTreeSet::new(),
            abi_exports: std::collections::BTreeSet::new(),
            has_entry: false,
            structmap_summary: None,
            descendant_bodies: std::collections::BTreeMap::new(),
            str_eq_support_object: None,
        })
    }
}

/// Absolute directory containing the manifest at `manifest_path`.
fn absolute_package_root(manifest_path: &Path) -> PathBuf {
    let parent = match manifest_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::new(),
    };
    absolutize_dir(&parent)
}

/// Absolutize `dir` relative to the current working directory. Exposed
/// crate-wide (not just for manifest parents) so any two paths a single
/// cascade run derives independently — one from the top manifest, one
/// from a spawned member child's own absolute view — can be compared
/// for containment on the same basis.
pub(crate) fn absolutize_dir(dir: &Path) -> PathBuf {
    if dir.is_absolute() {
        return dir.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) if dir.as_os_str().is_empty() => cwd,
        Ok(cwd) => cwd.join(dir),
        Err(_) if dir.as_os_str().is_empty() => PathBuf::from("."),
        Err(_) => dir.to_path_buf(),
    }
}

/// Resolve the active target.
///
/// Order of precedence: `--target` override → `manifest.build.default_target`.
/// Unknown features (manifest- or CLI-supplied) emit the
/// `unknown_target_feature` class and continue; the resulting
/// [`TargetCfg`] still grounds `target_has(...)` against the features
/// that *did* validate.
fn resolve_target(
    manifest: &PackageManifest,
    options: &BuildOptions,
    diagnostics: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Result<TargetCfg, DriverError> {
    let triple = pick_triple(manifest, options)?;
    let mut features = FeatureSet::new(triple.arch());

    for spec in &manifest.build.default_features {
        if features.insert(spec.name.as_ref()).is_err() {
            emit_unknown_feature(
                diagnostics,
                lint_cfg,
                &spec.name,
                spec.span,
                triple,
            );
        }
    }
    for name in &options.feature_override {
        if features.insert(name).is_err() {
            emit_unknown_feature(diagnostics, lint_cfg, name, Span::DUMMY, triple);
        }
    }

    Ok(TargetCfg::with_features(triple, features)
        .expect("FeatureSet was built with triple.arch() — arch mismatch impossible"))
}

/// Compose the effective [`LintConfig`] for one build invocation by
/// overlaying the CLI's `--warn-as-error` list on top of the manifest's
/// `lints` block. Without this overlay, a CLI escalation
/// (`--warn-as-error unused_import`) is silently dropped and warnings
/// stay at their default severity — which lets a build with only
/// warnings exit 0 even when the operator asked for it to fail.
fn compose_lint_cfg(manifest: &PackageManifest, options: &BuildOptions) -> LintConfig {
    let mut cfg = manifest.lints.clone();
    for class in &options.warn_as_error {
        cfg.set(*class, LintSeverity::Error);
    }
    cfg
}

fn pick_triple(manifest: &PackageManifest, options: &BuildOptions) -> Result<TargetTriple, DriverError> {
    if let Some(spelling) = &options.target_override {
        return TargetTriple::parse(spelling).map_err(|_| DriverError::InvalidTriple {
            spelling: spelling.clone(),
        });
    }
    manifest
        .build
        .default_target
        .ok_or(DriverError::TargetUnspecified)
}

fn emit_unknown_feature(
    diagnostics: &mut Diagnostics,
    lint_cfg: &LintConfig,
    name: &str,
    span: Span,
    triple: TargetTriple,
) {
    let class = DiagnosticClass::UnknownTargetFeature;
    let severity = lint_cfg.effective(class);
    let message = format!(
        "feature `{name}` is not in the locked catalogue for arch `{}`",
        triple.arch().name(),
    );
    let diag = Diagnostic::new(class, severity, span, message).with_note(
        "feature names are spec-locked in build-system.md §9; the arch-specific catalogue is the source of truth",
    );
    diagnostics.push(diag);
}

/// Register every `source = "path+<rel>"` dependency declared in
/// `manifest` into `deps` so the resolver can pull the dependency's
/// modules in transitively. A missing directory, an unreadable or
/// unparseable target `package.toml`, or a `root_namespace` collision
/// with an already-registered package each emit an
/// `import_resolution_error` and skip that one entry rather than
/// aborting the whole build.
fn register_path_dependencies(
    manifest: &PackageManifest,
    package_root: &Path,
    interner: &Interner,
    deps: &mut DepIndex,
    diagnostics: &mut Diagnostics,
    lint_cfg: &LintConfig,
) {
    for dep in &manifest.dependencies {
        let DependencySource::Path(rel) = &dep.source else {
            continue;
        };
        let joined = package_root.join(rel.as_ref());
        let dep_dir = match resolve_path_dependency_dir(package_root, Path::new(rel.as_ref())) {
            Ok(path) => path,
            Err(err) => {
                emit_path_dep_error(diagnostics, lint_cfg, &dep.name, rel, &format!(
                    "path `{}` could not be resolved ({err})",
                    joined.display()
                ));
                continue;
            }
        };
        let dep_manifest_path = dep_dir.join("package.toml");
        let dep_source_map = SourceMap::new();
        let mut dep_diags = Diagnostics::new();
        let dep_lint_cfg = LintConfig::new();
        let Some(dep_manifest) = edda_manifest::load(
            &dep_manifest_path,
            &dep_source_map,
            &mut dep_diags,
            &dep_lint_cfg,
        ) else {
            emit_path_dep_error(diagnostics, lint_cfg, &dep.name, rel, &format!(
                "`package.toml` at `{}` is missing or unparseable",
                dep_manifest_path.display()
            ));
            continue;
        };
        let dep_layout = package_layout_from_manifest(&dep_manifest, dep_dir, interner);
        if deps.get(dep_layout.root_namespace).is_some() {
            let ns = interner.resolve(dep_layout.root_namespace).to_owned();
            emit_path_dep_error(diagnostics, lint_cfg, &dep.name, rel, &format!(
                "its `root_namespace` `{ns}` collides with an already-registered package"
            ));
            continue;
        }
        deps.insert(dep_layout);
    }
}

/// Resolve a `path+<rel>` dependency directory relative to
/// `package_root`, retrying against the worktree's main checkout root
/// when the naive walk fails and `package_root` sits inside a linked git
/// worktree.
fn resolve_path_dependency_dir(package_root: &Path, rel: &Path) -> std::io::Result<PathBuf> {
    let naive = package_root.join(rel);
    let naive_result = naive.canonicalize();
    if naive_result.is_ok() {
        return naive_result;
    }
    if let Some((worktree_root, main_root)) = find_linked_worktree_roots(package_root) {
        if let Ok(suffix) = package_root.strip_prefix(&worktree_root) {
            if let Ok(resolved) = main_root.join(suffix).join(rel).canonicalize() {
                return Ok(resolved);
            }
        }
    }
    naive_result
}

/// Maximum depth [`find_linked_worktree_roots`] climbs before giving up.
const MAX_GIT_ANCESTOR_DEPTH: u32 = 16;

/// Walk up from `start` to find the nearest `.git` entry. If it is a
/// FILE — the linked-worktree marker, containing a single
/// `gitdir: <main>/.git/worktrees/<name>` line — return
/// `(worktree_root, main_checkout_root)`: the directory holding that
/// `.git` file, and the main checkout directory derived from the
/// pointer. Returns `None` when the nearest `.git` entry is a directory
/// (already a main checkout) or when none is found.
fn find_linked_worktree_roots(start: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut cur: Option<&Path> = Some(start);
    let mut depth: u32 = 0;
    while let Some(dir) = cur {
        if depth >= MAX_GIT_ANCESTOR_DEPTH {
            return None;
        }
        let git_entry = dir.join(".git");
        if git_entry.is_dir() {
            return None;
        }
        if git_entry.is_file() {
            let contents = std::fs::read_to_string(&git_entry).ok()?;
            let gitdir_line = contents.lines().find_map(|l| l.trim().strip_prefix("gitdir:"))?;
            let worktree_git_dir = PathBuf::from(gitdir_line.trim());
            let common_git_dir = worktree_git_dir.parent()?.parent()?;
            if common_git_dir.file_name()?.to_str()? != ".git" {
                return None;
            }
            let main_root = common_git_dir.parent()?.to_path_buf();
            return Some((dir.to_path_buf(), main_root));
        }
        cur = dir.parent();
        depth += 1;
    }
    None
}

/// Emit an `import_resolution_error` explaining why a `path+` dependency
/// could not be registered. Anchored at `Span::DUMMY` — the failure is a
/// manifest-level configuration problem with no single source token to
/// point at — with a note recalling the declared `source` spelling.
fn emit_path_dep_error(
    diagnostics: &mut Diagnostics,
    lint_cfg: &LintConfig,
    dep_name: &str,
    rel: &str,
    detail: &str,
) {
    let class = DiagnosticClass::ImportResolutionError;
    let severity = lint_cfg.effective(class);
    let message =
        format!("local path dependency `{dep_name}` could not be registered: {detail}");
    let diag = Diagnostic::new(class, severity, Span::DUMMY, message).with_note(format!(
        "declared as `source = \"path+{rel}\"`; resolved relative to this package's manifest directory"
    ));
    diagnostics.push(diag);
}
