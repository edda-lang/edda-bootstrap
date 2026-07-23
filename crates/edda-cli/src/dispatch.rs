//! Dispatcher seam: the [`Dispatcher`] trait the binary calls into for
//! every locked §10 verb, plus [`StubDispatcher`] — an early
//! placeholder that returns [`crate::exit::SYSTEM_ERROR`] for every
//! call and emits a `parse_error` diagnostic explaining the verb is
//! not yet wired.
//!
//! [`crate::driver::DriverDispatcher`] (calls `edda-driver`) is the active
//! implementation `main` uses unconditionally; [`StubDispatcher`] is now
//! test-only scaffolding. The `daemon` verb is routed through the driver
//! dispatcher (long-lived-server dispatch is still owed).

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, Severity};
use edda_span::Span;

use crate::cli::{
    AddArgs, AuditArgs, BenchArgs, BuildArgs, CheckArgs, CleanArgs, ContractDiffArgs, DaemonArgs,
    DemoteArgs, FmtArgs, GcArgs, HotArgs, KeyArgs, LintArgs, PromoteArgs, PublishArgs,
    RegenerateArgs, RunArgs, StructmapArgs, TestArgs, UpdateArgs, WhyArgs,
};
use crate::exit::SYSTEM_ERROR;

/// Execute a parsed CLI verb. Implementors return the process exit
/// code per `build-system.md` §10. Diagnostics produced during
/// execution are pushed into the supplied [`Diagnostics`] take.
///
/// Method-per-verb (rather than `fn run(&self, verb: Verb)`) keeps the
/// trait honest: every locked §10 verb appears here and removing one
/// is a compile error. New verbs added to §10 add a trait method.
pub trait Dispatcher {
    /// `edda build` — full cascade for the current target/profile.
    fn build(&mut self, args: &BuildArgs, diags: &mut Diagnostics) -> u8;
    /// `edda check` — stop after typecheck.
    fn check(&mut self, args: &CheckArgs, diags: &mut Diagnostics) -> u8;
    /// `edda run` — compile + execute the resulting binary.
    fn run(&mut self, args: &RunArgs, diags: &mut Diagnostics) -> u8;
    /// `edda hot [member] [-- <args>]` — build, supervise, and
    /// hot-restart the target on source change.
    fn hot(&mut self, args: &HotArgs, diags: &mut Diagnostics) -> u8;
    /// `edda test` — compile + execute `tests/` entry points.
    fn test(&mut self, args: &TestArgs, diags: &mut Diagnostics) -> u8;
    /// `edda bench` — compile + execute `bench/` entry points.
    fn bench(&mut self, args: &BenchArgs, diags: &mut Diagnostics) -> u8;
    /// `edda gc` — garbage-collect codegen artifacts.
    fn gc(&mut self, args: &GcArgs, diags: &mut Diagnostics) -> u8;
    /// `edda promote <artifact>` — cache tier → repo tier.
    fn promote(&mut self, args: &PromoteArgs, diags: &mut Diagnostics) -> u8;
    /// `edda demote <artifact>` — repo tier → cache tier.
    fn demote(&mut self, args: &DemoteArgs, diags: &mut Diagnostics) -> u8;
    /// `edda regenerate <spec>` — force-regenerate a specialisation.
    fn regenerate(&mut self, args: &RegenerateArgs, diags: &mut Diagnostics) -> u8;
    /// `edda clean` — remove `.edda/build/`.
    fn clean(&mut self, args: &CleanArgs, diags: &mut Diagnostics) -> u8;
    /// `edda fmt` — run the formatter.
    fn fmt(&mut self, args: &FmtArgs, diags: &mut Diagnostics) -> u8;
    /// `edda lint` — run lints without producing output.
    fn lint(&mut self, args: &LintArgs, diags: &mut Diagnostics) -> u8;
    /// `edda daemon` — start the long-lived MCP server.
    fn daemon(&mut self, args: &DaemonArgs, diags: &mut Diagnostics) -> u8;
    /// `edda structmap [<path>]` — emit per-directory `index.toon` files.
    fn structmap(&mut self, args: &StructmapArgs, diags: &mut Diagnostics) -> u8;
    /// `edda add <name>[@<ver>]` — resolve, fetch, verify and pin a dep (§8.1).
    fn add(&mut self, args: &AddArgs, diags: &mut Diagnostics) -> u8;
    /// `edda update [<name>]` — bump deps within caret/tilde range (§8.2).
    fn update(&mut self, args: &UpdateArgs, diags: &mut Diagnostics) -> u8;
    /// `edda audit` — re-verify all lockfile entries against cached bytes (§8.3).
    fn audit(&mut self, args: &AuditArgs, diags: &mut Diagnostics) -> u8;
    /// `edda publish` — build, sign, and upload the current rune (§8.4).
    fn publish(&mut self, args: &PublishArgs, diags: &mut Diagnostics) -> u8;
    /// `edda contract-diff <a> <b>` — surface/effect diff between two rune versions (§8.5).
    fn contract_diff(&mut self, args: &ContractDiffArgs, diags: &mut Diagnostics) -> u8;
    /// `edda why <name>` — transitive provenance trace for a dependency (§8.6).
    fn why(&mut self, args: &WhyArgs, diags: &mut Diagnostics) -> u8;
    /// `edda key generate` — mint a fresh ed25519 publisher keypair and
    /// persist it to the keystore. The router sits on `Verb::Key`'s
    /// nested subcommand enum; this trait surfaces the only landed
    /// action directly so the dispatch table stays one-method-per-leaf.
    fn key_generate(&mut self, args: &KeyArgs, diags: &mut Diagnostics) -> u8;
}

