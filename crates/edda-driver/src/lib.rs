//! Build orchestrator; library form of `edda build` and `edda check`.
//!
//! Reads `package.toml`, builds the source + artifact graphs, runs the
//! cascade (parse → import-resolve → typecheck → codegen → compile → link)
//! against the per-command root set, and surfaces aggregated diagnostics.
//! Module-level parallelism within each pass is implemented here.
//!
//! Both batch invocations (`edda build`) and the daemon's serve loop reach
//! through this crate; per `docs/tooling/build-system.md` §1, the two
//! surfaces produce identical outputs.
//!
//! Implements:
//!   - `docs/tooling/build-system.md` §2 (compilation units, build graph)
//!   - `docs/tooling/build-system.md` §5 (cascade order, parallelism,
//!     reachability-driven codegen)
//!
//! # Stage status
//!
//! - **Parse + import-resolve**: wired through
//!   [`edda_resolve::build_source_graph`] +
//!   [`edda_resolve::build_resolved_package`].
//! - **Typecheck**: wired through [`edda_types::check_package`], whose
//!   resulting [`edda_types::TypedPackage`] carries the typed HIR
//!   downstream.
//! - **Compile**: wired through [`edda_mir::lower`] (typed HIR → MIR)
//!   plus [`edda_compile::Emitter::compile_program_to_object`] (MIR →
//!   LLVM IR → native object). Both a pretty-printed `.mir` artifact
//!   (for inspection) and a `.o` object file land under
//!   `target/edda/<triple>/<package>.{mir,o}`. Requires the `llvm`
//!   cargo feature on `edda-compile`, enabled by this crate's
//!   manifest.
//! - **Codegen (spec instantiation per Edda's vocabulary)**: wired
//!   through [`edda_codegen::instantiate_spec`] against a
//!   [`edda_cache::Store`]-backed [`edda_codegen::CodegenSession`].
//!   Generated `.ea` artifacts land under `.edda/cache/codegen/`
//!   (repo-tier for project-local invocations, cache-tier for stdlib
//!   ones). The cascade then runs a pass-2 re-resolve + re-typecheck over
//!   the union of the entry files and the codegen-emitted paths, and
//!   `drive_compile` consumes that authoritative pass-2 package.
//! - **Link**: wired to `edda-link`. The compile stage now populates
//!   [`Driver::compile_outputs`] with object-file paths via
//!   [`edda_compile::Emitter::compile_program_to_object`], so the
//!   cascade routinely invokes the per-target linker through
//!   [`LinkPlan`]. Linking may still fail at runtime on cross-compile
//!   without a sysroot, but the wiring is complete.
//!
//! The public surface (the [`Command`] enum, the [`Driver`] state
//! struct, the [`Reporter`] callback set, the [`Outcome`] return shape)
//! is stable across these landings.

mod cascade;
mod codegen;
mod command;
mod context;
mod derive_debug;
mod derive_eq;
mod discover;
mod error;
mod mimir;
mod outcome;
mod plan;
mod properties;
mod render;
mod reporter;
mod stdlib_source;
mod structmap;
mod summary;
mod workspace;
mod workspace_exec;

pub use cascade::run as run_cascade;
pub use command::{
    AddCommand, AuditCommand, BuildOptions, Command, ContractDiffCommand, KeyGenerateCommand,
    MimirOptions, PublishCommand, StructureBudgetMode, UpdateCommand, WhyCommand,
};
pub use context::Driver;
pub use error::DriverError;
pub use outcome::{ExitCode, Outcome, Summary};
pub use plan::{RunPlan, Stage};
pub use render::{render_diagnostic, resolve_labels};
pub use reporter::{Reporter, SilentReporter};
pub use summary::render as render_summary;
pub use workspace::{MemberHandle, WorkspaceResolution, resolve_default_run, resolve_unique_runnable};

use std::sync::Arc;

