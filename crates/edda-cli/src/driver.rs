//! Real dispatcher that drives `edda-driver` for the build-pipeline
//! verbs.
//!
//! [`DriverDispatcher`] implements [`crate::dispatch::Dispatcher`] by
//! translating per-verb argv (CommonArgs + verb-specific flags) into an
//! [`edda_driver::BuildOptions`], invoking [`edda_driver::run`], and
//! lifting the resulting [`edda_driver::Outcome`] back into the CLI's
//! diagnostic take + exit-code surface.
//!
//! Verbs whose backing implementation lives outside `edda-driver`
//! (`gc`, `promote`, `demote`, `regenerate`, `daemon`) emit a
//! `parse_error` "not yet implemented" diagnostic and return
//! [`crate::exit::SYSTEM_ERROR`], the same shape
//! [`crate::dispatch::StubDispatcher`] used.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, Severity};
use edda_driver::{
    BuildOptions, Command, DriverError, ExitCode, MimirOptions, Reporter, Summary, render_summary,
    run, run_mimir,
};
use edda_span::Span;

use crate::cli::{
    AddArgs, AuditArgs, BenchArgs, BuildArgs, CheckArgs, CleanArgs, CommonArgs, ContractDiffArgs,
    DaemonArgs, DemoteArgs, FmtArgs, GcArgs, HotArgs, KeyArgs, KeySubcommand, LintArgs,
    PromoteArgs, PublishArgs, RegenerateArgs, RunArgs, StructmapArgs, TestArgs, UpdateArgs,
    WhyArgs,
};
use crate::dispatch::Dispatcher;
use crate::exit::{BUILD_ERROR, SUCCESS, SYSTEM_ERROR};

/// Drives `edda-driver` for the build-pipeline verbs and refuses the
/// cache / daemon verbs (which live in `edda-cache` / `edda-daemon` and
/// have no wired implementation in this surface yet).
#[derive(Debug, Default, Clone, Copy)]
pub struct DriverDispatcher;

impl DriverDispatcher {
    /// Construct a new driver-backed dispatcher.
    pub fn new() -> Self {
        Self
    }
}

