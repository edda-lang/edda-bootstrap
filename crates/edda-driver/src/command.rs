//! CLI / MCP verb surface per `docs/tooling/build-system.md` §10.
//!
//! [`Command`] is the public entry the CLI and MCP wire forms both
//! lower to before reaching the driver. [`BuildOptions`] carries the
//! common-flag set so callers do not need to mutate the parsed
//! manifest before invoking the cascade.

use std::path::PathBuf;

use edda_diag::DiagnosticClass;

/// The §10 verb catalogue.
///
/// Each variant corresponds 1:1 to a CLI command and an MCP operation
/// per `docs/tooling/build-system.md` §10. The driver lowers a
/// [`Command`] plus [`BuildOptions`] into an internal
/// [`crate::plan::RunPlan`] that names the cascade stop point.
///
/// # Stage coverage
///
/// [`Command::Build`] runs the full cascade through link;
/// [`Command::Check`] / [`Command::Lint`] / [`Command::Structmap`] stop
/// after their relevant pass. The Mímir verbs (`Add` / `Update` /
/// `Audit` / `Publish` / `ContractDiff` / `Why` / `KeyGenerate`) are
/// dispatched through `run_mimir`. The §10 table is the authoritative
/// verb set.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum Command {
    /// `edda build` — full cascade for the active target / profile.
    Build,
    /// `edda check` — stop after the typecheck pass.
    Check,
    /// `edda run` — compile and execute the resulting binary.
    Run,
    /// `edda test` — compile and execute test entry points under `tests/`.
    Test,
    /// `edda bench` — compile and execute bench entry points under `bench/`.
    Bench,
    /// `edda clean` — remove every build-pipeline-emitted directory under
    /// the package root: `codegen/` (repo-tier instantiated spec
    /// artifacts), `.edda/` (cache-tier manifest, sharded cache artifacts,
    /// and linked binaries), and `target/edda/` (per-target MIR + object
    /// files). The manifest and source tree are preserved.
    Clean,
    /// `edda fmt` — run the formatter (text-level only, no cascade).
    Fmt,
    /// `edda lint` — run lints without producing output.
    Lint,
    /// `edda structmap` — emit the codex `schema_version = 4` structure
    /// map per `06-tooling.md` §5. Runs parse + resolve + typecheck and
    /// then writes per-directory `index.toon` files; no codegen /
    /// compile / link stages.
    Structmap,
    /// `edda add <name>[@<ver>]` — resolve, fetch, verify and pin a dep (§8.1).
    Add(AddCommand),
    /// `edda update [<name>]` — bump deps within caret/tilde range (§8.2).
    Update(UpdateCommand),
    /// `edda audit` — re-verify all lockfile entries against cached bytes (§8.3).
    Audit(AuditCommand),
    /// `edda publish` — build, sign, and upload the current rune (§8.4).
    Publish(PublishCommand),
    /// `edda contract-diff <a> <b>` — surface/effect diff between two rune versions (§8.5).
    ContractDiff(ContractDiffCommand),
    /// `edda why <name>` — transitive provenance trace for a dependency (§8.6).
    Why(WhyCommand),
    /// `edda key generate` — mint and persist an ed25519 publisher keypair (slice F).
    KeyGenerate(KeyGenerateCommand),
}

impl Command {
    /// The lowercase verb name as it appears on the CLI and as the
    /// `verb` field of the §10 summary line.
    pub fn name(&self) -> &'static str {
        match self {
            Command::Build => "build",
            Command::Check => "check",
            Command::Run => "run",
            Command::Test => "test",
            Command::Bench => "bench",
            Command::Clean => "clean",
            Command::Fmt => "fmt",
            Command::Lint => "lint",
            Command::Structmap => "structmap",
            Command::Add(_) => "add",
            Command::Update(_) => "update",
            Command::Audit(_) => "audit",
            Command::Publish(_) => "publish",
            Command::ContractDiff(_) => "contract-diff",
            Command::Why(_) => "why",
            Command::KeyGenerate(_) => "key",
        }
    }

    /// `true` for commands whose §10 row touches the parse-and-resolve
    /// pass (everything except `clean` and `fmt`). The cascade driver
    /// uses this to decide whether to materialise the source graph.
    pub fn runs_cascade(&self) -> bool {
        matches!(
            self,
            Command::Build
                | Command::Check
                | Command::Run
                | Command::Test
                | Command::Bench
                | Command::Lint
                | Command::Structmap
        )
        // Mímir verbs (Add/Update/Audit/Publish/ContractDiff/Why) do not run the
        // compilation cascade — they operate directly on the manifest / lockfile /
        // registry. The match above deliberately excludes them.
    }
}

