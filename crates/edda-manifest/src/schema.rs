//! Locked manifest schema (build-system.md §3).
//!
//! Types only — TOON-to-schema validation and default population live in
//! [`crate::parse`].

use edda_diag::LintConfig;
use edda_span::Span;
use edda_target::TargetTriple;

/// A fully-parsed `package.toml` manifest.
#[derive(Clone, Debug)]
pub struct PackageManifest {
    /// `package`: globally-unique identifier of the package.
    pub package: Box<str>,
    /// `version`: SemVer 2.0 version string.
    pub version: SemVer,
    /// `root_namespace`: the dot-prefix the package's modules are exposed under.
    pub root_namespace: Box<str>,
    /// Resolved dependencies in source order.
    pub dependencies: Vec<Dependency>,
    /// Build defaults (default target / features / profile).
    pub build: BuildConfig,
    /// Named profiles. `dev`, `release`, `bench` are always present
    /// (locked defaults applied when the manifest does not override them).
    pub profiles: ProfileSet,
    /// User-supplied lint overrides; merge with `--warn-as-error` at the
    /// driver layer to produce the effective `LintConfig`.
    pub lints: LintConfig,
    /// `codegen` block — tier policy, GC schedule, compression, global cache.
    pub codegen: CodegenConfig,
    /// Optional `[workspace]` table — populated when the manifest declares
    /// member packages under `lib/<member>/`. The driver iterates each
    /// member's own `package.toml` and runs the cascade per member with
    /// the sibling layouts registered as implicit dependencies.
    pub workspace: Option<WorkspaceTable>,
    /// `[structmap]` block — emitter knobs for compiler-native
    /// `index.toon` generation. Defaults apply when the table is absent.
    pub structmap: StructmapConfig,
    /// `[package].max_trust` — optional package-wide ceiling on the count
    /// of `@unverified` / `@trust` verification escape hatches. When set,
    /// the resolver emits a hard error if the count exceeds it, ratcheting
    /// the trust budget per 03-verification.md §3.
    /// `None` (the default) imposes no ceiling, so manifests without the
    /// key keep parsing and building unchanged.
    pub max_trust: Option<u32>,
    /// `[package].edition` — optional descriptive language-edition string
    /// (codex `06-tooling.md` §9.3). Captured verbatim; the bootstrap does
    /// not yet act on it. `None` when the key is absent.
    pub edition: Option<Box<str>>,
    /// `[package].authors` — optional list of author strings (§9.3).
    /// Empty when the key is absent.
    pub authors: Vec<Box<str>>,
    /// `[package].license` — optional SPDX license string (§9.3).
    /// Captured verbatim. `None` when the key is absent.
    pub license: Option<Box<str>>,
    /// `[package].description` — optional one-line package description
    /// (§9.3). `None` when the key is absent.
    pub description: Option<Box<str>>,
    /// `[package].kind` — optional output-artifact kind request
    /// (the everyday-toolchain half of the dynamic-library output-kind
    /// feature). `None` (the default) keeps the legacy driver split: a
    /// member with a `main` entry links an executable, a mainless member
    /// archives a static library. `Some(_)` routes the link stage
    /// explicitly, which is the only way to request a dynamic library.
    pub kind: Option<PackageKind>,
}

/// `[package] kind` values — the output-artifact kind a package requests.
///
/// String forms accepted in the manifest: `"executable"`,
/// `"static_library"`, `"dynamic_library"`. The driver maps this onto
/// `edda_link::OutputKind` at link time; the manifest crate stays
/// link-agnostic.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum PackageKind {
    /// `kind = "executable"` — link a binary under `bin/`.
    Executable,
    /// `kind = "static_library"` — archive the member's objects under `lib/`.
    StaticLibrary,
    /// `kind = "dynamic_library"` — link a `.dll` / `.so` / `.dylib` under `lib/`.
    DynamicLibrary,
}

impl PackageKind {
    /// The manifest string form of this kind.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Executable => "executable",
            Self::StaticLibrary => "static_library",
            Self::DynamicLibrary => "dynamic_library",
        }
    }

    /// Parse the manifest string form; `None` for any other string.
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "executable" => Some(Self::Executable),
            "static_library" => Some(Self::StaticLibrary),
            "dynamic_library" => Some(Self::DynamicLibrary),
            _ => None,
        }
    }
}