impl Dispatcher for DriverDispatcher {
    fn build(&mut self, args: &BuildArgs, diags: &mut Diagnostics) -> u8 {
        let options = build_options(&args.common, args.full_materialization);
        invoke(Command::Build, options, diags)
    }
    fn check(&mut self, args: &CheckArgs, diags: &mut Diagnostics) -> u8 {
        let options = build_options(&args.common, args.full_materialization);
        invoke(Command::Check, options, diags)
    }
    fn run(&mut self, args: &RunArgs, diags: &mut Diagnostics) -> u8 {
        let Some(manifest) = resolve_run_manifest(args, diags) else {
            return BUILD_ERROR;
        };
        let mut options = build_options(&args.common, false);
        options.manifest_path = manifest.clone();
        let build_code = invoke(Command::Run, options, diags);
        if build_code != SUCCESS {
            return build_code;
        }
        run_built_target(&manifest, diags)
    }
    fn hot(&mut self, args: &HotArgs, diags: &mut Diagnostics) -> u8 {
        crate::hot::run_hot(args, diags)
    }
    fn test(&mut self, args: &TestArgs, diags: &mut Diagnostics) -> u8 {
        invoke(
            Command::Test,
            build_options_test(&args.common, args.properties),
            diags,
        )
    }
    fn bench(&mut self, args: &BenchArgs, diags: &mut Diagnostics) -> u8 {
        invoke(Command::Bench, build_options(&args.common, false), diags)
    }
    fn gc(&mut self, _args: &GcArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "gc", "edda-cache");
        SYSTEM_ERROR
    }
    fn promote(&mut self, _args: &PromoteArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "promote", "edda-cache");
        SYSTEM_ERROR
    }
    fn demote(&mut self, _args: &DemoteArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "demote", "edda-cache");
        SYSTEM_ERROR
    }
    fn regenerate(&mut self, _args: &RegenerateArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "regenerate", "edda-codegen");
        SYSTEM_ERROR
    }
    fn clean(&mut self, args: &CleanArgs, diags: &mut Diagnostics) -> u8 {
        invoke(Command::Clean, build_options(&args.common, false), diags)
    }
    fn fmt(&mut self, args: &FmtArgs, diags: &mut Diagnostics) -> u8 {
        if args.strip_comments {
            return run_strip_comments(&args.common, diags);
        }
        invoke(Command::Fmt, build_options(&args.common, false), diags)
    }
    fn lint(&mut self, args: &LintArgs, diags: &mut Diagnostics) -> u8 {
        let mut options = build_options(&args.common, false);
        options.lint_trust_points = args.trust_points;
        options.lint_capability_safe_stdlib = args.capability_safe_stdlib;
        invoke(Command::Lint, options, diags)
    }
    fn daemon(&mut self, _args: &DaemonArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "daemon", "edda-daemon");
        SYSTEM_ERROR
    }
    fn structmap(&mut self, args: &StructmapArgs, diags: &mut Diagnostics) -> u8 {
        run_structmap(args, diags)
    }
    fn add(&mut self, args: &AddArgs, diags: &mut Diagnostics) -> u8 {
        let options = mimir_options(&args.common);
        invoke_mimir(edda_driver::Command::Add(edda_driver::AddCommand {
            name_at_version: args.name.clone(),
            accept_unstable: args.accept_unstable,
            max_effects: args.max_effects.clone(),
        }), options, diags)
    }
    fn update(&mut self, args: &UpdateArgs, diags: &mut Diagnostics) -> u8 {
        let options = mimir_options(&args.common);
        invoke_mimir(edda_driver::Command::Update(edda_driver::UpdateCommand {
            name: args.name.clone(),
            accept_surface_change: args.accept_surface_change.clone(),
            accept_publisher_rotation: args.accept_publisher_rotation.clone(),
        }), options, diags)
    }
    fn audit(&mut self, args: &AuditArgs, diags: &mut Diagnostics) -> u8 {
        let options = mimir_options(&args.common);
        invoke_mimir(edda_driver::Command::Audit(edda_driver::AuditCommand {
            quiet: args.quiet,
        }), options, diags)
    }
    fn publish(&mut self, args: &PublishArgs, diags: &mut Diagnostics) -> u8 {
        let options = mimir_options(&args.common);
        invoke_mimir(edda_driver::Command::Publish(edda_driver::PublishCommand {}), options, diags)
    }
    fn contract_diff(&mut self, args: &ContractDiffArgs, diags: &mut Diagnostics) -> u8 {
        let options = mimir_options(&args.common);
        invoke_mimir(edda_driver::Command::ContractDiff(edda_driver::ContractDiffCommand {
            a: args.a.clone(),
            b: args.b.clone(),
        }), options, diags)
    }
    fn why(&mut self, args: &WhyArgs, diags: &mut Diagnostics) -> u8 {
        let options = mimir_options(&args.common);
        invoke_mimir(edda_driver::Command::Why(edda_driver::WhyCommand {
            name: args.name.clone(),
        }), options, diags)
    }
    fn key_generate(&mut self, args: &KeyArgs, diags: &mut Diagnostics) -> u8 {
        let options = mimir_options(&args.common);
        let KeySubcommand::Generate(gen_args) = &args.subcommand;
        invoke_mimir(
            edda_driver::Command::KeyGenerate(edda_driver::KeyGenerateCommand {
                keystore: gen_args.keystore.clone(),
                label: gen_args.label.clone(),
            }),
            options,
            diags,
        )
    }
}

