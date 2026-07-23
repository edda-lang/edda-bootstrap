//! Cascade orchestrator.
//!
//! [`run`] walks the [`Stage`] sequence in `build-system.md` §5 order
//! against the active [`RunPlan`]. The current driver runs:
//!
//! - **Parse + import-resolve** through [`edda_resolve::build_source_graph`] +
//!   [`edda_resolve::build_resolved_package`].
//! - **Typecheck** through [`edda_types::check_package`], capturing the
//!   resulting [`edda_types::TypedPackage`] for downstream stages.
//! - **Codegen** (spec instantiation per Edda's vocabulary) through
//!   [`crate::codegen::drive_codegen`], which iterates the typed
//!   package's implicit + explicit spec invocations and stages
//!   materialised `.ea` artifacts under `.edda/cache/codegen/` via
//!   [`edda_codegen::CodegenSession`].
//! - **Compile** runs [`edda_mir::lower`] (typed HIR → MIR), dumps a
//!   pretty-printed `.mir` artifact under
//!   `target/edda/<triple>/<package>.mir` for inspection, then drives
//!   [`edda_compile::Emitter::compile_program_to_object`] to emit the
//!   native object file at `target/edda/<triple>/<package>.o`.
//! - **Link** is wired to `edda-link` via [`Driver::compile_outputs`]:
//!   every object file the compile stage writes is appended to that
//!   slot, and `drive_link` invokes the per-target linker over the
//!   slot's contents.
//!
//! ## Stage data flow
//!
//! ```text
//!     ResolvedPackage -> TypedPackage -> [codegen artifacts] -> MirProgram -> .o -> exe
//!     (driver.resolved)  (local var)     (.edda/cache/codegen/) (local)      (compile_outputs)  (link out)
//! ```
//!
//! Each stage's output is held only as long as a downstream stage may
//! consume it; nothing accumulates on the [`Driver`] beyond
//! [`Driver::resolved`] which the daemon-bound surface continues to
//! query. After codegen stages its artifacts, the cascade re-runs
//! resolve + typecheck over the union of the original entry files and
//! the codegen-emitted paths; `drive_compile` then operates on the
//! pass-2 [`TypedPackage`] so MIR lowering can reference functions and
//! types inside the materialised spec modules.

mod compile;
mod link;
mod resolve;
mod str_eq_support;

use compile::drive_compile;
use link::drive_link;
use resolve::{drive_resolve, drive_typecheck, finalize_unused_import_lint};

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use edda_compile::CompileError;
use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_mir::MirError;
use edda_span::Span;

use crate::command::Command;
use crate::context::Driver;
use crate::discover::discover_source_files;
use crate::outcome::{ExitCode, Outcome, Summary};
use crate::plan::{RunPlan, Stage};

pub(super) const STAGE_IMPORT_RESOLVE: &str = "import_resolve";
pub(super) const STAGE_TYPECHECK: &str = "typecheck";
pub(super) const STAGE_COMPILE: &str = "compile";