/// `[workspace]` table — lists the workspace's members.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceTable {
    /// Member paths under `<manifest_dir>/lib/`. POSIX-style relative paths
    /// admit nested layouts like `"core/option"` or `"crypto/aead/aes_gcm"`
    /// — stdlib uses one level of subsystem nesting (`lib/core/option/`,
    /// `lib/io/stream/`); runes may use deeper. Cross-member implicit
    /// dependency registration keys by each member's declared
    /// `root_namespace`, not by directory path, so two members with the
    /// same leaf name in different subsystems do not collide.
    ///
    /// Empty when [`discover`](Self::discover) is set — the driver fills
    /// in the list from a filesystem walk at workspace-resolve time.
    pub members: Vec<Box<str>>,
    /// Auto-discovery mode (B-011). When set, [`members`](Self::members)
    /// is filled by walking the discovery root for `package.toml` files
    /// instead of being hard-coded. `Some(_)` and a non-empty `members`
    /// list are mutually exclusive — the parser rejects manifests that
    /// set both.
    pub discover: Option<WorkspaceDiscover>,
    /// Default member for a bare `edda run` at this workspace root.
    /// When set, `edda run` with no
    /// `<member>` positional and no `--manifest-path` builds + launches
    /// `lib/<default_run>/package.toml`, exactly as if the user had typed
    /// `edda run <default_run>`; when unset, a bare workspace `edda run` is
    /// still rejected with the member list. The value is a `lib/`-relative
    /// member path in the same shape as a [`members`](Self::members) entry.
    /// Snake-case (`default_run`) for parity with the manifest's other
    /// `default_*` keys (`default_target` / `default_profile` /
    /// `default_features` / `default_tier`).
    pub default_run: Option<Box<str>>,
}

/// Workspace-member auto-discovery mode for `[workspace] discover = …`.
///
/// `discover = true` walks `lib/` for every directory containing a
/// `package.toml`. `discover = "<relative-path>"` walks the named
/// directory instead — useful for non-`lib`-rooted layouts. Either form
/// makes the filesystem the source of truth for workspace membership,
/// replacing the hand-maintained `members = [...]` list.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceDiscover {
    /// `discover = true` — walk `lib/` under the manifest's directory.
    LibRoot,
    /// `discover = "<path>"` — walk the named directory under the
    /// manifest's directory. The path is admitted in the same POSIX-style
    /// relative form as a `members` entry.
    Path(Box<str>),
}

/// Top-level manifest that declares only `[workspace]` (no `[package]`).
///
/// Cargo-style virtual manifest: when a `package.toml` lives at the root
/// of a workspace and the root directory contains no `src/` subtree, the
/// `[package]` block is optional. The driver never builds the root as a
/// package in that case — it just enumerates `workspace.members` and
/// runs a cascade per member. `[lints]` and `[codegen]` are still honoured
/// at the root so per-workspace defaults stay expressible.
#[derive(Clone, Debug)]
pub struct WorkspaceManifest {
    /// User-supplied lint overrides at the workspace root.
    pub lints: LintConfig,
    /// `codegen` block — tier policy, GC schedule, compression, global cache.
    pub codegen: CodegenConfig,
    /// `[workspace]` table — always populated for this variant.
    pub workspace: WorkspaceTable,
    /// `[structmap]` block — drives the aggregated root `index.toon` shape.
    pub structmap: StructmapConfig,
}

/// Result of loading or parsing a top-level `package.toml`.
///
/// `Package` is the long-standing single-package shape with the full
/// §3 schema. `WorkspaceOnly` is the Cargo-style virtual manifest:
/// `[package]` is absent because the root is purely a workspace
/// container (`[workspace]` present, no `src/` sibling). Callers that
/// require a real package (everything that constructs a [`Driver`])
/// should pattern-match and surface their own error on the workspace
/// branch.
#[derive(Clone, Debug)]
pub enum LoadedManifest {
    /// The manifest declared `[package]`.
    Package(PackageManifest),
    /// The manifest declared `[workspace]` only and the directory has no `src/`.
    WorkspaceOnly(WorkspaceManifest),
}

impl LoadedManifest {
    /// `true` when this is the workspace-only variant.
    pub fn is_workspace_only(&self) -> bool {
        matches!(self, LoadedManifest::WorkspaceOnly(_))
    }

