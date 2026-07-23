//! Parsed CLI surface: the verb catalogue from `build-system.md` §10, the
//! common-flag struct, and per-verb flag structs.
//!
//! Validation responsibility: this layer captures *syntactic* shape only.
//! Triple/feature/profile *semantic* validation lives in `edda-manifest`
//! and `edda-target` — the CLI stores raw strings so the manifest layer
//! can produce richer diagnostics with the right diagnostic class.

use std::path::PathBuf;

use edda_diag::DiagnosticClass;
use edda_driver::StructureBudgetMode;

/// The parsed CLI invocation.
#[derive(Debug, Clone)]
pub struct Cli {
    /// The verb the user invoked and its associated flags.
    pub verb: Verb,
}

/// The user's verb. Each variant carries the flags valid for that verb.
#[derive(Debug, Clone)]
pub enum Verb {
    /// `edda build` — full cascade for the current target/profile.
    Build(BuildArgs),
    /// `edda check` — stop after typecheck.
    Check(CheckArgs),
    /// `edda run` — compile + execute the resulting binary.
    Run(RunArgs),
    /// `edda hot [member] [-- <args>]` — build, run, and supervise the
    /// target; rebuild + restart on source change.
    Hot(HotArgs),
    /// `edda test` — compile + execute `tests/` entry points.
    Test(TestArgs),
    /// `edda bench` — compile + execute `bench/` entry points.
    Bench(BenchArgs),
    /// `edda gc` — garbage-collect codegen artifacts per §7.
    Gc(GcArgs),
    /// `edda promote <artifact>` — cache tier → repo tier.
    Promote(PromoteArgs),
    /// `edda demote <artifact>` — repo tier → cache tier.
    Demote(DemoteArgs),
    /// `edda regenerate <spec>` — force-regenerate a specialisation.
    Regenerate(RegenerateArgs),
    /// `edda clean` — remove `.edda/build/` (caches untouched).
    Clean(CleanArgs),
    /// `edda fmt` — run the formatter.
    Fmt(FmtArgs),
    /// `edda lint` — run lints without producing output.
    Lint(LintArgs),
    /// `edda daemon` — start the long-lived MCP server.
    Daemon(DaemonArgs),
    /// `edda structmap [<path>]` — emit per-directory `index.toon`
    /// files for the active package (codex `06-tooling.md` §5, §8).
    Structmap(StructmapArgs),
    /// `edda add <name>[@<ver>]` — add a dependency (§8.1).
    Add(AddArgs),
    /// `edda update [<name>]` — update one or all dependencies (§8.2).
    Update(UpdateArgs),
    /// `edda audit` — re-verify every lockfile entry (§8.3).
    Audit(AuditArgs),
    /// `edda publish` — build and publish the current rune (§8.4).
    Publish(PublishArgs),
    /// `edda contract-diff <a> <b>` — surface/effect diff between two rune versions (§8.5).
    ContractDiff(ContractDiffArgs),
    /// `edda why <name>` — transitive provenance trace for a dependency (§8.6).
    Why(WhyArgs),
    /// `edda key <subcommand>` — publisher-key management (currently
    /// `edda key generate`). Clustered with the Mímir verbs because the
    /// keys it mints are the publisher identities `edda publish` / `edda add`
    /// sign with and verify against.
    Key(KeyArgs),
}

impl Verb {
    /// Lowercase verb name as it appears on the command line.
    pub fn name(&self) -> &'static str {
        match self {
            Verb::Build(_) => "build",
            Verb::Check(_) => "check",
            Verb::Run(_) => "run",
            Verb::Hot(_) => "hot",
            Verb::Test(_) => "test",
            Verb::Bench(_) => "bench",
            Verb::Gc(_) => "gc",
            Verb::Promote(_) => "promote",
            Verb::Demote(_) => "demote",
            Verb::Regenerate(_) => "regenerate",
            Verb::Clean(_) => "clean",
            Verb::Fmt(_) => "fmt",
            Verb::Lint(_) => "lint",
            Verb::Daemon(_) => "daemon",
            Verb::Structmap(_) => "structmap",
            Verb::Add(_) => "add",
            Verb::Update(_) => "update",
            Verb::Audit(_) => "audit",
            Verb::Publish(_) => "publish",
            Verb::ContractDiff(_) => "contract-diff",
            Verb::Why(_) => "why",
            Verb::Key(_) => "key",
        }
    }
}

