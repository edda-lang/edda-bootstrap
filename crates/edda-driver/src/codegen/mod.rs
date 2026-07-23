//! Cascade-stage codegen wiring — drives `edda_codegen::instantiate_spec`
//! against the spec invocations surfaced by typecheck.
//!
//! The driver-side codegen pass wires spec instantiation into the
//! cascade. The `edda-codegen` crate ships the per-invocation pipeline
//! (`instantiate_spec`) and the cascade-session lifecycle
//! (`CodegenSession`); this module sequences them against the active
//! [`TypedPackage`]:
//!
//! 1. Collect the root set — implicit `ImplicitSpec::Range` /
//!    `ImplicitSpec::Option` requests harvested by inference, plus
//!    explicit `spec Path(args)` directives at module scope.
//! 2. For each root: resolve the spec declaration AST, build an
//!    [`ArgumentTuple`], call `instantiate_spec` against an open
//!    [`CodegenSession`].
//! 3. Commit the session, propagating any `CodegenError` through
//!    `Driver::diagnostics`.
//!
//! After this pass stages its artifacts, [`crate::cascade::run`]
//! re-runs resolve + typecheck over the union of the original entry
//! files and `CodegenReport::generated_paths`, then drives MIR
//! lowering against the pass-2 [`TypedPackage`]. Programs that
//! reference functions or types inside the materialised spec modules
//! resolve cleanly through that second pass.

use std::path::PathBuf;

use edda_cache::{ArtifactHash, BodyVersion, CacheRoots, Store};
use edda_codegen::{
    ArgumentTuple, CodegenError, CodegenSession, instantiate_spec,
};
use edda_diag::{Diagnostics, LintConfig};
use edda_intern::{Interner, Symbol};
use edda_resolve::{
    BindingId, ResolvedPackage,
};
use edda_syntax::ast::{
    Item, ItemKind, Spec,
};
use edda_types::{
    TyInterner, TypedPackage,
};
use smol_str::SmolStr;
use time::OffsetDateTime;

use crate::context::Driver;

mod arguments;
mod debug_formatter;
mod derive;
mod eq_comparator;
mod qualified_name;
mod resolver;
mod spec_decl;
mod support;

pub(crate) use debug_formatter::build_debug_formatter_map;
pub(crate) use eq_comparator::build_eq_comparator_map;

use derive::{collect_derive, collect_explicit, collect_implicit};
use qualified_name::{
    binding_qualified_name, resolve_path_to_qualified, spec_invocation_qualified_name,
};
use resolver::DriverResolver;
use spec_decl::{find_spec_decl, root_from_substituted_invocation, SpecLookup};
use support::{classify_tier, dedup_key, emit_typecheck, push_codegen_error};

/// Side of the cascade summary the codegen stage populates.
#[derive(Clone, Default, Debug)]
pub(crate) struct CodegenReport {
    /// Number of fresh artifacts written to disk this cascade.
    pub artifacts_generated: usize,
    /// Number of staged-then-deduplicated artifacts (same hash twice
    /// within one session).
    pub artifacts_cached: usize,
    /// Absolute `final_path` of every artifact successfully staged this
    /// cascade. The cascade-restart follow-up consumes this list as additional
    /// entry files for the second-pass resolve + typecheck.
    pub generated_paths: Vec<PathBuf>,
}