    /// Borrow the `[workspace]` table when one is present (either variant).
    pub fn workspace_table(&self) -> Option<&WorkspaceTable> {
        match self {
            LoadedManifest::Package(p) => p.workspace.as_ref(),
            LoadedManifest::WorkspaceOnly(w) => Some(&w.workspace),
        }
    }

    /// Borrow the `[structmap]` block — always populated on either variant.
    pub fn structmap(&self) -> &StructmapConfig {
        match self {
            LoadedManifest::Package(p) => &p.structmap,
            LoadedManifest::WorkspaceOnly(w) => &w.structmap,
        }
    }
}

/// SemVer 2.0 version parsed from `package.toml`'s `version` field.
///
/// Pre-release and build-metadata suffixes are captured verbatim — full
/// validation of their internal grammar is deferred until dependency
/// resolution actually compares versions (the full dependency-resolution
/// algorithm is reserved for later).
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct SemVer {
    /// Major version.
    pub major: u32,
    /// Minor version.
    pub minor: u32,
    /// Patch version.
    pub patch: u32,
    /// Pre-release identifiers (everything after `-`, before `+`).
    pub pre_release: Option<Box<str>>,
    /// Build metadata (everything after `+`).
    pub build: Option<Box<str>>,
}

/// One row of the `[[dependencies]]` table (§3 base + §6 Mímir extension fields).
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Dependency {
    /// `name` column.
    pub name: Box<str>,
    /// `version` column — a SemVer requirement, captured verbatim.
    pub version_req: Box<str>,
    /// `source` column, resolved to its named scheme.
    pub source: DependencySource,
    /// §6.2 — pinned BLAKE3 of the dep's public surface at first install.
    ///
    /// Text form: `"blake3:<lowercase-hex>"`. `None` is the first-install
    /// state; `edda add` writes this field on first resolution. The prefix
    /// is the algorithm discriminator; hex length is not strictly enforced
    /// here to leave room for algorithm agility.
    pub surface_hash: Option<Box<str>>,
    /// §6.3 — maximum allowed effect-row entries for this dependency.
    ///
    /// Each string uses the same path-qualified leaf-form notation as
    /// Edda effect rows everywhere else (e.g. `"err: alloc.AllocError"`,
    /// `"cancellation"`, `"Filesystem"`, `"alloc(bytes <= 4096)"`).
    /// An empty list (`max_effects = []`) is valid and requires the dep
    /// be fully pure. Typed parsing of each entry is intentionally deferred
    /// to slice F (hash walker) and slice H (CLI), because the typed
    /// effect-row IR lives in `edda-types`, not `edda-manifest` — pulling
    /// it in here would create an unwanted crate dependency.
    pub max_effects: Vec<Box<str>>,
    /// §6.4 — whether pre-release versions of this dep are admitted.
    ///
    /// Defaults to `false` when absent from the manifest.
    pub accept_unstable: bool,
    /// §6.5 — publisher public-key pin for this dependency.
    ///
    /// `None` is the first-install state; subsequent `edda add --update`
    /// writes this field. Once set, the pin must match the signing key
    /// on every resolution to guard against publisher-key rotation attacks.
    pub publisher: Option<PublisherPin>,
}

/// Publisher public-key pin nested inside a `[[dependencies]]` entry (§6.5).
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct PublisherPin {
    /// Ed25519 public-key fingerprint. Text form: `"ed25519:<lowercase-hex>"`.
    /// The prefix is the algorithm discriminator; hex length is not strictly
    /// validated here to allow future algorithm agility.
    pub key_fingerprint: Box<str>,
}

/// Dependency-source schemes per §3.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum DependencySource {
    /// Resolve from the package registry (default scheme).
    Registry,
    /// Resolve from a git URL (`git+<url>`).
    Git(Box<str>),
    /// Resolve from a relative path (`path+<rel-path>`).
    Path(Box<str>),
}