/// Driver entry point.
///
/// Peeks at the top manifest first: when `[workspace]` is declared the
/// cascade runs once per member (each with the other members registered
/// as implicit deps), and the per-member outcomes are aggregated. When
/// no `[workspace]` table is present this is exactly the single-package
/// path: lower `command` + `options` into a [`RunPlan`], construct a
/// [`Driver`] via [`Driver::initialize`], run the cascade, return the
/// [`Outcome`].
///
/// # Errors
///
/// [`DriverError`] is returned when the cascade cannot start at all —
/// missing or unparseable manifest, missing source root, unspecified
/// or invalid target. Build-level errors (parse / resolve / typecheck
/// failures) do **not** flow through this `Err`; they land in
/// [`Outcome::diagnostics`] with [`ExitCode::BuildError`].
pub fn run(
    command: Command,
    options: BuildOptions,
    reporter: Arc<dyn Reporter>,
) -> Result<Outcome, DriverError> {
    let plan = RunPlan::for_command(command, options.full_materialization);
    let resolution = workspace::resolve(&options.manifest_path)?;

    if resolution.members.is_empty() {
        // The active manifest may itself be a member of an enclosing
        // `[workspace]` declared one level up — register the other members
        // as implicit deps so cross-member imports resolve when the user
        // ran `edda check` from inside `lib/<member>/` rather than the
        // workspace root.
        let enclosing_siblings = workspace::resolve_enclosing(&options.manifest_path)?;
        let mut driver = if enclosing_siblings.is_empty() {
            Driver::initialize(options, reporter)?
        } else {
            Driver::initialize_with_siblings(options, reporter, &enclosing_siblings)?
        };
        let outcome = cascade::run(&mut driver, &plan);
        // When this process is a workspace-member child spawned by
        // `run_workspace`'s isolated path, persist the package summary so
        // the parent can build the workspace aggregator `index.toon`
        // without in-process access to this `Driver`. Inert otherwise.
        workspace_exec::write_member_summary_sidecar(&driver);
        // Likewise persist this member's per-directory descendant bodies
        // when the parent requested them (only under a `descendant_tree`
        // workspace). Inert otherwise.
        workspace_exec::write_member_bodies_sidecar(&driver);
        return Ok(outcome);
    }

    // Workspace path. A bare `edda run` over a `[workspace]` has no single
    // target binary to launch. Member-selection gives it
    // defined semantics: the CLI resolves `edda run <member>` to that
    // member's own manifest (a single package) before reaching here, so
    // this branch fires only when no member was designated — reject with
    // guidance naming the members the user can pick.
    if matches!(plan.command, Command::Run) {
        return Ok(workspace_run_unsupported(&options.manifest_path, &resolution));
    }

    run_workspace(resolution, options, reporter, &plan)
}