/// Implement `edda structmap [<path>]`: an explicit `--manifest-path`
/// targets exactly that package; otherwise, if `<path>` (or CWD) contains
/// a `package.toml`, run a single invocation; otherwise scan the path for
/// per-package manifests and run one invocation per match.
fn run_structmap(args: &StructmapArgs, diags: &mut Diagnostics) -> u8 {
    if args.common.manifest_path.is_some() {
        let mut options = build_options(&args.common, false);
        options.structmap_check = args.check;
        return invoke(Command::Structmap, options, diags);
    }
    let root = match args.path.as_ref() {
        Some(p) => p.clone(),
        None => PathBuf::from("."),
    };
    let manifests = match collect_manifests(&root) {
        Ok(v) => v,
        Err(message) => {
            diags.push(Diagnostic::new(
                DiagnosticClass::ParseError,
                Severity::Error,
                Span::DUMMY,
                message,
            ));
            return SYSTEM_ERROR;
        }
    };
    if manifests.is_empty() {
        diags.push(Diagnostic::new(
            DiagnosticClass::ParseError,
            Severity::Error,
            Span::DUMMY,
            format!(
                "edda structmap: no `package.toml` found under `{}`",
                root.display()
            ),
        ));
        return SYSTEM_ERROR;
    }
    let mut worst: u8 = SUCCESS;
    for manifest in manifests {
        let mut options = build_options(&args.common, false);
        options.manifest_path = manifest;
        options.structmap_check = args.check;
        let code = invoke(Command::Structmap, options, diags);
        worst = worst_code(worst, code);
    }
    worst
}

fn collect_manifests(root: &std::path::Path) -> Result<Vec<PathBuf>, String> {
    if !root.exists() {
        return Err(format!("edda structmap: path `{}` not found", root.display()));
    }
    let direct = root.join("package.toml");
    if direct.exists() {
        return Ok(vec![direct]);
    }
    if !root.is_dir() {
        return Err(format!(
            "edda structmap: `{}` is not a directory and is not a package manifest",
            root.display()
        ));
    }
    let mut out = Vec::new();
    let entries = std::fs::read_dir(root).map_err(|e| {
        format!(
            "edda structmap: failed to read `{}`: {}",
            root.display(),
            e
        )
    })?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            let m = p.join("package.toml");
            if m.exists() {
                out.push(m);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn worst_code(a: u8, b: u8) -> u8 {
    // Priority: SYSTEM_ERROR > BUILD_ERROR > SUCCESS.
    if a == SYSTEM_ERROR || b == SYSTEM_ERROR {
        SYSTEM_ERROR
    } else if a == BUILD_ERROR || b == BUILD_ERROR {
        BUILD_ERROR
    } else {
        SUCCESS
    }
}

/// Lower [`CommonArgs`] into [`MimirOptions`] for Mímir verbs.
fn mimir_options(common: &CommonArgs) -> MimirOptions {
    MimirOptions {
        manifest_path: common
            .manifest_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("package.toml")),
    }
}

/// Invoke [`edda_driver::run_mimir`] and project the result into the CLI's surface.
fn invoke_mimir(command: Command, options: MimirOptions, diags: &mut Diagnostics) -> u8 {
    match run_mimir(command, options) {
        Ok(outcome) => {
            for d in outcome.diagnostics.into_vec() {
                diags.push(d);
            }
            exit_code_of(outcome.exit_code)
        }
        Err(err) => {
            emit_driver_error(diags, &err);
            SYSTEM_ERROR
        }
    }
}

/// Lower the CLI's [`CommonArgs`] (plus the verb's `full_materialization` toggle) into [`BuildOptions`].
pub(crate) fn build_options(common: &CommonArgs, full_materialization: bool) -> BuildOptions {
    BuildOptions {
        manifest_path: common
            .manifest_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("package.toml")),
        target_override: common.target.clone(),
        feature_override: common.features.clone(),
        profile_override: common.profile.clone(),
        full_materialization,
        jobs: common.jobs.map(|j| j as usize),
        warn_as_error: common.warn_as_error.clone(),
        properties: false,
        structure_budget: common.structure_budget,
        freestanding: common.freestanding,
        structmap_check: false,
        lint_trust_points: false,
        lint_capability_safe_stdlib: false,
    }
}