/// Run the cascade described by `plan` against the prepared `driver`.
///
/// Returns an [`Outcome`] in every case: system-level early-exit
/// conditions surface as [`ExitCode::SystemError`] with an explanatory
/// diagnostic; build-level errors surface as [`ExitCode::BuildError`]
/// with the originating pass's diagnostics; otherwise
/// [`ExitCode::Success`]. The function moves
/// [`Driver::diagnostics`] into the returned outcome — the driver's
/// diagnostic take is empty after this call returns.
///
/// For [`crate::command::Command::Fmt`] the cascade short-circuits at
/// [`Stage::Parse`] and produces an empty summary.
/// [`crate::command::Command::Clean`] short-circuits the cascade but
/// first invokes [`drive_clean`] to remove every directory the build
/// pipeline emits into so subsequent invocations start from a clean
/// state (per `build-system.md` §10's `edda clean` row).
pub fn run(driver: &mut Driver, plan: &RunPlan) -> Outcome {
    let start = Instant::now();

    driver.reporter.package_started(&driver.manifest.package);

    if matches!(plan.command, Command::Clean) {
        drive_clean(driver);
        return finish(driver, &Summary::default(), start);
    }

    if !plan.command.runs_cascade() {
        return finish(driver, &Summary::default(), start);
    }

    let entry_files = match discover_source_files(&driver.src_root) {
        Ok(v) => v,
        Err(err) => {
            push_io_error(
                &mut driver.diagnostics,
                &driver.lint_cfg,
                &driver.src_root,
                &err,
            );
            return finish(driver, &Summary::default(), start);
        }
    };

    let mut modules_total = drive_resolve(driver, &entry_files);

    //
    // For typecheck the gate is "no resolver / parser errors yet" — the
    // pass-1 → codegen → pass-2 cascade below is allowed to *produce*
    // typecheck errors on pass-1 (mini-lisp's missing spec members), so
    // we cannot also gate on the absence of those.
    let pre_typecheck_diag_len = driver.diagnostics.len();
    let pre_typecheck_had_errors = driver.diagnostics.has_errors();
    let typed_pass1 = if plan.stop_after >= Stage::Typecheck && !pre_typecheck_had_errors {
        drive_typecheck(driver)
    } else {
        None
    };
    let post_typecheck_diag_len = driver.diagnostics.len();

    // Codegen runs against the typed package and stages `.ea` artifacts
    // under `.edda/cache/codegen/`. We deliberately let codegen run even
    // when pass-1 typecheck produced errors: `TypedPackage::spec_invocations`
    // and `TypedPackage::implicit_specs` are populated regardless of
    // unrelated typecheck failures, and pass-2 below re-runs typecheck
    // against the expanded source set so any spec-related pass-1 errors
    // can clear.
    let mut artifacts_generated: usize = 0;
    let mut artifacts_cached: usize = 0;
    let mut codegen_paths: Vec<PathBuf> = Vec::new();
    if plan.stop_after >= Stage::Codegen && !pre_typecheck_had_errors {
        if let Some(t) = typed_pass1.as_ref() {
            let report = crate::codegen::drive_codegen(driver, t);
            artifacts_generated = artifacts_generated.saturating_add(report.artifacts_generated);
            artifacts_cached = artifacts_cached.saturating_add(report.artifacts_cached);
            codegen_paths = report.generated_paths;
        }
    }

    // C9: `edda test --properties` synthesis. Discovers every function
    // with at least one `requires`/`ensures` clause in the resolved
    // AST, runs the C8 strategy table per parameter, and writes a
    // synthesised runner module to disk that invokes each target with
    // its generated input set and asserts the `ensures` predicate at
    // each call site. The synthesised file gets appended to
    // `codegen_paths` so the Pass-2 reentry harness below picks it up
    // and typechecks it alongside the codegen-expanded source set.
    if plan.command == crate::command::Command::Test && driver.options.properties {
        if let Some(resolved) = driver.resolved.as_ref() {
            let targets = crate::properties::discover_targets(resolved, &driver.interner);
            if let Some(source) =
                crate::properties::synthesize_runner_source(&targets, 5)
            {
                let path = crate::properties::runner_module_path(&driver.package_root);
                match crate::properties::write_runner_module(&path, &source) {
                    Ok(_) => codegen_paths.push(path),
                    Err(err) => {
                        let _ = err;
                        // I/O errors are non-fatal — the rest of the
                        // cascade continues without the property
                        // runner.
                    }
                }
            }
        }
    }

    // Pass-2: re-resolve + re-typecheck against the expanded source set.
    // The pass-1 typecheck diagnostics (indices
    // `[pre_typecheck_diag_len, post_typecheck_diag_len)`) are discarded —
    // pass-2 is authoritative. Codegen-stage diagnostics pushed after
    // `post_typecheck_diag_len` (e.g. an implicit-spec miss surfaced by
    // `crate::codegen::support::emit_typecheck`) are not pass-1 typecheck
    // output, so they are snapshotted out before the truncate and spliced
    // back in after — otherwise a codegen diagnostic silently vanished
    // whenever the same build staged at least one other artifact.
    let typed = if !codegen_paths.is_empty() {
        let mut items = std::mem::take(&mut driver.diagnostics).into_vec();
        let codegen_diags = items.split_off(post_typecheck_diag_len);
        items.truncate(pre_typecheck_diag_len);
        for d in items.into_iter().chain(codegen_diags) {
            driver.diagnostics.push(d);
        }
        let mut entry_files_expanded: Vec<PathBuf> =
            Vec::with_capacity(entry_files.len() + codegen_paths.len());
        entry_files_expanded.extend(entry_files.iter().cloned());
        entry_files_expanded.extend(codegen_paths.iter().cloned());
        modules_total = drive_resolve(driver, &entry_files_expanded);
        if plan.stop_after >= Stage::Typecheck && !driver.diagnostics.has_errors() {
            drive_typecheck(driver)
        } else {
            None
        }
    } else {
        typed_pass1
    };

    // Bug C12: emit `unused_import` *after* typecheck so method-call
    // desugar contributions ("`receiver.method(...)` routed through a
    // free function in an imported module") suppress the warning
    // correctly. The resolver no longer emits the lint inline; it now
    // accumulates a per-module used-leaf set on `ResolvedPackage`, and
    // this step folds in the typechecker's method-resolution map
    // before the lint walks every import.
    finalize_unused_import_lint(driver, typed.as_ref());

    // Codex `06-tooling.md` §5: emit structure-map files as a side-effect
    // of the cascade when either (a) the verb is `Command::Structmap` or
    // (b) the active manifest opts in via `build.emit_structmap` (default
    // `true`). Runs after the final typecheck so the emitter sees the
    // post-codegen-expansion typed package.
    if should_emit_structmap(driver, plan) {
        if let Some(t) = typed.as_ref() {
            crate::structmap::drive_structmap(driver, t);
        }
    }

    if plan.stop_after >= Stage::Compile && !driver.diagnostics.has_errors() {
        if let Some(t) = typed.as_ref() {
            if let Some(report) = drive_compile(driver, t) {
                artifacts_generated =
                    artifacts_generated.saturating_add(report.artifacts_generated);
            }
        }
    }

    // Link is wired to `edda-link`. `drive_compile` populates
    // `driver.compile_outputs` via `write_object`; `drive_link`
    // short-circuits only when that step failed or was skipped.
    if plan.stop_after >= Stage::Link && !driver.diagnostics.has_errors() {
        drive_link(driver);
    }

    let summary = Summary {
        modules_total,
        modules_compiled: modules_total,
        artifacts_cached,
        artifacts_generated,
        elapsed: Duration::ZERO,
    };
    finish(driver, &summary, start)
}

