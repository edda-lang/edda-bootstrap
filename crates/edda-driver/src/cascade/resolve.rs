//! Cascade resolve + typecheck stages: parse-and-resolve into a
//! `ResolvedPackage`, the post-typecheck `unused_import` lint
//! finalization, and the typecheck pass producing the `TypedPackage`.

use std::path::PathBuf;
use std::sync::Arc;

use edda_resolve::{
    FsLoader, ResolveCx, ResolvedSourceGraph, build_resolved_package, build_source_graph,
};
use edda_types::TypedPackage;

use crate::context::Driver;
use crate::reporter::Reporter;

use super::{STAGE_IMPORT_RESOLVE, STAGE_TYPECHECK};

/// Run the combined parse-and-resolve pass and stash the resolved
/// package on the driver. Returns the resolved module count for the
/// summary line.
pub(super) fn drive_resolve(driver: &mut Driver, entry_files: &[PathBuf]) -> usize {
    let graph = build_graph(driver, entry_files);
    let module_count = graph.len();
    let resolved = {
        let cx = ResolveCx {
            layout: &driver.layout,
            deps: &driver.deps,
            stdlib: &driver.stdlib,
            interner: &driver.interner,
        };
        build_resolved_package(graph, &cx, &mut driver.diagnostics, &driver.lint_cfg)
    };

    notify_modules_done(&driver.reporter, &resolved, &driver.interner, STAGE_IMPORT_RESOLVE);
    driver.resolved = Some(resolved);
    module_count
}

fn build_graph(driver: &mut Driver, entry_files: &[PathBuf]) -> ResolvedSourceGraph {
    let cx = ResolveCx {
        layout: &driver.layout,
        deps: &driver.deps,
        stdlib: &driver.stdlib,
        interner: &driver.interner,
    };
    build_source_graph(
        entry_files,
        &cx,
        &FsLoader,
        &driver.source_map,
        &mut driver.diagnostics,
        &driver.lint_cfg,
    )
}

fn notify_modules_done(
    reporter: &Arc<dyn Reporter>,
    resolved: &edda_resolve::ResolvedPackage,
    interner: &edda_intern::Interner,
    stage: &str,
) {
    for entry in resolved.graph().modules() {
        let path_text = entry.canonical_path.to_owned_string(interner);
        reporter.module_done(&path_text, stage);
    }
}

/// Bug C12 surface — finalize the `unused_import` lint after
/// typecheck has produced its method-resolution map.
///
/// For every `(method_call_span, resolved_binding)` in the typed
/// package, find the importer module (from the span's file id) and
/// the callee's owning module (from the binding's
/// `BindingId::module`). If the importer has an import-leaf pointing
/// at the callee module, mark that leaf used so the lint walk does
/// not over-fire.
pub(super) fn finalize_unused_import_lint(driver: &mut Driver, typed: Option<&TypedPackage>) {
    let trust_points = driver.options.lint_trust_points;
    let capability_safe_stdlib = driver.options.lint_capability_safe_stdlib;
    let Some(resolved) = driver.resolved.as_mut() else {
        return;
    };
    if let Some(t) = typed {
        contribute_method_resolutions(resolved, t);
    }
    let cx = edda_resolve::ResolveCx {
        layout: &driver.layout,
        deps: &driver.deps,
        stdlib: &driver.stdlib,
        interner: &driver.interner,
    };
    edda_resolve::emit_unused_import_lints(
        resolved,
        &cx,
        &mut driver.diagnostics,
        &driver.lint_cfg,
    );
    edda_resolve::emit_binding_should_be_let_lints(
        resolved,
        &cx,
        &mut driver.diagnostics,
        &driver.lint_cfg,
    );
    edda_resolve::emit_exec_scope_without_spawn_lints(
        resolved,
        &cx,
        &mut driver.diagnostics,
        &driver.lint_cfg,
    );
    edda_resolve::emit_unused_closure_capture_lints(
        resolved,
        &cx,
        &mut driver.diagnostics,
        &driver.lint_cfg,
    );
    edda_resolve::emit_duplicate_spec_invocation_lints(
        resolved,
        &cx,
        &mut driver.diagnostics,
        &driver.lint_cfg,
    );
    edda_resolve::emit_dead_private_function_lints(
        resolved,
        &cx,
        &mut driver.diagnostics,
        &driver.lint_cfg,
    );
    edda_resolve::emit_trust_hatch_too_dense_lints(
        resolved,
        &cx,
        &mut driver.diagnostics,
        &driver.lint_cfg,
    );
    if let Some(max_trust) = driver.manifest.max_trust {
        edda_resolve::emit_trust_budget_lints(resolved, &cx, max_trust, &mut driver.diagnostics);
    }
    edda_resolve::emit_mode_overgrab_lints(
        resolved,
        &cx,
        &mut driver.diagnostics,
        &driver.lint_cfg,
    );
    // `edda lint` audit subflags (codex 06 §8) — gated, so the analyses run
    // only when explicitly requested, never on an ordinary cascade. Both are
    // resolve-AST-structural, so they run here regardless of whether the
    // later typecheck pass succeeded.
    if trust_points {
        edda_resolve::emit_trust_points_listing(resolved, &cx, &mut driver.diagnostics);
    }
    if capability_safe_stdlib {
        edda_resolve::emit_capability_safe_stdlib_lints(
            resolved,
            &cx,
            &mut driver.diagnostics,
            &driver.lint_cfg,
        );
    }
}

