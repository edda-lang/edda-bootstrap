//! Import-declaration resolution — the §4 dispatch from an `import`
//! statement's AST path to its canonical [`ModulePath`] and expected
//! on-disk source location.

use edda_diag::{Diagnostics, LintConfig};
use edda_intern::{Interner, Symbol};
use edda_span::Span;
use edda_syntax::ast::{Import, Path as AstPath};

use crate::layout::{DepIndex, ImporterContext, PackageLayout, owning_layout_for_file};
use crate::path::ModulePath;
use crate::resolve::{
    BENCH_DIR, CODEGEN_DIR, EDDA_EXT, EXAMPLES_DIR, ImportKind, ResolveCx, ResolvedImport, SRC_DIR,
    TESTS_DIR, emit_resolution_error, is_recovery_path,
};

/// Resolve an `import` declaration to its canonical [`ModulePath`] and
/// expected filesystem location. Pushes an `import_resolution_error`
/// diagnostic on any failure mode and returns `None`.
pub fn resolve_import_path(
    import: &Import,
    importer: &ImporterContext,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<ResolvedImport> {
    let path = &import.path;
    if is_recovery_path(path) {
        return None;
    }

    if path.segments.len() == 1 {
        return resolve_sibling_leaf(path, importer, cx, diags, lint_cfg);
    }

    resolve_dot_path(path, importer, cx, diags, lint_cfg)
}

/// Bare-leaf import (`import value`) per `declarations.md §301`:
/// resolves to a sibling `.ea` file in the importer's directory.
/// Collisions with in-scope dot-path prefixes are rejected.
fn resolve_sibling_leaf(
    path: &AstPath,
    importer: &ImporterContext,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<ResolvedImport> {
    let leaf = path.segments[0].name;
    let leaf_text = cx.interner.resolve(leaf);

    if collides_with_dotpath_prefix(leaf, leaf_text, cx.layout, cx.deps) {
        emit_resolution_error(
            path.span,
            format!("bare-leaf import `{leaf_text}` collides with an in-scope dot-path prefix"),
            "rename the sibling file or use the fully qualified dot-path form",
            diags,
            lint_cfg,
        );
        return None;
    }

    let expected_file = importer.importer_dir.join(format!("{leaf_text}.{EDDA_EXT}"));
    let canonical = match importer.importer_module.parent() {
        Some(parent) => parent.push(leaf),
        None => ModulePath::new(vec![importer.importer_module.first(), leaf]),
    };

    Some(ResolvedImport {
        canonical,
        expected_file,
        kind: ImportKind::Sibling,
    })
}

/// True when `leaf` matches any prefix that could appear as the head
/// of a dot-path import — `std`, the package's own `root_namespace`,
/// or any dependency's `root_namespace`.
fn collides_with_dotpath_prefix(
    leaf: Symbol,
    leaf_text: &str,
    layout: &PackageLayout,
    deps: &DepIndex,
) -> bool {
    leaf == layout.root_namespace || leaf_text == "std" || deps.get(leaf).is_some()
}

/// Multi-segment import — dispatch on the head segment.
fn resolve_dot_path(
    path: &AstPath,
    importer: &ImporterContext,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<ResolvedImport> {
    let canonical = ModulePath::from_ast(path);
    let head = canonical.first();
    let head_text = cx.interner.resolve(head);

    if head_text == "std" {
        return resolve_stdlib(canonical, path.span, cx, diags, lint_cfg);
    }

    if head_text == "local" {
        return resolve_local(canonical, path.span, importer, cx, diags, lint_cfg);
    }

    if head == cx.layout.root_namespace {
        return resolve_in_namespace(canonical, path.span, cx.layout, false, cx.interner, diags, lint_cfg);
    }

    if let Some(dep) = cx.deps.get(head) {
        return resolve_in_namespace(canonical, path.span, dep, true, cx.interner, diags, lint_cfg);
    }

    emit_resolution_error(
        path.span,
        format!("unknown import-path prefix `{head_text}`"),
        "expected `std`, `local`, this package's `root_namespace`, or a declared dependency's `root_namespace`",
        diags,
        lint_cfg,
    );
    None
}

/// `local.<path>` import — resolves directly under the *owning*
/// package's `src/` root. `local.parser.tokens` declared inside
/// `lib/foo/src/main.ea` maps to `lib/foo/src/parser/tokens.ea`
/// regardless of which workspace member is the active driver. The
/// canonical module path is built from the owning package's
/// `root_namespace` so identity comparison reaches the same module
/// entry whether the file is visited as `foo`'s entry or pulled in
/// as a dep of another member (B-003 fix).
fn resolve_local(
    canonical: ModulePath,
    span: Span,
    importer: &ImporterContext,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<ResolvedImport> {
    let segments = canonical.segments();
    if segments.len() < 2 {
        emit_resolution_error(
            span,
            "import `local` names the package root, not a module".to_owned(),
            "expected at least one segment after `local`",
            diags,
            lint_cfg,
        );
        return None;
    }

    let owning = owning_layout_for_file(&importer.importer_dir, cx.layout, cx.deps, cx.stdlib);

    // Rebuild canonical path with the owning layout's canonical root
    // path (multi-segment for stdlib leaves; single-segment
    // `[root_namespace]` for user packages) instead of `local`.
    let mut canonical_segs: Vec<Symbol> = owning.canonical_root_path.segments().to_vec();
    canonical_segs.extend_from_slice(&segments[1..]);
    let canonical_ns = ModulePath::new(canonical_segs);

    let mut expected = owning.root_dir.clone();
    expected.push(SRC_DIR);
    for sym in &segments[1..segments.len() - 1] {
        expected.push(cx.interner.resolve(*sym));
    }
    let leaf = segments.last().expect("len >= 2");
    expected.push(format!("{}.{EDDA_EXT}", cx.interner.resolve(*leaf)));

    Some(ResolvedImport {
        canonical: canonical_ns,
        expected_file: expected,
        kind: ImportKind::InPackage,
    })
}

/// `std.*` import — look up the canonical path in the stdlib index.
/// When the index is disabled ([`StdlibIndex::is_enabled`] is `false`)
/// the bootstrap could not locate a stdlib source root at startup —
/// per-import errors emitted here carry the operator-actionable
/// `EDDA_STDLIB_ROOT` hint; the root-cause note also lands once at
/// driver initialisation via `emit_stdlib_source_selection`.
fn resolve_stdlib(
    canonical: ModulePath,
    span: Span,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<ResolvedImport> {
    if let Some(file) = cx.stdlib.get(&canonical) {
        return Some(ResolvedImport {
            canonical,
            expected_file: file.clone(),
            kind: ImportKind::Stdlib,
        });
    }

    let note = if cx.stdlib.is_enabled() {
        "the bundled stdlib does not expose this module"
    } else {
        "stdlib source root not located at startup; set `EDDA_STDLIB_ROOT=<path/to/stdlib>` to override the compile-time-baked path"
    };
    emit_resolution_error(
        span,
        format!("unresolved stdlib import `{}`", canonical.display(cx.interner)),
        note,
        diags,
        lint_cfg,
    );
    None
}

/// Resolve a dot-path import whose head matches some package's
/// `root_namespace`. Used for both the importing package
/// (`is_third_party=false`) and dependency packages (`true`); the
/// subtree dispatch on `src/` / `tests/` / `bench/` / `examples/` /
/// `codegen/` is the same in both cases.
fn resolve_in_namespace(
    canonical: ModulePath,
    span: Span,
    layout: &PackageLayout,
    is_third_party: bool,
    interner: &Interner,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<ResolvedImport> {
    let segments = canonical.segments();
    if segments.len() < 2 {
        emit_resolution_error(
            span,
            format!(
                "import path `{}` names a package root, not a module",
                canonical.display(interner)
            ),
            "expected at least one segment after the package's `root_namespace`",
            diags,
            lint_cfg,
        );
        return None;
    }

    let (subtree, rest_start, kind) = classify_subtree(segments, interner, is_third_party);
    let rest = &segments[rest_start..];
    if rest.is_empty() {
        emit_resolution_error(
            span,
            format!(
                "import path `{}` names a subtree, not a module",
                canonical.display(interner)
            ),
            "expected at least one segment after the subtree name",
            diags,
            lint_cfg,
        );
        return None;
    }

    let mut expected = layout.root_dir.clone();
    expected.push(subtree);
    for sym in rest {
        expected.push(interner.resolve(*sym));
    }
    expected.set_extension(EDDA_EXT);

    Some(ResolvedImport {
        canonical,
        expected_file: expected,
        kind,
    })
}

/// Decide which top-level subtree owns the import. Returns the
/// `src/`-or-sibling directory name, the index in `segments` where
/// remaining module segments start, and the [`ImportKind`].
///
/// `segments[0]` is the `root_namespace` and is never part of the
/// returned `rest_start`.
fn classify_subtree(
    segments: &[Symbol],
    interner: &Interner,
    is_third_party: bool,
) -> (&'static str, usize, ImportKind) {
    let second_text = interner.resolve(segments[1]);
    match second_text {
        TESTS_DIR => (TESTS_DIR, 2, ImportKind::Tests),
        BENCH_DIR => (BENCH_DIR, 2, ImportKind::Bench),
        EXAMPLES_DIR => (EXAMPLES_DIR, 2, ImportKind::Examples),
        CODEGEN_DIR => (CODEGEN_DIR, 2, ImportKind::Codegen),
        _ => {
            let kind = if is_third_party {
                ImportKind::ThirdParty
            } else {
                ImportKind::InPackage
            };
            (SRC_DIR, 1, kind)
        }
    }
}