/// Flags shared by every verb that touches the build pipeline.
///
/// Verbs that have no business with a given flag (e.g. `edda fmt` does
/// not honour `--features`) still accept it syntactically — the
/// dispatcher decides which fields to read. This keeps the surface
/// uniform and matches §10's loose phrasing ("common flags across all
/// build commands").
#[derive(Debug, Clone, Default)]
pub struct CommonArgs {
    /// `--target <triple>` override, raw text.
    pub target: Option<String>,
    /// `--features <list>` override, comma-split.
    pub features: Vec<String>,
    /// `--profile <name>` selection.
    pub profile: Option<String>,
    /// `--manifest-path <path>` override.
    pub manifest_path: Option<PathBuf>,
    /// `--warn-as-error <classes>` — escalations applied after manifest overrides.
    pub warn_as_error: Vec<DiagnosticClass>,
    /// `-q` / `-v` adjustments to diagnostic verbosity.
    pub verbosity: Verbosity,
    /// `--jobs <N>` parallelism cap.
    pub jobs: Option<u32>,
    /// `--structure-budget=<off|report|error>` — how the structure-map
    /// token-budget gates behave. Defaults to
    /// [`StructureBudgetMode::Error`] (red-band nodes fail the build).
    pub structure_budget: StructureBudgetMode,
    /// `--freestanding` — emit the OS entry stub and link with no C
    /// runtime and no Rust `edda-rt`.
    pub freestanding: bool,
}

/// Diagnostic verbosity tristate driven by `-q` / `-v`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Verbosity {
    /// `-q` / `--quiet`.
    Quiet,
    /// Neither `-q` nor `-v` given.
    #[default]
    Normal,
    /// `-v` / `--verbose`.
    Verbose,
}

/// `edda build` arguments.
#[derive(Debug, Clone, Default)]
pub struct BuildArgs {
    /// Common build flags.
    pub common: CommonArgs,
    /// `--full-materialization` — materialise every spec invocation, not just reachable ones (§5).
    pub full_materialization: bool,
}

/// `edda check` arguments.
#[derive(Debug, Clone, Default)]
pub struct CheckArgs {
    /// Common build flags.
    pub common: CommonArgs,
    /// `--full-materialization` — materialise every spec invocation (§5).
    pub full_materialization: bool,
}

/// `edda run [member]` arguments.
#[derive(Debug, Clone, Default)]
pub struct RunArgs {
    /// Common build flags.
    pub common: CommonArgs,
    /// Optional positional: the workspace member to build and run.
    /// `None` means the package at `--manifest-path` (or `./package.toml`).
    pub member: Option<String>,
}

/// `edda hot [member] [-- <args>]` arguments.
#[derive(Debug, Clone, Default)]
pub struct HotArgs {
    /// Common build flags.
    pub common: CommonArgs,
    /// Optional positional: the workspace member to build and serve.
    /// `None` means the package at `--manifest-path` (or `./package.toml`).
    pub member: Option<String>,
    /// Arguments after `--`, passed to the supervised child on every spawn.
    pub child_args: Vec<String>,
}

/// `edda test` arguments.
#[derive(Debug, Clone, Default)]
pub struct TestArgs {
    /// Common build flags.
    pub common: CommonArgs,
    /// `--properties`: enable derived property-based-testing per
    /// `corpus/edda-codex/language/06-tooling.md`
    /// (`edda test [--filter <pattern>] [--properties]`). Synthesises
    /// a runner module that invokes every function with at least one
    /// `requires` or `ensures` clause against generated inputs.
    pub properties: bool,
}

