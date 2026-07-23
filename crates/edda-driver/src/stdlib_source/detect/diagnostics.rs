//! Operator-facing diagnostics explaining the resolved stdlib source —
//! the info note on a working-tree win, and the warn notes for an empty
//! walk or a missing root.

use std::path::Path;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig, Severity};
use edda_span::Span;

use super::super::StdlibSource;
use super::discover::{env_stdlib_override, vendored_stdlib_root};

/// Emit at most one diagnostic explaining the [`StdlibSource`] selection
/// to the operator. `module_count` is the number of stdlib modules the
/// downstream [`build_stdlib_index`] actually registered from the
/// selected source — passing it lets the emitter distinguish a healthy
/// selection from a "live root but empty walk" case
/// that previously produced only the
/// per-import cascade with no upstream signal.
///
/// - [`StdlibSource::WorkingTree`] with `module_count > 0` → one info
///   note via `GcRecoverable` naming the chosen working-tree path (and
///   the compile-time-baked vendored path that was skipped, when one
///   was configured at build time).
/// - [`StdlibSource::Vendored`] with `module_count > 0` → silent. This
///   is the normal consumer build and needs no announcement.
/// - [`StdlibSource::WorkingTree`] / [`StdlibSource::Vendored`] with
///   `module_count == 0` → one warn note via `ImportResolutionError`
///   naming the root that was walked (and finding nothing) plus the
///   `EDDA_STDLIB_ROOT` escape hatch. Covers the
///   silent failure: e.g. a baked vendored
///   path that points at an emptied / uninitialised submodule
///   directory.
/// - [`StdlibSource::None`] → one warn note via `ImportResolutionError`
///   naming the baked path that did not exist and pointing the operator
///   at the `EDDA_STDLIB_ROOT` env var escape hatch.
///   Without this note the operator only
///   sees the cascade of `unresolved stdlib import …` errors and has no
///   signal that the root cause is upstream.
pub(crate) fn emit_stdlib_source_selection(
    source: &StdlibSource,
    module_count: usize,
    diagnostics: &mut Diagnostics,
    lint_cfg: &LintConfig,
) {
    match source {
        StdlibSource::WorkingTree(chosen) if module_count == 0 => {
            emit_empty_source_warn(chosen, "active worktree", diagnostics, lint_cfg);
        }
        StdlibSource::Vendored(chosen) if module_count == 0 => {
            emit_empty_source_warn(chosen, "vendored stdlib root", diagnostics, lint_cfg);
        }
        StdlibSource::WorkingTree(chosen) => {
            let class = DiagnosticClass::GcRecoverable;
            let _ = lint_cfg;
            let message = match vendored_stdlib_root() {
                Some(vendored) => format!(
                    "stdlib source is the active worktree at {}; skipping vendored copy at {}",
                    chosen.display(),
                    vendored.display(),
                ),
                None => format!(
                    "stdlib source is the active worktree at {}",
                    chosen.display(),
                ),
            };
            diagnostics.push(Diagnostic::new(class, Severity::Info, Span::DUMMY, message));
        }
        StdlibSource::None => {
            let class = DiagnosticClass::ImportResolutionError;
            let _ = lint_cfg;
            // Describe whichever candidate was actually rejected:
            // - env var set with bad path: it short-circuits the baked
            //   path (per resolve_stdlib_source precedence), so the
            //   message only mentions the env var
            // - env var unset: the baked path was checked and rejected
            let cause = match (env_stdlib_override(), vendored_stdlib_root()) {
                (Some(p), _) => format!(
                    "`EDDA_STDLIB_ROOT` is set to `{}` which does not exist on disk",
                    p.display(),
                ),
                (None, Some(baked)) => format!(
                    "compile-time-baked path `{}` does not exist and `EDDA_STDLIB_ROOT` is unset",
                    baked.display(),
                ),
                (None, None) => {
                    "no stdlib tree was discovered and `EDDA_STDLIB_ROOT` is unset".to_string()
                }
            };
            let message = format!(
                "could not locate the stdlib source root: {}; all `std.*` imports will fail",
                cause,
            );
            diagnostics.push(
                Diagnostic::new(class, Severity::Warn, Span::DUMMY, message).with_note(
                    "set `EDDA_STDLIB_ROOT=<path/to/stdlib>` to point the bootstrap at a known stdlib tree",
                ),
            );
        }
        StdlibSource::Vendored(_) => {}
    }
}

/// Push the empty-source warn — the
/// [`StdlibSource`] resolver chose a live root but the
/// [`build_stdlib_index`] walk registered zero modules. Names the
/// chosen root + the role label that picked it (`active worktree` vs
/// `vendored stdlib root`) and points the operator at the
/// `EDDA_STDLIB_ROOT` escape hatch so the silent "the bundled stdlib
/// does not expose this module" cascade now has an upstream signal.
fn emit_empty_source_warn(
    chosen: &Path,
    role: &str,
    diagnostics: &mut Diagnostics,
    lint_cfg: &LintConfig,
) {
    let class = DiagnosticClass::ImportResolutionError;
    let _ = lint_cfg;
    let message = format!(
        "stdlib source root at {} ({}) registered zero modules; all `std.*` imports will fail",
        chosen.display(),
        role,
    );
    diagnostics.push(
        Diagnostic::new(class, Severity::Warn, Span::DUMMY, message).with_note(
            "set `EDDA_STDLIB_ROOT=<path/to/stdlib>` to point the bootstrap at a known-good stdlib tree",
        ),
    );
}
