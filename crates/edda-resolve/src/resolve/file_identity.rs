//! Source-file module identity — the §4 rule that derives a file's
//! canonical [`ModulePath`] from a `module` keyword override or from
//! its filesystem position under the owning package root.

use std::path::{Component, Path};

use edda_diag::{Diagnostics, LintConfig};
use edda_intern::Symbol;
use edda_span::Span;
use edda_syntax::ast::ModuleDecl;

use crate::layout::owning_layout_for_file;
use crate::path::ModulePath;
use crate::resolve::{
    BENCH_DIR, CODEGEN_DIR, EDDA_EXT, EXAMPLES_DIR, ResolveCx, SRC_DIR, TESTS_DIR,
    emit_resolution_error, is_recovery_path,
};

/// Compute the canonical [`ModulePath`] for a source file at
/// `file_path`. A `module` keyword override (`declarations.md §286`)
/// takes precedence; otherwise the path-derived identity is built
/// from the file's location under `layout.root_dir`.
///
/// `file_span` is used as the diagnostic location when the
/// path-derived rule fails (file outside the package, unrecognized
/// subtree, missing `.ea` extension); pass the file's content span
/// or [`Span::DUMMY`] for entry-point files.
pub fn module_identity_for_file(
    file_path: &Path,
    module_decl: Option<&ModuleDecl>,
    file_span: Span,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<ModulePath> {
    if let Some(decl) = module_decl {
        // Parser-recovery `module` declarations (typically synthesised
        // when the `module` keyword appears as a parameter or binding
        // name and the cursor lands on `parse_module`) carry a
        // sentinel `Symbol::DUMMY` head; the parse_error is already on
        // the diagnostic take, so we fall through to the path-derived
        // rule rather than minting a `[DUMMY]` canonical path that
        // would later panic when an interner lookup hits the sentinel.
        if !is_recovery_path(&decl.path) {
            return Some(ModulePath::from_ast(&decl.path));
        }
    }

    // Stdlib short-circuit: when the file matches a known entry in the
    // [`StdlibIndex`], the canonical module path is the one the index
    // already holds. This handles stdlib `.ea` files whose source omits
    // an explicit `module std.…` declaration — without this branch the
    // path-derived rule below runs against the importing package's
    // `root_dir` (the user package), which cannot produce a sensible
    // result for a stdlib file living elsewhere on disk.
    if let Some(canonical) = cx.stdlib.lookup_by_file(file_path) {
        return Some(canonical.clone());
    }

    let owning = owning_layout_for_file(file_path, cx.layout, cx.deps, cx.stdlib);

    let rel = match file_path.strip_prefix(&owning.root_dir) {
        Ok(r) => r,
        Err(_) => {
            emit_resolution_error(
                file_span,
                format!("source file `{}` is outside the package root", file_path.display()),
                "every source file must live under the directory containing `package.toml`",
                diags,
                lint_cfg,
            );
            return None;
        }
    };

    if !has_edda_extension(rel) {
        emit_resolution_error(
            file_span,
            format!("source file `{}` does not have a `.{EDDA_EXT}` extension", rel.display()),
            "every Edda source file must end in `.ea`",
            diags,
            lint_cfg,
        );
        return None;
    }

    let components = collect_normal_components(rel, file_span, diags, lint_cfg)?;
    let stem = strip_extension(components.last().expect("non-empty after extension check"));

    let (skip_first, prefix_subtree) = match components.first().map(String::as_str) {
        Some(SRC_DIR) => (true, None),
        Some(s @ (TESTS_DIR | BENCH_DIR | EXAMPLES_DIR | CODEGEN_DIR)) => {
            (false, Some(cx.interner.intern(s)))
        }
        _ => {
            emit_resolution_error(
                file_span,
                format!("source file `{}` is not in a recognized package subtree", rel.display()),
                "expected `src/`, `tests/`, `bench/`, `examples/`, or `codegen/` at the package root",
                diags,
                lint_cfg,
            );
            return None;
        }
    };

    let mut segments: Vec<Symbol> = owning.canonical_root_path.segments().to_vec();
    if let Some(sub) = prefix_subtree {
        segments.push(sub);
    }
    let body_iter = components.iter().enumerate().filter_map(|(idx, c)| {
        if idx == 0 && skip_first {
            return None;
        }
        if idx == components.len() - 1 {
            return None;
        }
        Some(cx.interner.intern(c.as_str()))
    });
    segments.extend(body_iter);
    segments.push(cx.interner.intern(stem));

    Some(ModulePath::new(segments))
}

fn has_edda_extension(rel: &Path) -> bool {
    rel.extension().and_then(|e| e.to_str()) == Some(EDDA_EXT)
}

fn strip_extension(filename: &str) -> &str {
    match filename.rfind('.') {
        Some(i) => &filename[..i],
        None => filename,
    }
}

/// Collect path components as owned strings, rejecting non-Normal
/// components (`..`, drive prefixes, etc.) and non-UTF-8 names.
fn collect_normal_components(
    rel: &Path,
    file_span: Span,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for comp in rel.components() {
        match comp {
            Component::Normal(os) => match os.to_str() {
                Some(s) => out.push(s.to_owned()),
                None => {
                    emit_resolution_error(
                        file_span,
                        format!("source path `{}` contains non-UTF-8 segments", rel.display()),
                        "source-file paths must be UTF-8 to derive a module identity",
                        diags,
                        lint_cfg,
                    );
                    return None;
                }
            },
            _ => {
                emit_resolution_error(
                    file_span,
                    format!("source path `{}` contains non-normal components", rel.display()),
                    "expected a plain, package-relative path with no `..` or root prefixes",
                    diags,
                    lint_cfg,
                );
                return None;
            }
        }
    }
    if out.is_empty() {
        emit_resolution_error(
            file_span,
            "source path is empty".to_string(),
            "every source file must live in a package subtree (`src/`, `tests/`, ...)",
            diags,
            lint_cfg,
        );
        return None;
    }
    Some(out)
}