/// Run the no-comment migration: walk every `.ea` file under
/// the package directory (the manifest's parent, or `.`), strip all
/// comments, and rewrite each file whose content changed. Returns
/// [`SYSTEM_ERROR`] if any file could not be read or written, else
/// [`SUCCESS`].
fn run_strip_comments(common: &CommonArgs, diags: &mut Diagnostics) -> u8 {
    let manifest = common
        .manifest_path
        .clone()
        .unwrap_or_else(|| PathBuf::from("package.toml"));
    let root = manifest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let mut files = Vec::new();
    collect_ea_files(&root, 0, &mut files);

    let interner = edda_intern::Interner::new();
    let lint_cfg = edda_diag::LintConfig::default();
    let mut rewritten = 0usize;
    let mut status = SUCCESS;
    for path in &files {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                diags.push(Diagnostic::new(
                    DiagnosticClass::ParseError,
                    Severity::Error,
                    Span::DUMMY,
                    format!("fmt --strip-comments: cannot read `{}`: {e}", path.display()),
                ));
                status = SYSTEM_ERROR;
                continue;
            }
        };
        let sm = edda_span::SourceMap::new();
        let fid = sm.add_file(path.clone(), content.clone());
        let stripped =
            edda_syntax::strip_comments(sm.file_content(fid), fid, &interner, diags, &lint_cfg);
        if stripped != content {
            if let Err(e) = std::fs::write(path, &stripped) {
                diags.push(Diagnostic::new(
                    DiagnosticClass::ParseError,
                    Severity::Error,
                    Span::DUMMY,
                    format!("fmt --strip-comments: cannot write `{}`: {e}", path.display()),
                ));
                status = SYSTEM_ERROR;
            } else {
                rewritten += 1;
            }
        }
    }
    println!(
        "edda fmt --strip-comments: rewrote {rewritten} of {} `.ea` file(s) under {}",
        files.len(),
        root.display()
    );
    status
}

/// Recursively collect `*.ea` files under `dir`.
fn collect_ea_files(dir: &std::path::Path, depth: u32, out: &mut Vec<PathBuf>) {
    const MAX_DEPTH: u32 = 64;
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let skip = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with('.') || n == "target")
                .unwrap_or(true);
            if !skip {
                collect_ea_files(&path, depth + 1, out);
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("ea") {
            out.push(path);
        }
    }
}

/// Lower the CLI's [`CommonArgs`] for `edda test`, with the
/// `--properties` flag overlaid. Mirrors [`build_options`] otherwise.
fn build_options_test(common: &CommonArgs, properties: bool) -> BuildOptions {
    let mut opts = build_options(common, false);
    opts.properties = properties;
    opts
}

/// Resolve the manifest `edda run` builds and launches against: an
/// explicit `--manifest-path`, else the named workspace member under
/// `lib/<member>/package.toml`, else the `[workspace] default_run` member,
/// else the unique workspace member requesting `[package] kind =
/// "executable"`, when a bare `edda run` lands on a workspace root, else
/// `./package.toml`.
fn resolve_run_manifest(args: &RunArgs, diags: &mut Diagnostics) -> Option<PathBuf> {
    if let Some(p) = args.common.manifest_path.as_ref() {
        return Some(p.clone());
    }
    if let Some(member) = args.member.as_ref() {
        return Some(Path::new("lib").join(member).join("package.toml"));
    }
    let root = PathBuf::from("package.toml");
    match edda_driver::resolve_default_run(&root) {
        Ok(Some(member_manifest)) => Some(member_manifest),
        Ok(None) => match edda_driver::resolve_unique_runnable(&root) {
            Ok(Some(member_manifest)) => Some(member_manifest),
            Ok(None) => Some(root),
            Err(message) => {
                diags.push(Diagnostic::new(
                    DiagnosticClass::ParseError,
                    Severity::Error,
                    Span::DUMMY,
                    message,
                ));
                None
            }
        },
        Err(message) => {
            diags.push(Diagnostic::new(
                DiagnosticClass::ParseError,
                Severity::Error,
                Span::DUMMY,
                message,
            ));
            None
        }
    }
}