/// `edda bench` arguments.
#[derive(Debug, Clone, Default)]
pub struct BenchArgs {
    /// Common build flags.
    pub common: CommonArgs,
}

/// `edda gc` arguments. Per §7's GC pass and §6's tier model.
#[derive(Debug, Clone, Default)]
pub struct GcArgs {
    /// Common flags (only `--manifest-path` and verbosity meaningfully apply).
    pub common: CommonArgs,
    /// `--dry-run` — list what would be removed without removing.
    pub dry_run: bool,
    /// `--cache-tier` — GC the cache tier only (§7).
    pub cache_tier: bool,
    /// `--repo-tier` — GC the repo tier only (§7).
    pub repo_tier: bool,
    /// `--global` — GC the global cache (§6).
    pub global: bool,
}

/// `edda promote <artifact>` arguments.
#[derive(Debug, Clone, Default)]
pub struct PromoteArgs {
    /// Common flags.
    pub common: CommonArgs,
    /// The artifact name (mangled leaf, e.g. `Stack_Option_String__b7e91c2a8f06`).
    pub artifact: String,
}

/// `edda demote <artifact>` arguments.
#[derive(Debug, Clone, Default)]
pub struct DemoteArgs {
    /// Common flags.
    pub common: CommonArgs,
    /// The artifact name (mangled leaf).
    pub artifact: String,
}

/// `edda regenerate <spec>` arguments.
#[derive(Debug, Clone, Default)]
pub struct RegenerateArgs {
    /// Common flags.
    pub common: CommonArgs,
    /// The spec's dotted path (e.g. `std.stack.Stack`).
    pub spec: String,
}

/// `edda clean` arguments.
#[derive(Debug, Clone, Default)]
pub struct CleanArgs {
    /// Common flags (only `--target`/`--profile`/`--manifest-path` meaningfully apply).
    pub common: CommonArgs,
}

/// `edda fmt` arguments.
#[derive(Debug, Clone, Default)]
pub struct FmtArgs {
    /// Common flags (only `--manifest-path` and verbosity meaningfully apply).
    pub common: CommonArgs,
    /// `--strip-comments`: rewrite every `.ea` file under the package,
    /// deleting all comments via the real tokenizer (the
    /// no-comment migration). A `//` inside a string / f-string literal is
    /// preserved.
    pub strip_comments: bool,
}

/// `edda lint` arguments.
#[derive(Debug, Clone, Default)]
pub struct LintArgs {
    /// Common flags (notably `--warn-as-error`).
    pub common: CommonArgs,
    /// `--trust-points`: list every `@unverified` / `@trust` annotation in
    /// the project — the audit surface (codex `06-tooling.md` §8).
    pub trust_points: bool,
    /// `--capability-safe-stdlib`: enforce the stdlib capability discipline —
    /// no stdlib item may shadow a capability or silently elevate effects
    /// (codex `06-tooling.md` §8).
    pub capability_safe_stdlib: bool,
}

/// `edda daemon` arguments. The wire protocol is `mcp-protocol.md` territory.
#[derive(Debug, Clone, Default)]
pub struct DaemonArgs {
    /// Common flags (only `--manifest-path` and verbosity meaningfully apply).
    pub common: CommonArgs,
}

// CLI treats it either as a single-package root (if it contains `package.toml`)
// or as a tree-of-packages and iterates every `package.toml` found beneath.
/// `edda structmap [<path>]` arguments. Codex `06-tooling.md` §5, §8.
#[derive(Debug, Clone, Default)]
pub struct StructmapArgs {
    /// Common flags.
    pub common: CommonArgs,
    /// Optional positional path. Defaults to the current package (cwd).
    pub path: Option<PathBuf>,
    /// `--check`: exit non-zero if any on-disk structmap is stale rather
    /// than rewriting it (codex `06-tooling.md` §8).
    pub check: bool,
}