/// Drive the codegen stage of the cascade against `typed`.
///
/// Walks `typed.implicit_specs` and `typed.spec_invocations` to build
/// the root set, opens a [`Store`] under `driver.package_root`, begins
/// a [`CodegenSession`] at [`BodyVersion::CURRENT`], invokes
/// [`instantiate_spec`] per root, and commits. Failures project to
/// `Driver::diagnostics` through `CodegenError::to_diagnostic` gated
/// by `Driver::lint_cfg`; the cascade continues past codegen errors
/// rather than aborting the rest of the build.
pub(crate) fn drive_codegen(driver: &mut Driver, typed: &TypedPackage) -> CodegenReport {
    let resolved = match driver.resolved.as_ref() {
        Some(r) => r,
        None => return CodegenReport::default(),
    };

    let initial_roots = collect_roots(
        typed,
        resolved,
        &driver.interner,
        &driver.ty_interner,
        &mut driver.diagnostics,
        &driver.lint_cfg,
    );
    if initial_roots.is_empty() {
        return CodegenReport::default();
    }

    let now = OffsetDateTime::now_utc();
    let store = match Store::open(
        CacheRoots::new(driver.package_root.clone()),
        driver.manifest.package.as_ref(),
        now,
    ) {
        Ok(s) => s,
        Err(err) => {
            push_codegen_error(
                &mut driver.diagnostics,
                &driver.lint_cfg,
                &CodegenError::from(err),
            );
            return CodegenReport::default();
        }
    };

    let mut session = match CodegenSession::begin(
        &store,
        driver.package_root.clone(),
        BodyVersion::CURRENT,
        "edda-codegen 0.0.0",
    ) {
        Ok(s) => s,
        Err(err) => {
            push_codegen_error(&mut driver.diagnostics, &driver.lint_cfg, &err);
            return CodegenReport::default();
        }
    };

    let resolver = DriverResolver {
        resolved,
        interner: &driver.interner,
    };
    //   it sees the same chosen root that fed `StdlibIndex` construction;
    //   re-deriving the path here would risk drift from the working-tree-
    //   wins precedence selected at driver init (B-023 / upstream #9).
    let stdlib_root: Option<PathBuf> = driver
        .stdlib_source
        .root()
        .map(std::path::Path::to_path_buf);
    let mut report = CodegenReport::default();
    //   one or more roots in this cascade; the first surfacing counts
    //   as `artifacts_generated`, every subsequent one as
    //   `artifacts_cached`. The session itself dedupes at the staging
    //   layer (B15: a duplicate `(spec, args)` returns the cached
    //   `StagedArtifact`); this local set keeps the user-facing summary
    //   counters and the pass-2 entry-files list in lockstep with the
    //   on-disk materialisation.
    let mut seen_hashes: std::collections::BTreeSet<ArtifactHash> =
        std::collections::BTreeSet::new();
    //   formatted via `dedup_key`; ensures a `(spec, args)` pair is processed
    //   at most once per cascade even if it is demanded by multiple parents
    //   and bounds the worklist against any spec-body cycle the parser
    //   should have rejected
    let mut visited_keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    //   declaration order, then any nested invocation each instantiation
    //   surfaces lands at the back so parents always commit before their
    //   children
    let mut worklist: std::collections::VecDeque<RootInvocation> = initial_roots.into();

    while let Some(root) = worklist.pop_front() {
        let key = dedup_key(&root.spec_qualified, &root.args);
        if !visited_keys.insert(key) {
            continue;
        }
        let tier = classify_tier(&root.source_path, stdlib_root.as_deref());
        match instantiate_spec(
            &mut session,
            &root.spec_decl,
            &root.spec_qualified,
            &root.args,
            tier,
            &resolver,
            &driver.interner,
            now,
            &root.parent_imports,
            &root.parent_qualified,
            &root.parent_sibling_names,
        ) {
            Ok((staged, nested_invocations)) => {
                if seen_hashes.insert(staged.hash) {
                    report.artifacts_generated += 1;
                    report.generated_paths.push(staged.final_path);
                } else {
                    // Same canonical form (`(spec_qualified, args)`)
                    // surfaced through more than one root — e.g. the
                    // user wrote `spec Option(T)` AND another spec
                    // (IntMap, StringMap, …) whose body transitively
                    // names `Option(T)`. The cascade-session collapsed
                    // both demands to one on-disk artifact; we count
                    // the second as cached and skip the path entry so
                    // pass-2 sees one entry-file per artifact.
                    report.artifacts_cached += 1;
                }
                // Enqueue any spec invocation the substituted body
                // demanded transitively.
                for nested in &nested_invocations {
                    if let Some(child) = root_from_substituted_invocation(
                        nested,
                        resolved,
                        &driver.interner,
                        &driver.ty_interner,
                        &mut driver.diagnostics,
                        &driver.lint_cfg,
                    ) {
                        worklist.push_back(child);
                    }
                }
            }
            Err(CodegenError::DuplicateStaged { .. }) => {
                // Retained for forward-compat: the current session no
                // longer returns this variant, but older builds /
                // alternative session impls might. Treat as cached.
                report.artifacts_cached += 1;
            }
            Err(err) => {
                push_codegen_error(&mut driver.diagnostics, &driver.lint_cfg, &err);
            }
        }
    }

    if let Err(err) = session.commit(now) {
        push_codegen_error(&mut driver.diagnostics, &driver.lint_cfg, &err);
    }

    report
}

