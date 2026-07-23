//! `edda-cli` — the `edda` binary's library surface.
//!
//! Subcommands per `docs/tooling/build-system.md` §10 + the Mímir verbs
//! (`08-packages.md` §8):
//!   build, check, run, hot, test, bench, gc, promote, demote, regenerate,
//!   clean, fmt, lint, daemon, structmap, add, update, audit, publish,
//!   contract-diff, why, key.
//!
//! Each CLI verb has a 1:1 MCP-operation counterpart; the daemon serves
//! the wire form and this binary serves the terminal form.
//!
//! # Wave status
//!
//! - **Argv parsing (`§10`)**: complete for the v0.1 surface — every
//!   locked verb, every common flag (`--target`, `--features`,
//!   `--profile`, `--manifest-path`, `--warn-as-error`, `-q`/`-v`,
//!   `-j`/`--jobs`), `build`/`check`'s `--full-materialization` (§5),
//!   and `gc`'s tier selectors (`--dry-run`, `--cache-tier`,
//!   `--repo-tier`, `--global`).
//! - **Dispatch seam**: [`Dispatcher`] is the trait downstream waves
//!   plug into. [`StubDispatcher`] is retained as test scaffolding.
//!   [`DriverDispatcher`] is the active implementation — `main` uses
//!   it unconditionally.
//! - **Diagnostic rendering**: the binary's `main` renders each
//!   diagnostic through `edda_driver::render_diagnostic` (the multi-line
//!   §6 surface format), one block per diagnostic to stderr.

pub mod cli;
pub mod common_flags;
pub mod dispatch;
pub mod driver;
pub mod exit;
pub mod flags;
pub mod hot;
pub mod parse;
pub mod version;

pub use cli::{
    AddArgs, AuditArgs, BenchArgs, BuildArgs, CheckArgs, CleanArgs, Cli, CommonArgs,
    ContractDiffArgs, DaemonArgs, DemoteArgs, FmtArgs, GcArgs, HotArgs, KeyArgs, KeyGenerateArgs,
    KeySubcommand, LintArgs, PromoteArgs, PublishArgs, RegenerateArgs, RunArgs, StructmapArgs,
    TestArgs, UpdateArgs, Verb, Verbosity, WhyArgs,
};
pub use dispatch::{Dispatcher, StubDispatcher};
pub use driver::DriverDispatcher;
pub use exit::{BUILD_ERROR, SUCCESS, SYSTEM_ERROR, code as exit_code};

use std::process::ExitCode;

use edda_diag::Diagnostics;

/// Parse `argv` and execute the resulting verb via `dispatcher`.
///
/// `argv[0]` is the program name (typically `"edda"`) and is ignored.
/// Diagnostics produced during parsing and dispatch are pushed into
/// `diags`; the caller is responsible for rendering them.
///
/// Exit code mapping per `build-system.md` §10:
/// - Argv parse failure (unknown verb, missing required positional, or any
///   soft parse error such as an unknown flag) → [`SYSTEM_ERROR`]; the verb
///   is not dispatched.
/// - Successful parse → whatever code the dispatcher returns.
pub fn run<D: Dispatcher>(
    argv: &[String],
    dispatcher: &mut D,
    diags: &mut Diagnostics,
) -> ExitCode {
    if let Some(code) = version::intercept(argv) {
        return exit_code(code);
    }
    let Some(cli) = parse::parse_args(argv, diags) else {
        return exit_code(SYSTEM_ERROR);
    };
    // A soft parse error — e.g. an unknown flag on an otherwise well-formed
    // verb — leaves `parse_args` returning `Some` with the error already in
    // `diags` (the parse keeps going so multiple errors surface at once). A
    // malformed invocation must not dispatch the verb: several dispatchers
    // (notably `structmap`) run a full generation pass, so dispatching here
    // would do minutes of work and dirty the tree before the parse error ever
    // surfaced. Fail fast; `main` still renders the parse diagnostics.
    if diags.has_errors() {
        return exit_code(SYSTEM_ERROR);
    }
    let code = match &cli.verb {
        Verb::Build(a) => dispatcher.build(a, diags),
        Verb::Check(a) => dispatcher.check(a, diags),
        Verb::Run(a) => dispatcher.run(a, diags),
        Verb::Hot(a) => dispatcher.hot(a, diags),
        Verb::Test(a) => dispatcher.test(a, diags),
        Verb::Bench(a) => dispatcher.bench(a, diags),
        Verb::Gc(a) => dispatcher.gc(a, diags),
        Verb::Promote(a) => dispatcher.promote(a, diags),
        Verb::Demote(a) => dispatcher.demote(a, diags),
        Verb::Regenerate(a) => dispatcher.regenerate(a, diags),
        Verb::Clean(a) => dispatcher.clean(a, diags),
        Verb::Fmt(a) => dispatcher.fmt(a, diags),
        Verb::Lint(a) => dispatcher.lint(a, diags),
        Verb::Daemon(a) => dispatcher.daemon(a, diags),
        Verb::Structmap(a) => dispatcher.structmap(a, diags),
        Verb::Add(a) => dispatcher.add(a, diags),
        Verb::Update(a) => dispatcher.update(a, diags),
        Verb::Audit(a) => dispatcher.audit(a, diags),
        Verb::Publish(a) => dispatcher.publish(a, diags),
        Verb::ContractDiff(a) => dispatcher.contract_diff(a, diags),
        Verb::Why(a) => dispatcher.why(a, diags),
        Verb::Key(a) => match a.subcommand {
            cli::KeySubcommand::Generate(_) => dispatcher.key_generate(a, diags),
        },
    };
    exit_code(code)
}
