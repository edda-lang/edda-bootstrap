//! Internal lowered form of a CLI / MCP invocation.
//!
//! The [`Command`](crate::command::Command) + [`BuildOptions`](crate::command::BuildOptions)
//! pair the CLI and MCP wire formats produce gets lowered to a
//! [`RunPlan`] before the cascade runs. The lowered form names exactly
//! one cascade stop point and one materialisation policy, so the
//! orchestrator has a single decision tree to walk.

use crate::command::Command;

/// A cascade stage. Used by [`RunPlan::stop_after`] to bound how far
/// the cascade runs.
///
/// The order matches `build-system.md` §5 ("The cascade order is parse
/// → import-resolve → typecheck → codegen → compile → link"). Each
/// variant compares less-than to all later variants; the cascade
/// orchestrator runs stage K iff `K <= plan.stop_after`.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Debug)]
pub enum Stage {
    /// Token + AST production per file.
    Parse,
    /// Source-import graph + per-module name resolution.
    ImportResolve,
    /// Type inference + effect-row checking.
    Typecheck,
    /// Reachability-driven spec materialisation.
    Codegen,
    /// Per-module object emission.
    Compile,
    /// Final binary / library assembly.
    Link,
}

/// Internal lowered form of a CLI / MCP invocation.
#[derive(Clone, Debug)]
pub struct RunPlan {
    /// The originating verb. Carried through so the §10 summary line
    /// and any "stage X not yet implemented" diagnostics can name the
    /// command the user issued.
    pub command: Command,
    /// The latest stage the cascade is permitted to run. Inclusive:
    /// `Stage::Typecheck` means parse + import-resolve + typecheck all
    /// run; codegen does not.
    pub stop_after: Stage,
    /// `--full-materialization`: bypass §5 reachability-driven codegen.
    /// Carried even for stop points before codegen so the daemon's
    /// long-lived state knows the policy without re-deriving it.
    pub full_materialization: bool,
}

impl RunPlan {
    /// Lower a [`Command`] into its plan per `build-system.md` §10.
    pub fn for_command(command: Command, full_materialization: bool) -> Self {
        let stop_after = match &command {
            Command::Check | Command::Lint | Command::Structmap => Stage::Codegen,
            Command::Fmt | Command::Clean => Stage::Parse,
            Command::Build | Command::Run | Command::Test | Command::Bench => Stage::Link,
            // Mímir verbs bypass the cascade entirely — they are dispatched
            // via `run_mimir` and never reach `RunPlan::for_command`. Assigning
            // `Stage::Parse` is a safe sentinel so the type is well-formed
            // even if this arm is somehow reached.
            Command::Add(_)
            | Command::Update(_)
            | Command::Audit(_)
            | Command::Publish(_)
            | Command::ContractDiff(_)
            | Command::Why(_)
            | Command::KeyGenerate(_) => Stage::Parse,
        };
        Self {
            command,
            stop_after,
            full_materialization,
        }
    }
}