/// One concrete spec invocation the codegen stage will materialise.
struct RootInvocation {
    /// Resolved qualified name (`"std.option.Option"`), used as the
    /// `spec_qualified` input to `CanonicalForm`.
    spec_qualified: SmolStr,
    /// Comptime argument tuple in declaration order.
    args: ArgumentTuple,
    /// Cloned spec declaration AST whose body is substituted.
    spec_decl: Spec,
    /// On-disk path of the source file the spec declaration lives in.
    /// Used to classify repo-tier vs cache-tier placement (stdlib
    /// invocations land in cache-tier; project-local ones in repo-tier).
    source_path: PathBuf,
    /// Cloned `ItemKind::Import` items from the spec's parent module,
    /// in source order. The generated artifact is materialised as a
    /// sibling module under the spec's parent path (e.g.
    /// `std.alloc.Box_Expr` for `spec std.alloc.Box(Expr)`); without
    /// these imports the artifact loses access to the parent module's
    /// dependencies (`std.alloc`'s `import std.alloc` shim, etc.) and
    /// fails to resolve in the pass-2 cascade.
    parent_imports: Vec<Item>,
    /// Parent module's qualified dotted path (e.g. `"std.alloc"` for a
    /// spec declared in `std.alloc`). Used by `instantiate_spec` to
    /// synthesize a `import <parent>` so bare references to parent-module
    /// siblings (`AllocError`) can be re-qualified through the parent's
    /// leaf (`alloc.AllocError`).
    parent_qualified: SmolStr,
    /// Names of every top-level non-spec / non-import item in the spec's
    /// parent module (type-decls, functions, let-decls). Each one gets
    /// rewritten in the substituted spec body from bare `<name>` to
    /// `<parent_leaf>.<name>` via [`SubstitutionMap::with_parent_siblings`].
    parent_sibling_names: Vec<Symbol>,
}

/// Walk `typed.implicit_specs` + `typed.spec_invocations` and build the
/// codegen root set, emitting diagnostics for any invocation that fails
/// argument validation or spec-decl resolution.
fn collect_roots(
    typed: &TypedPackage,
    resolved: &ResolvedPackage,
    interner: &Interner,
    ty_interner: &TyInterner,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Vec<RootInvocation> {
    let mut roots = Vec::with_capacity(typed.implicit_specs.len() + typed.spec_invocations.len());

    for req in &typed.implicit_specs {
        if let Some(root) = collect_implicit(req.kind, req.type_arg, req.span, resolved, interner, ty_interner, diags, lint_cfg) {
            roots.push(root);
        }
    }

    for si in &typed.spec_invocations {
        if let Some(root) = collect_explicit(si, resolved, interner, ty_interner, diags, lint_cfg) {
            roots.push(root);
        }
    }

    //   every derive so a nominal type reached from multiple `derive eq`
    //   targets is materialised once; the worklist's `(spec, args)` dedup
    //   would catch repeats anyway, but the shared set keeps the closure
    //   walk linear in the number of types rather than per-derive
    let mut eq_cascade_visited: std::collections::HashSet<BindingId> =
        std::collections::HashSet::new();
    //   every derive, independent of the eq set — a nominal type reached from
    //   a `derive debug` field/payload is materialised once as a `debug_<S>`
    //   formatter, even when the same type is also an eq-cascade member
    let mut debug_cascade_visited: std::collections::HashSet<BindingId> =
        std::collections::HashSet::new();
    for module in resolved.graph().modules() {
        for item in &module.ast.items {
            let ItemKind::Derive(derive) = &item.kind else {
                continue;
            };
            collect_derive(
                derive,
                resolved,
                interner,
                ty_interner,
                &typed.ty_cx,
                diags,
                lint_cfg,
                &mut roots,
                &mut eq_cascade_visited,
                &mut debug_cascade_visited,
            );
        }
    }

    roots
}