/// Build-error [`Outcome`] rejecting a bare `edda run` against a
/// workspace root, naming the members selectable with `edda run <member>`.
fn workspace_run_unsupported(
    manifest_path: &std::path::Path,
    resolution: &WorkspaceResolution,
) -> Outcome {
    use edda_diag::{Diagnostic, DiagnosticClass, Severity};
    use edda_span::Span;

    let members: Vec<String> = resolution
        .members
        .iter()
        .filter_map(|m| {
            m.manifest_dir
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .collect();
    let member_hint = if members.is_empty() {
        String::new()
    } else {
        format!(" (members: {})", members.join(", "))
    };

    let mut outcome = Outcome::empty();
    outcome.exit_code = ExitCode::BuildError;
    outcome.diagnostics.push(Diagnostic::new(
        DiagnosticClass::ParseError,
        Severity::Error,
        Span::DUMMY,
        format!(
            "`edda run` needs a target member for the workspace at `{}`: a workspace \
             has no single target binary. Run a member with `edda run <member>`{} \
             or `--manifest-path lib/<member>/package.toml`",
            manifest_path.display(),
            member_hint,
        ),
    ));
    outcome
}

/// Dispatch a Mímir verb (add / update / audit / publish / contract-diff / why).
///
/// Unlike [`run`], these verbs do not construct a [`Driver`] or run the
/// compile cascade. They operate directly on `package.toml` /
/// `package.lock.toml` / registry sources. [`MimirOptions`] is a minimal
/// options struct that carries only the manifest path.
///
/// # Errors
///
/// Returns `Err(DriverError)` only for hard system failures (e.g., manifest
/// unreadable at the system level). Build-level failures (hash mismatches,
/// verification errors, capability escalations) surface as diagnostics with
/// [`ExitCode::BuildError`] inside the returned `Ok(Outcome)`.
pub fn run_mimir(
    command: Command,
    options: MimirOptions,
) -> Result<Outcome, DriverError> {
    use edda_diag::Diagnostics;

    let mut diags = Diagnostics::new();

    let exit_code = match &command {
        Command::Add(cmd) => {
            let out = mimir::add::run_add(cmd, &options, &mut diags);
            out.exit_code
        }
        Command::Update(cmd) => {
            let out = mimir::update::run_update(cmd, &options, &mut diags);
            out.exit_code
        }
        Command::Audit(cmd) => {
            let out = mimir::audit::run_audit(cmd, &options, &mut diags);
            out.exit_code
        }
        Command::Publish(cmd) => {
            let out = mimir::publish::run_publish(cmd, &options, &mut diags);
            out.exit_code
        }
        Command::ContractDiff(cmd) => {
            let out = mimir::contract_diff::run_contract_diff(cmd, &options, &mut diags);
            out.exit_code
        }
        Command::Why(cmd) => {
            let out = mimir::why::run_why(cmd, &options, &mut diags);
            out.exit_code
        }
        Command::KeyGenerate(cmd) => {
            // `key generate` does not consult the manifest; `options` is ignored
            // by the handler. We accept it on the dispatch row to keep the
            // signature uniform with the other Mímir verbs.
            let _ = &options;
            let out = mimir::key::run_generate(cmd, &mut diags);
            out.exit_code
        }
        other => {
            // Caller passed a non-Mímir command — this is a programming error.
            // Surface it as a system-error diagnostic rather than panicking.
            use edda_diag::{Diagnostic, DiagnosticClass, Severity};
            use edda_span::Span;
            diags.push(Diagnostic::new(
                DiagnosticClass::ParseError,
                Severity::Error,
                Span::DUMMY,
                format!(
                    "run_mimir: command `{}` is not a Mímir verb",
                    other.name()
                ),
            ));
            ExitCode::SystemError
        }
    };

    let mut outcome = Outcome::empty();
    outcome.exit_code = exit_code;
    for d in diags.into_vec() {
        outcome.diagnostics.push(d);
    }
    Ok(outcome)
}

/// Dispatch a workspace build either to the per-member subprocess path
/// or the in-process path, then return the aggregate.
fn run_workspace(
    resolution: WorkspaceResolution,
    options: BuildOptions,
    reporter: Arc<dyn Reporter>,
    plan: &RunPlan,
) -> Result<Outcome, DriverError> {
    if workspace_exec::should_isolate(&plan.command) && !workspace_exec::already_isolated() {
        if let Ok(self_exe) = std::env::current_exe() {
            return workspace_exec::run_isolated(self_exe, &resolution, &options);
        }
    }
    run_workspace_in_process(resolution, options, reporter, plan)
}

/// Run the cascade once per workspace member, in-process. Each member
/// sees the other members as implicit deps so cross-member imports
/// resolve through the resolver's third-party-package path.
fn run_workspace_in_process(
    resolution: WorkspaceResolution,
    options: BuildOptions,
    reporter: Arc<dyn Reporter>,
    plan: &RunPlan,
) -> Result<Outcome, DriverError> {
    let mut aggregate = Outcome::empty();
    let mut member_summaries: Vec<edda_structmap::MemberSummary> = Vec::new();
    let mut descendant_bodies: std::collections::BTreeMap<std::path::PathBuf, String> =
        std::collections::BTreeMap::new();
    for (idx, member) in resolution.members.iter().enumerate() {
        let siblings: Vec<MemberHandle> = resolution
            .members
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != idx)
            .map(|(_, m)| m.clone())
            .collect();
        let member_options = BuildOptions {
            manifest_path: member.manifest_path.clone(),
            ..options.clone()
        };
        let mut driver =
            Driver::initialize_with_siblings(member_options, reporter.clone(), &siblings)?;
        let outcome = cascade::run(&mut driver, plan);
        // Capture the member's structure summary (if structmap emission
        // ran) so the workspace aggregator's children[] carries real
        // rolled-up totals + the member's public headline. The same gate
        // also collects this member's per-directory bodies
        // so a `descendant_tree` workspace root can inline them.
        if let Some(summary) = driver.structmap_summary.take() {
            member_summaries.push(edda_structmap::MemberSummary {
                dir: member.manifest_dir.clone(),
                types: summary.types,
                functions: summary.functions,
                public: summary.public,
            });
            descendant_bodies.append(&mut driver.descendant_bodies);
        }
        aggregate = aggregate.merge(outcome);
    }
    // After every member has emitted its own per-package index.toon,
    // write the workspace-level aggregator files at the workspace root +
    // every intermediate directory between the root and a member dir.
    // This keeps the chain-walk from project root down to a leaf
    // contiguous so an LLM agent never hits a missing index.toon
    // mid-traversal.
    //
    // Gated on the same condition that triggers per-member structmap
    // emission: the verb is `Command::Structmap` OR runs the cascade
    // (the manifest-level `build.emit_structmap` defaults to true).
    // `parse-roundtrip` and the Mímir verbs don't reach `run_workspace`.
    let _ = plan;
    let all_member_dirs: Vec<std::path::PathBuf> = resolution
        .members
        .iter()
        .map(|m| m.manifest_dir.clone())
        .collect();
    let stale = write_workspace_aggregator(
        &options.manifest_path,
        &all_member_dirs,
        &member_summaries,
        options.structmap_check,
        resolution.descendant_tree,
        &descendant_bodies,
    );
    // Aggregator files live above the member dirs, so per-member check
    // (which runs inside each member's `drive_structmap`) never sees them —
    // surface their staleness here so `structmap --check` covers the whole
    // chain root→leaf and exits non-zero if any aggregator drifted.
    if options.structmap_check {
        for path in stale {
            structmap::push_structmap_stale(&mut aggregate.diagnostics, &path);
        }
        if aggregate.diagnostics.has_errors() {
            aggregate.exit_code = aggregate.exit_code.worst(ExitCode::BuildError);
        }
    }
    Ok(aggregate)
}

pub(crate) fn write_workspace_aggregator(
    workspace_manifest_path: &std::path::Path,
    all_member_dirs: &[std::path::PathBuf],
    member_summaries: &[edda_structmap::MemberSummary],
    check: bool,
    descendant_tree: bool,
    descendant_bodies: &std::collections::BTreeMap<std::path::PathBuf, String>,
) -> Vec<std::path::PathBuf> {
    let Some(manifest_parent) = workspace_manifest_path.parent() else {
        return Vec::new();
    };
    let workspace_root = context::absolutize_dir(manifest_parent);
    let all_member_dirs: Vec<std::path::PathBuf> = all_member_dirs
        .iter()
        .map(|d| context::absolutize_dir(d))
        .collect();
    let all_member_dirs = &all_member_dirs[..];
    let member_summaries: Vec<edda_structmap::MemberSummary> = member_summaries
        .iter()
        .cloned()
        .map(|mut m| {
            m.dir = context::absolutize_dir(&m.dir);
            m
        })
        .collect();
    let (member_summaries, carried) = complete_member_summaries(all_member_dirs, &member_summaries);
    if !carried.is_empty() {
        eprintln!(
            "warn: workspace aggregator: {} member(s) produced no fresh structure summary this \
             run (failed before typecheck or aborted); using committed rollup rows for: {}",
            carried.len(),
            carried.join(", "),
        );
    }
    // The workspace-root `index.toon`'s `project:` field is the top
    // manifest's `[package].name` when the root is a hybrid package, or the
    // literal `workspace` when the root is a workspace-only virtual manifest
    // (`[workspace]`, no `[package]`). Deriving it from the directory name
    // instead regressed every workspace-only root: the monorepo subtrees all
    // carry `project: workspace`, and the dir-name form silently reverted them
    // to `project: <dirname>` on any `edda structmap` / `edda build`.
    let project_name = workspace_root_project_name(workspace_manifest_path);
    let compiler_version = env!("CARGO_PKG_VERSION");
    let input = edda_structmap::WorkspaceAggregatorInput {
        project_name: &project_name,
        workspace_root: &workspace_root,
        compiler_version,
        members: &member_summaries,
    };
    let files = if descendant_tree {
        edda_structmap::emit_workspace_descendant_tree(&input, descendant_bodies)
    } else {
        edda_structmap::emit_workspace_aggregator(&input)
    };
    if check {
        // Side-effect-free staleness check: collect the aggregator files
        // whose on-disk content differs from the freshly-emitted text.
        return files
            .into_iter()
            .filter(|(path, contents)| structmap::index_toon_is_stale(path, contents))
            .map(|(path, _)| path)
            .collect();
    }
    for (path, contents) in files {
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "warn: failed to create aggregator dir {}: {}",
                    parent.display(),
                    err,
                );
                continue;
            }
        }
        if let Err(err) = structmap::write_index_toon_if_changed(&path, &contents) {
            eprintln!(
                "warn: failed to write workspace aggregator {}: {}",
                path.display(),
                err,
            );
        }
    }
    Vec::new()
}