/// `--structure-budget` mode: how the structure-map token-budget gates
/// behave for one invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum StructureBudgetMode {
    /// Skip the budget pass entirely.
    Off,
    /// Downgrade every worklist entry to a non-failing warning — the
    /// migration escape hatch for restructuring a tree that is not yet
    /// under budget. Not the default: an easy blanket opt-out is exactly
    /// what lets the rule get routed around.
    Report,
    /// Enforce: red-band nodes/spines fail the build at the class's
    /// effective severity; amber stays a warning. The default — the budget
    /// is enforced across the codebase, not advised.
    #[default]
    Error,
}

impl StructureBudgetMode {
    /// Parse the `--structure-budget=<v>` value; `None` for any
    /// unrecognised spelling.
    pub fn from_flag_str(s: &str) -> Option<Self> {
        match s {
            "off" => Some(StructureBudgetMode::Off),
            "report" => Some(StructureBudgetMode::Report),
            "error" => Some(StructureBudgetMode::Error),
            _ => None,
        }
    }
}

/// Per-invocation options that override or supplement manifest defaults.
///
/// Mirrors the common-flag set in `build-system.md` §10 — every flag
/// listed under "Common flags across all build commands" has a field
/// here. The driver materialises manifest defaults into the
/// corresponding [`edda_manifest::PackageManifest`] fields and overlays
/// these CLI values on top.
#[derive(Clone, Debug)]
pub struct BuildOptions {
    /// Path to the `package.toml` for this build. Defaults to
    /// `./package.toml` when [`Self::for_default_manifest`] is used.
    pub manifest_path: PathBuf,
    /// `--target <triple>`: when `Some`, overrides
    /// `manifest.build.default_target`.
    pub target_override: Option<String>,
    /// `--features <list>`: features to enable in addition to those in
    /// `manifest.build.default_features`.
    pub feature_override: Vec<String>,
    /// `--profile <name>`: when `Some`, overrides
    /// `manifest.build.default_profile`.
    pub profile_override: Option<String>,
    /// `--full-materialization`: bypass §5 reachability-driven codegen.
    pub full_materialization: bool,
    /// `--jobs <N>`: parallelism cap; `None` defers to the host default.
    pub jobs: Option<usize>,
    /// `--warn-as-error <classes>`: each class is escalated to
    /// [`edda_diag::LintSeverity::Error`] on top of any manifest-level
    /// override. Applied by [`crate::Driver::initialize`] after manifest
    /// `lints` are loaded so the CLI wins per `build-system.md` §3.
    pub warn_as_error: Vec<DiagnosticClass>,
    /// `--properties`: enable derived property-based-testing on the
    /// `edda test` verb per `corpus/edda-codex/language/06-tooling.md`
    /// (`edda test [--filter <pattern>] [--properties]`). When `true`,
    /// the cascade discovers every function with at least one
    /// `requires` or `ensures` clause, generates inputs from each
    /// param's refinement structure via the
    /// [`crate::properties`] strategy table, and synthesises an
    /// addressable runner module that the Pass-2 reentry harness
    /// drives through typecheck + compile. Ignored on non-Test verbs.
    pub properties: bool,
    /// `--structure-budget=<off|report|error>`: how the structure-map
    /// token-budget gates behave. Defaults to
    /// [`StructureBudgetMode::Error`] — red-band nodes/spines fail the
    /// build; the budget is an enforced language property.
    pub structure_budget: StructureBudgetMode,
    /// `--freestanding`: emit the per-target OS entry stub and link with
    /// no C runtime and no Rust `edda-rt` — the runtime-in-Edda closeout.
    /// Opt-in: a program that still
    /// references `__edda_*` runtime symbols not yet provided by the Edda
    /// stdlib will fail the link with an undefined-symbol error. Today
    /// only the Windows entry stub is emitted.
    pub freestanding: bool,
    /// `structmap --check`: compare each emitted `index.toon` against the
    /// on-disk file and report a `parse_error` (build-error exit) for every
    /// stale or missing one, **without** rewriting any file. Codex
    /// `06-tooling.md` §8 — the CI staleness gate.
    pub structmap_check: bool,
    /// `lint --trust-points`: emit the audit listing of every
    /// `@unverified` / `@trust` annotation in the resolved package. Codex
    /// `06-tooling.md` §8.
    pub lint_trust_points: bool,
    /// `lint --capability-safe-stdlib`: enforce the stdlib capability
    /// discipline — no item may shadow a locked capability nominal type,
    /// and no function's effect row may name an ambient capability not
    /// backed by one of its own parameters. Codex `06-tooling.md` §8.
    pub lint_capability_safe_stdlib: bool,
}