/// `build` block of `package.toml` (§3).
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct BuildConfig {
    /// `default_target` — used when `--target` is not supplied on the CLI.
    pub default_target: Option<TargetTriple>,
    /// `default_features[]` — feature names captured verbatim with their
    /// source spans. The driver validates them against the active target's
    /// arch via `edda_target::FeatureSet::insert`. If `default_target` is
    /// set at parse time, [`crate::parse::parse`] emits
    /// `unknown_target_feature` for any reject, pointing at the
    /// per-feature span.
    pub default_features: Vec<FeatureSpec>,
    /// `default_profile` — name of the profile to use when `--profile` is
    /// not supplied. Defaults to `"dev"` per §3.
    pub default_profile: Box<str>,
    /// `emit_structmap` — when true, the compiler emits per-directory
    /// `index.toon` files as a side-effect of `edda build` / `edda check`
    /// per `06-tooling.md` §5. Defaults to `true`; set to `false` in
    /// `package.toml` to suppress side-effect emission (the standalone
    /// `edda structmap` verb is unaffected).
    pub emit_structmap: bool,
}

impl Default for BuildConfig {
    fn default() -> Self {
        Self {
            default_target: None,
            default_features: Vec::new(),
            default_profile: Box::default(),
            emit_structmap: true,
        }
    }
}

/// One entry of a manifest's `default_features[]` list, paired with the
/// source span so per-feature diagnostics can point at the offending token.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct FeatureSpec {
    /// Feature name (e.g. `"avx2"`, `"sse4.2"`).
    pub name: Box<str>,
    /// Span covering the name inside the source file.
    pub span: Span,
}

/// Ordered map of profile names → settings (§3 `profiles` table).
///
/// The three locked profiles (`dev`, `release`, `bench`) are pre-populated
/// with the §3 default-value rows. Manifest entries replace those
/// defaults; additional custom profiles are appended in source order.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct ProfileSet {
    entries: Vec<(Box<str>, Profile)>,
}

impl ProfileSet {
    /// Construct a profile set pre-populated with the three locked defaults.
    pub fn with_locked_defaults() -> Self {
        ProfileSet {
            entries: vec![
                ("dev".into(), Profile::locked_dev()),
                ("release".into(), Profile::locked_release()),
                ("bench".into(), Profile::locked_bench()),
            ],
        }
    }

    /// Return the [`Profile`] for `name`, or `None` if no such profile exists.
    pub fn get(&self, name: &str) -> Option<&Profile> {
        self.entries
            .iter()
            .find(|(n, _)| n.as_ref() == name)
            .map(|(_, p)| p)
    }

    /// Insert or replace the profile entry for `name`.
    pub fn set(&mut self, name: Box<str>, profile: Profile) {
        if let Some(slot) = self.entries.iter_mut().find(|(n, _)| *n == name) {
            slot.1 = profile;
        } else {
            self.entries.push((name, profile));
        }
    }

    /// Iterate over `(name, profile)` pairs in source order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Profile)> {
        self.entries.iter().map(|(n, p)| (n.as_ref(), p))
    }

    /// Number of profiles defined (always ≥ 3 after parse).
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

/// One row of the `profiles{name,opt_level,debug_info,sanitizers[]}` table.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Profile {
    /// Optimisation level (0..=3).
    pub opt_level: u8,
    /// Debug-info kind.
    pub debug_info: DebugInfo,
    /// Sanitizer names captured verbatim. The backend (`edda-compile`)
    /// validates the set; this crate does not lock the accepted names.
    pub sanitizers: Vec<Box<str>>,
}

impl Profile {
    /// `dev` locked default: opt_level=0, debug_info=full, sanitizers=[address].
    pub fn locked_dev() -> Self {
        Profile {
            opt_level: 0,
            debug_info: DebugInfo::Full,
            sanitizers: vec!["address".into()],
        }
    }

    /// `release` locked default: opt_level=3, debug_info=line-tables-only.
    pub fn locked_release() -> Self {
        Profile {
            opt_level: 3,
            debug_info: DebugInfo::LineTablesOnly,
            sanitizers: Vec::new(),
        }
    }

    /// `bench` locked default: opt_level=3, debug_info=full.
    pub fn locked_bench() -> Self {
        Profile {
            opt_level: 3,
            debug_info: DebugInfo::Full,
            sanitizers: Vec::new(),
        }
    }
}

/// `debug_info` column values (§3 default-value table).
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum DebugInfo {
    /// Full DWARF / CodeView debug info.
    Full,
    /// Line-table-only debug info (release profile default).
    LineTablesOnly,
}