/// The `project:` label for a workspace-root aggregator `index.toon`.
fn workspace_root_project_name(manifest_path: &std::path::Path) -> String {
    use edda_manifest::LoadedManifest;
    let source_map = edda_span::SourceMap::new();
    let mut diags = edda_diag::Diagnostics::new();
    let lint_cfg = edda_diag::LintConfig::new();
    match edda_manifest::load_any(manifest_path, &source_map, &mut diags, &lint_cfg) {
        Some(LoadedManifest::Package(p)) => p.package.to_string(),
        _ => "workspace".to_string(),
    }
}

/// Complete a partial fresh-summary list against the full workspace
/// member set: a member missing from `fresh` is reconstructed from its
/// committed on-disk aggregator row (zeros + empty headline when no row
/// survives either), never dropped.
fn complete_member_summaries(
    all_member_dirs: &[std::path::PathBuf],
    fresh: &[edda_structmap::MemberSummary],
) -> (Vec<edda_structmap::MemberSummary>, Vec<String>) {
    let have: std::collections::BTreeSet<&std::path::Path> =
        fresh.iter().map(|m| m.dir.as_path()).collect();
    let mut out = fresh.to_vec();
    let mut carried: Vec<String> = Vec::new();
    for dir in all_member_dirs {
        if have.contains(dir.as_path()) {
            continue;
        }
        carried.push(
            dir.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| dir.to_string_lossy().into_owned()),
        );
        out.push(
            read_existing_member_row(dir).unwrap_or_else(|| edda_structmap::MemberSummary {
                dir: dir.clone(),
                types: 0,
                functions: 0,
                public: String::new(),
            }),
        );
    }
    (out, carried)
}

