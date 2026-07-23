//! `build-system.md` §4 path-resolution rules — pure functions over
//! [`PackageLayout`] / [`DepIndex`] / [`StdlibIndex`] inputs.
//!
//! Given a `Path` from an `import` declaration (or a filesystem path
//! for a source file), produce the canonical [`ModulePath`] plus the
//! expected on-disk source location, or push an
//! `import_resolution_error` diagnostic and return `None`.
//!
//! No filesystem I/O happens in this module: it is the rule layer. The
//! source-graph driver does the actual reads on top of these
//! decisions.

mod file_identity;
mod import_path;

pub use file_identity::module_identity_for_file;
pub use import_path::resolve_import_path;

use std::path::PathBuf;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_intern::{Interner, Symbol};
use edda_span::Span;
use edda_syntax::ast::Path as AstPath;

use crate::layout::{DepIndex, PackageLayout, StdlibIndex};
use crate::path::ModulePath;

/// File extension of an Edda source file. Lowercase; `module_identity_for_file`
/// matches case-sensitively.
pub(super) const EDDA_EXT: &str = "ea";

/// Package-wide read-only context for §4 path resolution. Bundles the
/// four pieces of compile-state every resolution call needs so the
/// public functions take a small, stable parameter set. The
/// [`ImporterContext`] (per-file location) is passed separately.
#[derive(Copy, Clone, Debug)]
pub struct ResolveCx<'a> {
    /// Layout of the importing package.
    pub layout: &'a PackageLayout,
    /// Resolved dependencies of the importing package, keyed by their
    /// exposed `root_namespace`.
    pub deps: &'a DepIndex,
    /// Compiler-bundled stdlib catalogue.
    pub stdlib: &'a StdlibIndex,
    /// Shared string interner — segment text rides through it.
    pub interner: &'a Interner,
}

/// Reserved top-level subtree names per `build-system.md §4`.
/// `src/` modules drop the `src` segment from their canonical path; the
/// other names appear in the module path as a literal segment after
/// the package's `root_namespace`.
pub(super) const SRC_DIR: &str = "src";
pub(super) const TESTS_DIR: &str = "tests";
pub(super) const BENCH_DIR: &str = "bench";
pub(super) const EXAMPLES_DIR: &str = "examples";
pub(super) const CODEGEN_DIR: &str = "codegen";

/// Successful import resolution: the canonical module path plus the
/// filesystem path where the importer expects to find that module's
/// source, plus which §4 rule produced the resolution.
#[derive(Clone, Debug)]
pub struct ResolvedImport {
    /// The canonical dot-path the imported module is exposed under.
    pub canonical: ModulePath,
    /// Filesystem path that should contain the module's `.ea` source.
    /// The source-graph driver reads this file via the [`crate::SourceLoader`]
    /// abstraction; a missing file at this path is
    /// reported by the driver, not by this function.
    pub expected_file: PathBuf,
    /// Which §4 rule produced this resolution.
    pub kind: ImportKind,
}

/// Classification of an import per `build-system.md §4`.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum ImportKind {
    /// `std.*` — compiler-bundled stdlib.
    Stdlib,
    /// `<root_namespace>.*` under `src/` — in-package import.
    InPackage,
    /// `<root_namespace>.tests.*` — files under `tests/`.
    Tests,
    /// `<root_namespace>.bench.*` — files under `bench/`.
    Bench,
    /// `<root_namespace>.examples.*` — files under `examples/`.
    Examples,
    /// `<root_namespace>.codegen.*` — files under `codegen/`.
    Codegen,
    /// `<dep_root_namespace>.*` — third-party package; the same
    /// subtree dispatch applies inside the dep's `root_dir`.
    ThirdParty,
    /// Bare-leaf `import value` — sibling file in importer's directory.
    Sibling,
}

/// `true` when `path` is a parser-recovery sentinel that downstream
/// stages must skip without re-diagnosing. The recovery sites in
/// `edda-syntax` emit a `parse_error` at the source span before
/// constructing such a path; this function is the resolver's seam for
/// honouring that contract.
pub(crate) fn is_recovery_path(path: &AstPath) -> bool {
    path.segments.is_empty() || path.segments[0].name == Symbol::DUMMY
}

pub(super) fn emit_resolution_error(
    span: Span,
    message: String,
    note: &'static str,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) {
    let class = DiagnosticClass::ImportResolutionError;
    let severity = lint_cfg.effective(class);
    diags.push(Diagnostic::new(class, severity, span, message).with_note(note));
}