impl DebugInfo {
    /// `lowercase_kebab-case` spec spelling.
    pub const fn name(self) -> &'static str {
        match self {
            DebugInfo::Full => "full",
            DebugInfo::LineTablesOnly => "line-tables-only",
        }
    }

    /// Parse a debug-info value from its §3 spelling.
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "full" => Some(DebugInfo::Full),
            "line-tables-only" => Some(DebugInfo::LineTablesOnly),
            _ => None,
        }
    }
}

/// `codegen` block (§3 + §6 + §7).
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct CodegenConfig {
    /// `default_tier`: `auto` (chain-origin rule per §6) or `cache` (force all into cache tier).
    pub default_tier: DefaultTier,
    /// `gc_schedule`: per-tier GC tags (§7).
    pub gc_schedule: GcSchedule,
    /// `compression`: cache-tier compression algorithm (§6).
    pub compression: Compression,
    /// `global_cache`: per-project opt-in to `~/.edda/global-cache/` (§6).
    pub global_cache: GlobalCache,
}

impl CodegenConfig {
    /// Spec-locked defaults applied when the manifest omits a field.
    pub fn locked_defaults() -> Self {
        CodegenConfig {
            default_tier: DefaultTier::Auto,
            gc_schedule: GcSchedule::locked_defaults(),
            compression: Compression::None,
            global_cache: GlobalCache::Disabled,
        }
    }
}

/// Values of the `codegen.default_tier` field (§6).
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum DefaultTier {
    /// Apply the chain-origin rule: repo tier for project-origin
    /// artifacts, cache tier for stdlib / third-party-origin artifacts.
    Auto,
    /// Force every new artifact into the cache tier.
    Cache,
}

impl DefaultTier {
    /// Spec spelling used in `package.toml`.
    pub const fn name(self) -> &'static str {
        match self {
            DefaultTier::Auto => "auto",
            DefaultTier::Cache => "cache",
        }
    }

    /// Parse from the spec spelling.
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(DefaultTier::Auto),
            "cache" => Some(DefaultTier::Cache),
            _ => None,
        }
    }
}

/// `codegen.gc_schedule` sub-block (§7).
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct GcSchedule {
    /// Schedule for `.edda/cache/codegen/`.
    pub cache_tier: GcTag,
    /// Schedule for the in-repo tier.
    pub repo_tier: GcTag,
    /// Schedule for `~/.edda/global-cache/`.
    pub global_cache: GcTag,
}

impl GcSchedule {
    /// Spec-locked defaults: cache_tier=weekly, repo_tier=never, global_cache=never.
    pub const fn locked_defaults() -> Self {
        GcSchedule {
            cache_tier: GcTag::Weekly,
            repo_tier: GcTag::Never,
            global_cache: GcTag::Never,
        }
    }
}

/// Locked GC-schedule named tags (§7).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum GcTag {
    /// `never` — GC never runs; user invokes `edda gc` manually.
    Never,
    /// `on_ci` — GC runs once per CI build (build detects CI via `CI=1`).
    OnCi,
    /// `daily` — first build of each UTC day.
    Daily,
    /// `weekly` — first build of each UTC week (Monday-anchored).
    Weekly,
    /// `on_promote_revoke` — after every `edda demote` action.
    OnPromoteRevoke,
}

impl GcTag {
    /// `lowercase_snake_case` spec spelling.
    pub const fn name(self) -> &'static str {
        match self {
            GcTag::Never => "never",
            GcTag::OnCi => "on_ci",
            GcTag::Daily => "daily",
            GcTag::Weekly => "weekly",
            GcTag::OnPromoteRevoke => "on_promote_revoke",
        }
    }

    /// Parse a GC tag from its spec spelling.
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "never" => Some(GcTag::Never),
            "on_ci" => Some(GcTag::OnCi),
            "daily" => Some(GcTag::Daily),
            "weekly" => Some(GcTag::Weekly),
            "on_promote_revoke" => Some(GcTag::OnPromoteRevoke),
            _ => None,
        }
    }
}

/// `codegen.compression` field (§6).
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Compression {
    /// No compression.
    None,
    /// zstd compression on cache-tier artifacts (§6).
    Zstd,
}