/// Placeholder dispatcher. Every verb emits a `parse_error`
/// diagnostic at `Span::DUMMY` saying the implementation is pending
/// the relevant downstream crate, and returns
/// [`crate::exit::SYSTEM_ERROR`].
///
/// This type disappears once the real dispatchers land — its
/// surface is early scaffolding, not a long-term API.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubDispatcher;

impl StubDispatcher {
    /// Construct a new stub dispatcher.
    pub fn new() -> Self {
        Self
    }
}

impl Dispatcher for StubDispatcher {
    fn build(&mut self, _args: &BuildArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "build", "edda-driver");
        SYSTEM_ERROR
    }
    fn check(&mut self, _args: &CheckArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "check", "edda-driver");
        SYSTEM_ERROR
    }
    fn run(&mut self, _args: &RunArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "run", "edda-driver");
        SYSTEM_ERROR
    }
    fn hot(&mut self, _args: &HotArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "hot", "edda-driver");
        SYSTEM_ERROR
    }
    fn test(&mut self, _args: &TestArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "test", "edda-driver");
        SYSTEM_ERROR
    }
    fn bench(&mut self, _args: &BenchArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "bench", "edda-driver");
        SYSTEM_ERROR
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
    fn clean(&mut self, _args: &CleanArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "clean", "edda-driver");
        SYSTEM_ERROR
    }
    fn fmt(&mut self, _args: &FmtArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "fmt", "edda-syntax (pretty-printer)");
        SYSTEM_ERROR
    }
    fn lint(&mut self, _args: &LintArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "lint", "edda-driver");
        SYSTEM_ERROR
    }
    fn daemon(&mut self, _args: &DaemonArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "daemon", "edda-daemon");
        SYSTEM_ERROR
    }
    fn structmap(&mut self, _args: &StructmapArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "structmap", "edda-structmap");
        SYSTEM_ERROR
    }
    fn add(&mut self, _args: &AddArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "add", "edda-mimir-registry");
        SYSTEM_ERROR
    }
    fn update(&mut self, _args: &UpdateArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "update", "edda-mimir-registry");
        SYSTEM_ERROR
    }
    fn audit(&mut self, _args: &AuditArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "audit", "edda-manifest");
        SYSTEM_ERROR
    }
    fn publish(&mut self, _args: &PublishArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "publish", "edda-mimir-registry");
        SYSTEM_ERROR
    }
    fn contract_diff(&mut self, _args: &ContractDiffArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "contract-diff", "edda-mimir-hash");
        SYSTEM_ERROR
    }
    fn why(&mut self, _args: &WhyArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "why", "edda-manifest");
        SYSTEM_ERROR
    }
    fn key_generate(&mut self, _args: &KeyArgs, diags: &mut Diagnostics) -> u8 {
        emit_pending(diags, "key generate", "edda-mimir-crypto");
        SYSTEM_ERROR
    }
}

/// Emit the canonical "not yet implemented" diagnostic for a stubbed verb.
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