/// Implement `edda clean` per `build-system.md` §10.
///
/// Removes the three build-emitted roots — `codegen/`, `.edda/`, and
/// `target/edda/` — under the package root. Each removal is independent
/// and best-effort: a failure on one root does not prevent the others
/// from being attempted, but does emit a `parse_error` diagnostic so the
/// caller observes a non-zero exit code.
fn drive_clean(driver: &mut Driver) {
    const TARGETS: &[&str] = &["codegen", ".edda", "target/edda"];
    for relative in TARGETS {
        let mut dir = driver.package_root.clone();
        for seg in relative.split('/') {
            dir.push(seg);
        }
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => push_io_error(&mut driver.diagnostics, &driver.lint_cfg, &dir, &err),
        }
    }
}

fn should_emit_structmap(driver: &Driver, plan: &RunPlan) -> bool {
    if matches!(plan.command, Command::Structmap) {
        return true;
    }
    driver.manifest.build.emit_structmap
}

fn finish(driver: &mut Driver, summary: &Summary, start: Instant) -> Outcome {
    let summary = Summary {
        elapsed: start.elapsed(),
        ..*summary
    };
    crate::render::resolve_labels(&mut driver.diagnostics, &driver.source_map);
    let diagnostics = std::mem::take(&mut driver.diagnostics);
    let exit_code = if diagnostics.has_errors() {
        ExitCode::BuildError
    } else {
        ExitCode::Success
    };
    driver.reporter.summary(&summary);
    Outcome {
        diagnostics,
        summary,
        exit_code,
    }
}

pub(super) fn push_io_error(
    diagnostics: &mut Diagnostics,
    lint_cfg: &LintConfig,
    path: &Path,
    err: &std::io::Error,
) {
    let class = DiagnosticClass::ParseError;
    let severity = lint_cfg.effective(class);
    let message = format!(
        "cannot access path `{}`: {err}",
        path.display(),
    );
    diagnostics.push(Diagnostic::new(class, severity, Span::DUMMY, message));
}

/// Project a [`CompileError`] to a [`Diagnostic`].
///
/// Routes through [`CompileError::to_diagnostic`], then re-runs the
/// effective-severity gate so suppressed classes drop silently.
pub(super) fn push_compile_error(
    diagnostics: &mut Diagnostics,
    lint_cfg: &LintConfig,
    err: &CompileError,
) {
    let diag = err.to_diagnostic();
    let severity = lint_cfg.effective(diag.class);
    let diag = Diagnostic::new(diag.class, severity, diag.primary.span, diag.message);
    diagnostics.push(diag);
}

/// Project a [`MirError`] to a [`Diagnostic`].
///
/// Routes through [`MirError::class`] (currently `TypecheckError` for
/// every variant — both validation and lowering failures share that
/// class until §8 grows a MIR-specific class). The diagnostic's
/// message is the error's `Display` rendering; structural error
/// variants encode `body<N> block<M>` coordinates that human readers
/// can resolve against the dumped `.mir` artifact. Lowering errors
/// carry their originating HIR span; validation errors do not, and
/// fall back to [`Span::DUMMY`] via [`MirError::span`].
pub(super) fn push_mir_error(
    diagnostics: &mut Diagnostics,
    lint_cfg: &LintConfig,
    err: &MirError,
) {
    let class = err.class();
    let severity = lint_cfg.effective(class);
    diagnostics.push(Diagnostic::new(
        class,
        severity,
        err.span(),
        err.to_string(),
    ));
}