/// Locate the freshly built binary under `manifest`'s package root and
/// run it with inherited stdio, returning the child's exit code.
///
/// Backs the exec half of `edda run`: [`DriverDispatcher::run`] resolves
/// the target manifest (honouring the `<member>` positional), builds
/// through [`invoke`] first, and only reaches here on a `SUCCESS` build.
/// A missing executable (a library-only package) or a spawn failure is
/// reported as [`SYSTEM_ERROR`]; a child terminated by a Unix signal
/// maps to the conventional `128 + signo`.
fn run_built_target(manifest: &Path, diags: &mut Diagnostics) -> u8 {
    let package_root = manifest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let Some(exe) = crate::hot::find_built_exe(&package_root) else {
        diags.push(Diagnostic::new(
            DiagnosticClass::ParseError,
            Severity::Error,
            Span::DUMMY,
            format!(
                "edda run: no executable found under `{}/.edda/build/<triple>/<profile>/bin/` \
                 after a successful build — `run` requires a binary target",
                package_root.display()
            ),
        ));
        return SYSTEM_ERROR;
    };
    match std::process::Command::new(&exe).status() {
        Ok(status) => exit_code_from_status(&status),
        Err(e) => {
            diags.push(Diagnostic::new(
                DiagnosticClass::ParseError,
                Severity::Error,
                Span::DUMMY,
                format!("edda run: failed to launch `{}`: {e}", exe.display()),
            ));
            SYSTEM_ERROR
        }
    }
}

/// Project a child [`std::process::ExitStatus`] into the CLI's `u8`
/// exit-code surface.
fn exit_code_from_status(status: &std::process::ExitStatus) -> u8 {
    if let Some(code) = status.code() {
        return code as u8;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signo) = status.signal() {
            return 128u8.wrapping_add(signo as u8);
        }
    }
    SYSTEM_ERROR
}

/// Invoke [`edda_driver::run`] and project the result into the CLI's
/// diagnostic take + exit-code surface.
pub(crate) fn invoke(command: Command, options: BuildOptions, diags: &mut Diagnostics) -> u8 {
    let reporter: Arc<dyn Reporter> = Arc::new(SummaryReporter { verb: command.name() });
    match run(command, options, reporter) {
        Ok(outcome) => {
            for d in outcome.diagnostics.into_vec() {
                diags.push(d);
            }
            exit_code_of(outcome.exit_code)
        }
        Err(err) => {
            emit_driver_error(diags, &err);
            SYSTEM_ERROR
        }
    }
}

fn exit_code_of(code: ExitCode) -> u8 {
    match code {
        ExitCode::Success => SUCCESS,
        ExitCode::BuildError => BUILD_ERROR,
        ExitCode::SystemError => SYSTEM_ERROR,
    }
}

/// Stdout-printing reporter that emits the §10 summary line at cascade
/// completion. Diagnostics still flow through the [`Outcome`]; this
/// reporter only owns the summary print.
struct SummaryReporter {
    verb: &'static str,
}

impl Reporter for SummaryReporter {
    fn package_started(&self, package: &str) {
        println!("{}: starting {}", self.verb, package);
    }

    fn summary(&self, s: &Summary) {
        println!("{}", render_summary(self.verb, s));
    }
}

/// Emit the canonical "not yet implemented" diagnostic for verbs whose
/// backing crate has no wired entry point in this dispatcher.
fn emit_pending(diags: &mut Diagnostics, verb: &str, backing_crate: &str) {
    diags.push(Diagnostic::new(
        DiagnosticClass::ParseError,
        Severity::Error,
        Span::DUMMY,
        format!(
            "edda {}: not yet implemented (pending {} wave)",
            verb, backing_crate
        ),
    ));
}

/// Lift a [`DriverError`] into a `parse_error` diagnostic.
///
/// [`DriverError::ManifestLoad`] is special-cased: `edda-manifest::load`
/// already pushed the underlying parse diagnostic into the driver's
/// take before the error variant surfaced, so this path adds a single
/// summary line attributed to the manifest path. The other variants
/// are pure driver-side failures with no pre-pushed context, so they
/// emit a single diagnostic carrying the error's `Display`.
fn emit_driver_error(diags: &mut Diagnostics, err: &DriverError) {
    let message = err.to_string();
    diags.push(Diagnostic::new(
        DiagnosticClass::ParseError,
        Severity::Error,
        Span::DUMMY,
        message,
    ));
}