/// `edda add <name>[@<version>]` arguments (§8.1).
#[derive(Debug, Clone, Default)]
pub struct AddArgs {
    /// Common flags.
    pub common: CommonArgs,
    /// Required positional: `<name>` or `<name>@<version-req>`.
    pub name: String,
    /// `--accept-unstable` — allow pre-release versions.
    pub accept_unstable: bool,
    /// `--max-effects <entry>` (repeatable) — override the effect ceiling for this dep.
    pub max_effects: Vec<String>,
}

/// `edda update [<name>]` arguments (§8.2).
#[derive(Debug, Clone, Default)]
pub struct UpdateArgs {
    /// Common flags.
    pub common: CommonArgs,
    /// Optional positional: name of the single dep to update. `None` means all.
    pub name: Option<String>,
    /// `--accept-surface-change <name>` (repeatable) — allow surface-hash drift for named dep.
    pub accept_surface_change: Vec<String>,
    /// `--accept-publisher-rotation <name>` (repeatable) — allow publisher-key rotation for named dep.
    pub accept_publisher_rotation: Vec<String>,
}

/// `edda audit` arguments (§8.3).
#[derive(Debug, Clone, Default)]
pub struct AuditArgs {
    /// Common flags (only `--manifest-path` and verbosity meaningfully apply).
    pub common: CommonArgs,
    /// `--quiet` — CI-friendly output: exit code only, no per-entry lines.
    pub quiet: bool,
}

/// `edda publish` arguments (§8.4).
#[derive(Debug, Clone, Default)]
pub struct PublishArgs {
    /// Common flags (only `--manifest-path` and verbosity meaningfully apply).
    pub common: CommonArgs,
}

/// `edda contract-diff <a> <b>` arguments (§8.5).
#[derive(Debug, Clone, Default)]
pub struct ContractDiffArgs {
    /// Common flags.
    pub common: CommonArgs,
    /// First version (either `<name>@<version>` or a local `.rune` path).
    pub a: String,
    /// Second version (either `<name>@<version>` or a local `.rune` path).
    pub b: String,
}

/// `edda why <name>` arguments (§8.6).
#[derive(Debug, Clone, Default)]
pub struct WhyArgs {
    /// Common flags.
    pub common: CommonArgs,
    /// Required positional: the dependency name to trace.
    pub name: String,
}

/// `edda key <subcommand>` arguments.
///
/// The top-level `key` verb is a router — argv parsing reads the next
/// token as the subcommand name and lowers it into a [`KeySubcommand`]
/// variant. Today the only landed action is `generate`; the enum shape
/// leaves room for `verify`, `fingerprint`, `import`, `export`, etc.
/// without breaking the locked §10 verb catalogue.
#[derive(Debug, Clone)]
pub struct KeyArgs {
    /// Common flags. Only `--manifest-path` and verbosity meaningfully apply.
    pub common: CommonArgs,
    /// The chosen key subcommand.
    pub subcommand: KeySubcommand,
}

/// The `edda key` action.
#[derive(Debug, Clone)]
pub enum KeySubcommand {
    /// `edda key generate` — mint a fresh ed25519 publisher keypair and
    /// persist it to the keystore.
    Generate(KeyGenerateArgs),
}

/// `edda key generate` arguments.
#[derive(Debug, Clone, Default)]
pub struct KeyGenerateArgs {
    /// `--keystore <dir>` — override the platform-default keystore root.
    /// Useful for tests and for shared-keystore deployments.
    pub keystore: Option<PathBuf>,
    /// `--label <name>` — when present, key files land under
    /// `<keystore>/<label>/` instead of `<keystore>/<fingerprint>/`.
    pub label: Option<String>,
}