/// Recover a member's rolled-up `(types, functions, public)` from the
/// committed aggregator `index.toon` at the member's parent directory.
fn read_existing_member_row(member_dir: &std::path::Path) -> Option<edda_structmap::MemberSummary> {
    let parent = member_dir.parent()?;
    let name = member_dir.file_name()?.to_string_lossy().into_owned();
    let text = std::fs::read_to_string(parent.join(edda_structmap::INDEX_FILENAME)).ok()?;
    let mut in_children = false;
    for line in text.lines() {
        if line.starts_with("children[") {
            in_children = true;
            continue;
        }
        if !in_children {
            continue;
        }
        let Some(row) = line.strip_prefix("  ") else {
            // First non-row line ends the table; the row wasn't there.
            return None;
        };
        let mut fields = row.splitn(4, ',');
        let path = unquote_toon_field(fields.next()?);
        if path != name {
            continue;
        }
        let types = fields.next()?.trim().parse::<u32>().ok()?;
        let functions = fields.next()?.trim().parse::<u32>().ok()?;
        let public = unquote_toon_field(fields.next().unwrap_or(""));
        return Some(edda_structmap::MemberSummary {
            dir: member_dir.to_path_buf(),
            types,
            functions,
            public,
        });
    }
    None
}

/// Undo the aggregator emitter's TOON field quoting.
fn unquote_toon_field(s: &str) -> String {
    let s = s.trim();
    let Some(inner) = s.strip_prefix('"').and_then(|r| r.strip_suffix('"')) else {
        return s.to_string();
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => break,
        }
    }
    out
}