/// Walk every entry in `typed.method_resolutions` and report the
/// implied "this import contributed a method-resolution target"
/// signal to the resolver's used-leaf table.
fn contribute_method_resolutions(
    resolved: &mut edda_resolve::ResolvedPackage,
    typed: &TypedPackage,
) {
    for (span, binding) in typed.method_resolutions.iter() {
        let Some(importer) = resolved.graph().lookup_by_file(span.file) else {
            continue;
        };
        let callee_module = binding.module;
        if callee_module == importer {
            continue;
        }
        // Find the leaf in the importer's table whose target equals the
        // callee's owning module — that's the import the desugar
        // routed through.
        let entries: Vec<(edda_intern::Symbol, edda_resolve::ModuleId)> = resolved
            .module(importer)
            .leaf_imports
            .entries()
            .iter()
            .map(|b| (b.leaf, b.module))
            .collect();
        for (leaf, leaf_target) in entries {
            if leaf_target == callee_module {
                resolved.mark_leaf_used(importer, leaf);
            }
        }
    }
}

/// Run the typecheck pass against the resolved package on the driver
/// and return the resulting [`TypedPackage`] for downstream stages.
///
/// Calls into [`edda_types::check_package`], which builds the
/// signature + type-decl context for the whole package and then
/// type-checks every function body in place. Diagnostics flow into
/// [`Driver::diagnostics`] with their effective severity gated by
/// [`Driver::lint_cfg`].
///
/// Returns `None` only when the cascade aborted before
/// import-resolve completed and there is no resolved package to type.
/// In all other cases the typecheck pass runs to completion; the
/// caller decides whether to continue into compile based on
/// [`Diagnostics::has_errors`].
pub(super) fn drive_typecheck(driver: &mut Driver) -> Option<TypedPackage> {
    let resolved = driver.resolved.as_ref()?;
    let typed = edda_types::check_package(
        resolved,
        &driver.interner,
        &driver.ty_interner,
        &driver.lint_cfg,
        &driver.target,
        &mut driver.diagnostics,
    );
    edda_types::check_capability_availability(
        &typed,
        &driver.ty_interner,
        driver.target.triple(),
        &driver.lint_cfg,
        &mut driver.diagnostics,
    );
    notify_modules_done(&driver.reporter, resolved, &driver.interner, STAGE_TYPECHECK);
    Some(typed)
}