/// Minimal options for Mímir verbs (add / update / audit / publish / contract-diff / why).
///
/// These verbs do not run the compilation cascade; they only need the
/// manifest path to locate the `package.toml` and its adjacent
/// `package.lock.toml`. The richer [`BuildOptions`] type is for cascade
/// verbs; this type avoids forcing callers to fabricate unused fields.
#[derive(Clone, Debug)]
pub struct MimirOptions {
    /// Path to the `package.toml` for this invocation. Defaults to
    /// `./package.toml` when not overridden.
    pub manifest_path: PathBuf,
}

impl MimirOptions {
    /// Sentinel options that target `./package.toml`.
    pub fn for_default_manifest() -> Self {
        Self { manifest_path: PathBuf::from("package.toml") }
    }
}

// ---------------------------------------------------------------------------
// Mímir command payload structs
// ---------------------------------------------------------------------------

/// Payload for `Command::Add`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddCommand {
    /// Raw `<name>` or `<name>@<version-req>` positional.
    pub name_at_version: String,
    /// `--accept-unstable` — allow pre-release versions.
    pub accept_unstable: bool,
    /// `--max-effects <entry>` (repeatable) overrides.
    pub max_effects: Vec<String>,
}

/// Payload for `Command::Update`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateCommand {
    /// `None` → update all deps; `Some(name)` → update only that dep.
    pub name: Option<String>,
    /// `--accept-surface-change <name>` (repeatable).
    pub accept_surface_change: Vec<String>,
    /// `--accept-publisher-rotation <name>` (repeatable).
    pub accept_publisher_rotation: Vec<String>,
}

/// Payload for `Command::Audit`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditCommand {
    /// `--quiet` — CI-friendly output: exit code only.
    pub quiet: bool,
}

/// Payload for `Command::Publish`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishCommand {}

/// Payload for `Command::ContractDiff`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContractDiffCommand {
    /// First version spec or `.rune` path.
    pub a: String,
    /// Second version spec or `.rune` path.
    pub b: String,
}

/// Payload for `Command::Why`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WhyCommand {
    /// The dependency name to trace transitively.
    pub name: String,
}

/// Payload for the `edda key generate` driver entry point.
///
/// The CLI surfaces a nested-subcommand verb (`Verb::Key(KeyArgs)`); the
/// driver side flattens it to a single dispatch row per action so each
/// landed key-management subcommand has its own payload type without
/// requiring callers to pattern-match into a nested enum.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct KeyGenerateCommand {
    /// `--keystore <dir>` override; `None` means use
    /// [`edda_mimir_crypto::default_keystore_dir`].
    pub keystore: Option<PathBuf>,
    /// `--label <name>` override; `None` means use the key's canonical
    /// fingerprint as the per-key subdirectory name.
    pub label: Option<String>,
}

impl BuildOptions {
    /// Sentinel options that target `./package.toml` with no overrides.
    pub fn for_default_manifest() -> Self {
        Self {
            manifest_path: PathBuf::from("package.toml"),
            target_override: None,
            feature_override: Vec::new(),
            profile_override: None,
            full_materialization: false,
            jobs: None,
            warn_as_error: Vec::new(),
            properties: false,
            structure_budget: StructureBudgetMode::Error,
            freestanding: false,
            structmap_check: false,
            lint_trust_points: false,
            lint_capability_safe_stdlib: false,
        }
    }
}