impl Compression {
    /// Spec spelling used in `package.toml`.
    pub const fn name(self) -> &'static str {
        match self {
            Compression::None => "false",
            Compression::Zstd => "zstd",
        }
    }

    /// Parse from the spec spelling.
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "false" => Some(Compression::None),
            "zstd" => Some(Compression::Zstd),
            _ => None,
        }
    }
}

/// `codegen.global_cache` field (§6).
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum GlobalCache {
    /// Project opts in to the user-level global cache.
    Enabled,
    /// Project does not consult the global cache.
    Disabled,
}

impl GlobalCache {
    /// Spec spelling used in `package.toml`.
    pub const fn name(self) -> &'static str {
        match self {
            GlobalCache::Enabled => "enabled",
            GlobalCache::Disabled => "disabled",
        }
    }

    /// Parse from the spec spelling.
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "enabled" => Some(GlobalCache::Enabled),
            "disabled" => Some(GlobalCache::Disabled),
            _ => None,
        }
    }
}

/// BPE encoding the structure-map token-budget gates measure against.
/// Mirrors `edda_structmap::TokenEncoding`; kept local so `edda-manifest`
/// stays free of an `edda-structmap` dependency (the driver maps between
/// the two when building the budget config).
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum TokenBudgetEncoding {
    /// `o200k_base` — GPT-4o / modern-model encoding. The default.
    O200kBase,
    /// `cl100k_base` — GPT-4 / GPT-3.5-turbo encoding.
    Cl100kBase,
}

impl TokenBudgetEncoding {
    /// Canonical lowercase config spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            TokenBudgetEncoding::O200kBase => "o200k_base",
            TokenBudgetEncoding::Cl100kBase => "cl100k_base",
        }
    }

    /// Parse a config spelling; `None` for any unrecognised string.
    pub fn from_config_str(s: &str) -> Option<Self> {
        match s {
            "o200k_base" => Some(TokenBudgetEncoding::O200kBase),
            "cl100k_base" => Some(TokenBudgetEncoding::Cl100kBase),
            _ => None,
        }
    }
}

/// `[structmap]` block — knobs that control compiler-native `index.toon`
/// emission (codex `06-tooling.md` §5) and the token-budget gates that
/// replaced the line-based `structure_map_too_dense` heuristic.
///
/// The table is optional and every field has a spec-locked default
/// applied by [`crate::parse::parse`] when the table (or a given key) is
/// absent, so existing `package.toml` files keep parsing.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct StructmapConfig {
    /// When `true`, the root `index.toon` inlines the items of every
    /// descendant directory in the package, producing a single
    /// project-wide index the next agent can consume in one read instead
    /// of walking per-directory files. Per-directory `index.toon` files
    /// are still emitted so readers walking from a subdirectory still
    /// resolve. Defaults to `false`.
    pub descendant_tree: bool,
    /// Gate A (per-node) green ceiling, in tokens. Default 6000.
    pub node_green_max: usize,
    /// Gate A red threshold, in tokens. Default 6000.
    pub node_amber_max: usize,
    /// BPE encoding the gates measure against. Default `o200k_base`.
    pub token_budget_encoding: TokenBudgetEncoding,
    /// Fallback chars-per-token ratio × 100 (used only when the BPE
    /// tokenizer is unavailable). Default 350 (= 3.5).
    pub chars_per_token_centi: u32,
}

impl StructmapConfig {
    /// Spec-locked defaults applied when the manifest omits the `[structmap]` table.
    pub const fn locked_defaults() -> Self {
        StructmapConfig {
            descendant_tree: false,
            node_green_max: 6_000,
            node_amber_max: 6_000,
            token_budget_encoding: TokenBudgetEncoding::O200kBase,
            chars_per_token_centi: 350,
        }
    }
}