#[cfg(test)]
mod aggregator_completion_tests {
    use super::*;
    use std::path::PathBuf;

    fn summary(
        dir: PathBuf,
        types: u32,
        functions: u32,
        public: &str,
    ) -> edda_structmap::MemberSummary {
        edda_structmap::MemberSummary {
            dir,
            types,
            functions,
            public: public.to_string(),
        }
    }

    #[test]
    fn complete_passes_through_when_all_fresh() {
        let dirs = vec![PathBuf::from("/ws/lib/a"), PathBuf::from("/ws/lib/b")];
        let fresh = vec![
            summary(dirs[0].clone(), 1, 2, "A"),
            summary(dirs[1].clone(), 3, 4, "B"),
        ];
        let (complete, carried) = complete_member_summaries(&dirs, &fresh);
        assert!(carried.is_empty());
        assert_eq!(complete.len(), 2);
    }

    #[test]
    fn missing_member_carried_from_committed_row() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let lib = tmp.path().join("lib");
        std::fs::create_dir_all(lib.join("a")).unwrap();
        std::fs::create_dir_all(lib.join("b")).unwrap();
        std::fs::write(
            lib.join("index.toon"),
            "loc: lib\n\nchildren[2]{path,types,functions,public}:\n  a,1,2,A\n  b,9,17,Store hash_bytes\n\n",
        )
        .unwrap();
        let dirs = vec![lib.join("a"), lib.join("b")];
        let fresh = vec![summary(dirs[0].clone(), 5, 6, "A2")];
        let (complete, carried) = complete_member_summaries(&dirs, &fresh);
        assert_eq!(carried, vec!["b".to_string()]);
        assert_eq!(complete.len(), 2);
        let b = complete.iter().find(|m| m.dir == dirs[1]).expect("b row");
        assert_eq!((b.types, b.functions), (9, 17));
        assert_eq!(b.public, "Store hash_bytes");
        // The fresh member's numbers are used verbatim, not the stale row.
        let a = complete.iter().find(|m| m.dir == dirs[0]).expect("a row");
        assert_eq!((a.types, a.functions), (5, 6));
    }

    #[test]
    fn missing_member_with_no_committed_row_gets_zero_row() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let lib = tmp.path().join("lib");
        std::fs::create_dir_all(lib.join("a")).unwrap();
        std::fs::create_dir_all(lib.join("ghost")).unwrap();
        std::fs::write(
            lib.join("index.toon"),
            "loc: lib\n\nchildren[1]{path,types,functions,public}:\n  a,1,2,A\n\n",
        )
        .unwrap();
        let dirs = vec![lib.join("a"), lib.join("ghost")];
        let fresh = vec![summary(dirs[0].clone(), 1, 2, "A")];
        let (complete, carried) = complete_member_summaries(&dirs, &fresh);
        assert_eq!(carried, vec!["ghost".to_string()]);
        let ghost = complete
            .iter()
            .find(|m| m.dir == dirs[1])
            .expect("ghost row");
        assert_eq!((ghost.types, ghost.functions), (0, 0));
        assert_eq!(ghost.public, "");
    }

    #[test]
    fn quoted_public_headline_unquotes() {
        assert_eq!(unquote_toon_field("plain"), "plain");
        assert_eq!(unquote_toon_field("\"has, comma\""), "has, comma");
        assert_eq!(unquote_toon_field("\"esc \\\" quote\""), "esc \" quote");
        assert_eq!(unquote_toon_field("\"\""), "");
    }
}