impl Default for StructmapConfig {
    fn default() -> Self {
        Self::locked_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_locked_defaults_match_spec() {
        let dev = Profile::locked_dev();
        assert_eq!(dev.opt_level, 0);
        assert_eq!(dev.debug_info, DebugInfo::Full);
        assert_eq!(
            dev.sanitizers.iter().map(|s| s.as_ref()).collect::<Vec<_>>(),
            vec!["address"]
        );

        let release = Profile::locked_release();
        assert_eq!(release.opt_level, 3);
        assert_eq!(release.debug_info, DebugInfo::LineTablesOnly);
        assert!(release.sanitizers.is_empty());

        let bench = Profile::locked_bench();
        assert_eq!(bench.opt_level, 3);
        assert_eq!(bench.debug_info, DebugInfo::Full);
        assert!(bench.sanitizers.is_empty());
    }

    #[test]
    fn profile_set_starts_with_three_locked_profiles() {
        let set = ProfileSet::with_locked_defaults();
        assert_eq!(set.len(), 3);
        let names: Vec<_> = set.iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["dev", "release", "bench"]);
        assert!(set.get("dev").is_some());
        assert!(set.get("release").is_some());
        assert!(set.get("bench").is_some());
        assert!(set.get("custom").is_none());
    }

    #[test]
    fn profile_set_replace_preserves_position() {
        let mut set = ProfileSet::with_locked_defaults();
        let custom = Profile {
            opt_level: 2,
            debug_info: DebugInfo::Full,
            sanitizers: Vec::new(),
        };
        set.set("dev".into(), custom.clone());
        let names: Vec<_> = set.iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["dev", "release", "bench"]);
        assert_eq!(set.get("dev"), Some(&custom));
    }

    #[test]
    fn profile_set_append_new_profile() {
        let mut set = ProfileSet::with_locked_defaults();
        set.set(
            "fuzz".into(),
            Profile {
                opt_level: 1,
                debug_info: DebugInfo::Full,
                sanitizers: vec!["address".into(), "ub".into()],
            },
        );
        let names: Vec<_> = set.iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["dev", "release", "bench", "fuzz"]);
    }

    #[test]
    fn debug_info_round_trip() {
        for di in [DebugInfo::Full, DebugInfo::LineTablesOnly] {
            assert_eq!(DebugInfo::from_name(di.name()), Some(di));
        }
        assert_eq!(DebugInfo::from_name("none"), None);
        assert_eq!(DebugInfo::from_name(""), None);
    }

    #[test]
    fn default_tier_round_trip() {
        assert_eq!(DefaultTier::from_name("auto"), Some(DefaultTier::Auto));
        assert_eq!(DefaultTier::from_name("cache"), Some(DefaultTier::Cache));
        // The earlier `repo` tier was dropped — must not parse.
        assert_eq!(DefaultTier::from_name("repo"), None);
    }

    #[test]
    fn gc_tag_round_trip() {
        for tag in [
            GcTag::Never,
            GcTag::OnCi,
            GcTag::Daily,
            GcTag::Weekly,
            GcTag::OnPromoteRevoke,
        ] {
            assert_eq!(GcTag::from_name(tag.name()), Some(tag));
        }
        assert_eq!(GcTag::from_name("monthly"), None);
    }

    #[test]
    fn compression_round_trip() {
        assert_eq!(Compression::from_name("false"), Some(Compression::None));
        assert_eq!(Compression::from_name("zstd"), Some(Compression::Zstd));
        // `lz4` is reserved for v1.0 per §6; not admitted at v0.1.
        assert_eq!(Compression::from_name("lz4"), None);
        assert_eq!(Compression::from_name("true"), None);
    }

    #[test]
    fn global_cache_round_trip() {
        assert_eq!(GlobalCache::from_name("enabled"), Some(GlobalCache::Enabled));
        assert_eq!(GlobalCache::from_name("disabled"), Some(GlobalCache::Disabled));
        assert_eq!(GlobalCache::from_name(""), None);
    }

    #[test]
    fn gc_schedule_defaults_match_spec() {
        let s = GcSchedule::locked_defaults();
        assert_eq!(s.cache_tier, GcTag::Weekly);
        assert_eq!(s.repo_tier, GcTag::Never);
        assert_eq!(s.global_cache, GcTag::Never);
    }

    #[test]
    fn codegen_defaults_match_spec() {
        let c = CodegenConfig::locked_defaults();
        assert_eq!(c.default_tier, DefaultTier::Auto);
        assert_eq!(c.compression, Compression::None);
        assert_eq!(c.global_cache, GlobalCache::Disabled);
        assert_eq!(c.gc_schedule, GcSchedule::locked_defaults());
    }

    #[test]
    fn structmap_defaults_match_spec() {
        let s = StructmapConfig::locked_defaults();
        assert!(!s.descendant_tree);
        assert_eq!(s, StructmapConfig::default());
    }
}
