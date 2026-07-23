//! Structure-map token-budget gates — the token-based successor to the
//! line-based `structure_map_too_dense` heuristic.
//!
//! Two gates run over the rendered `index.toon` tree:
//!
//! - **Gate A — per-node cap (primary).** Every directory's `index.toon`
//!   must fall within the per-node token band. A directory that hoards
//!   too much local interface goes red on *its own* node cost — the
//!   honest, self-localising modularization signal: it blames the fat
//!   node, not a descendant. The remedy is to push files down into child
//!   directories.
//! - **Gate B — scale-free structure (secondary).** Gate B carries NO
//!   token budget. A spine token cap (absolute, then overhead) always had
//!   the quota disease: a dimensioned number that must be re-tuned as the
//!   codebase grows — the one thing nature never does (an aorta has no
//!   token budget). So Gate B instead gates dimensionless STRUCTURE, the
//!   way distribution networks (vasculature, river deltas, trees) bound
//!   access cost without any fixed quota:
//!   * **Law 1 — lean hub (conduit, not reservoir).** A directory WITH
//!     children must hold a *minority* of its own subtree's interface
//!     (`own/subtree ≤ 1/3` green, `> 1/2` red). A ratio → scale-free → a
//!     10× codebase keeps the same threshold. A lean conduit passes even
//!     when its subtree is huge; a hub hoarding the majority is flagged at
//!     any absolute size. Remedy: distribute its own files down.
//!   * **Law 2 — earn your place (carry or branch).** A directory must
//!     justify its existence: it must CARRY (hold ≥1 of its own `.ea`
//!     files) or BRANCH (≥2 children). One that does neither — 0 own
//!     files AND exactly one child — is a pure conduit-of-one (an empty
//!     wrapper whose `index.toon` says only "descend to the one child"),
//!     a vine link holding nothing; merge it. A single-child directory
//!     that carries even one file is a legitimate thin node — the
//!     "ingest only the maps on your path" property makes thin carrying
//!     levels cheap, so depth is fine as long as every level carries (and
//!     Law 1 keeps each lean).
//!   * **Law 3 — endpoints sized to function (the read cap).** The one
//!     legitimate absolute is the per-node read cap (Gate A) — a
//!     single-sitting ingestion limit set by the *reader*, not the
//!     codebase, so it never scales. A leaf over it with a call-graph cut
//!     is split; one with no cut (a mutual-recursion core) is a seed,
//!     accepted whole via atomic-defer.
//!   Together, lean hubs + real branching make the spine bound itself
//!   logarithmically — there is no spine number to maintain, ever, and
//!   "max reached → auto-green" falls out as "the structure is already
//!   optimal for this codebase's genuine size."
//!
//! Both gates are deterministic functions of the directory tree, so this
//! is a clean compile-time check. The pass measures real BPE tokens (see
//! [`crate::token`]) — `bytes × constant` is unstable across the
//! signature-dense vs routing-dense content mix.
//!
//! # Consuming-model calibration
//!
//! The consumer of `index.toon` is a Claude Opus 4.8 agent, but no Opus
//! tokenizer exists offline (the compiler must stay deterministic and
//! network-free). So the gate measures a stable offline substrate
//! (`o200k_base`) exactly and scales the count by
//! [`DEFAULT_MODEL_CALIBRATION`] (≈2.5, measured: 17226 o200k ↔ ~43000
//! Opus on `lib/types/src/check`) to estimate Opus tokens. The ceilings
//! are therefore expressed in Opus 4.8 tokens; the worklist reports
//! Opus-equivalent costs.
//!
//! # Spine measurement and the workspace skeleton
//!
//! [`analyze_budget`] is called per package, rooted at that package's
//! `package_root`. In a workspace the `parent:` chain deliberately
//! breaks at each member root and members are siblings under `lib/`, so
//! a per-member spine captures the entire content spine *except* the two
//! workspace-skeleton nodes (repo-root + `lib/`, < ~600 tokens combined)
//! — which are spine noise that contributes nothing. The driver notes
//! this exclusion on Gate B diagnostics.
//!
//! This pass only measures, gates, and diagnoses; it does not change
//! `index.toon` semantics (a routing/interface split is a separate,
//! later concern).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::INDEX_FILENAME;
use crate::model::{DirEntry, Tree};
use crate::token::{TokenEncoding, Tokenizer, TokenizerKind};

/// Which gate a [`WorklistEntry`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Gate A — per-node cap. The primary modularization driver.
    PerNode,
    /// Gate B — spine budget. The depth backstop.
    Spine,
}

impl Gate {
    /// Single-letter label used in the worklist surface (`A` / `B`).
    pub fn label(self) -> char {
        match self {
            Gate::PerNode => 'A',
            Gate::Spine => 'B',
        }
    }
}

/// Severity band a measured node or spine falls into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    /// Within budget — no action.
    Green,
    /// Over the green ceiling but under the red one — warning + hint.
    Amber,
    /// Over the red ceiling — error (hard, post-rollout).
    Red,
}

/// One fat file inside a red directory — a file whose own interface alone
/// exceeds the node ceiling AND has a valid internal call-graph seam.
/// Relocating such a file cannot bring its directory under budget; it
/// must be partitioned internally at the named seam. (A fat file with no
/// seam is exempt-atomic, not a `FatFile`.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FatFile {
    /// The fat file's basename.
    pub file: String,
    /// Its own interface token cost (consuming-model tokens).
    pub tokens: usize,
    /// The smallest separable function cluster to extract into a sibling
    /// module.
    pub cluster: Vec<String>,
    /// The dispatcher whose removal exposed the seam (hub-and-spoke).
    pub dispatcher: Option<String>,
}

/// The single, self-classifying resolution a flagged directory carries.
/// Derived from the call graph + the structural laws, not chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Directive {
    /// Gate A, wide fan-out: the directory's files split into `groups`
    /// call-disjoint components (fan each into a sibling subdirectory one
    /// level down) and/or carry `fat_files` whose own interface alone
    /// exceeds the read cap (each must be partitioned internally or
    /// deferred).
    WideSplit {
        /// ALL call-disjoint file components in the directory.
        groups: Vec<Vec<String>>,
        /// Files whose own interface alone exceeds the node ceiling.
        fat_files: Vec<FatFile>,
    },
    /// Gate A, partition-separable: split `file` at the named seam —
    /// `cluster`'s functions form a group disjoint from the rest. The
    /// genuinely-single-cohesive-file case (one call-connected group, no
    /// fat file).
    Partition {
        /// The file to split.
        file: String,
        /// The function cluster to extract into a sibling module.
        cluster: Vec<String>,
        /// The dispatcher whose removal exposed the seam (hub-and-spoke).
        dispatcher: Option<String>,
    },
    /// Gate B, atomic-hoard: a directory hosts ≥2 call-disjoint
    /// *exempt-atomic* files (each a cohesive, non-partitionable unit) whose
    /// combined interface exceeds the single-read cap. A lone atomic file
    /// of any size is exempt (its own leaf would still be over-cap, nothing
    /// to gain); ≥2 independent ones co-located is a genuine multi-file
    /// directory problem — give each its own leaf so each over-cap read is
    /// at least isolated. `groups` are the call-disjoint components of the
    /// exempt files (one prospective leaf each).
    AtomicHoard {
        /// The exempt-atomic files hoarded in this directory.
        files: Vec<String>,
        /// Combined interface token cost of `files` (consuming-model tokens).
        tokens: usize,
        /// Call-disjoint components of the exempt files — candidate leaves.
        groups: Vec<Vec<String>>,
    },
    /// Gate B, Law 1 (lean hub): a directory WITH children holds the
    /// majority of its own subtree's interface — a reservoir, not a
    /// conduit. Distribute its own files DOWN into children. `percent` is
    /// `own × 100 / subtree`; `groups` are the call-disjoint components of
    /// its own files (candidate distribution boundaries).
    HoardingHub {
        /// `own_interface × 100 / subtree_interface`.
        percent: usize,
        /// Call-disjoint components of the hub's own files.
        groups: Vec<Vec<String>>,
    },
    /// Gate B, Law 2 (earn-your-place): an EMPTY WRAPPER — a directory
    /// that carries no files of its own and has exactly one child, so it
    /// neither carries nor branches. Merge it into its child (remove the
    /// hop). A single-child directory that holds even one file is NOT
    /// flagged — it earns its place.
    Flatten,
}

impl Directive {
    /// Short kebab-case class name for the diagnostic surface.
    pub fn class(&self) -> &'static str {
        match self {
            Directive::WideSplit { .. } => "wide-split",
            Directive::Partition { .. } => "partition",
            Directive::AtomicHoard { .. } => "atomic-hoard",
            Directive::HoardingHub { .. } => "hoarding-hub",
            Directive::Flatten => "vine",
        }
    }
}

/// Multiplier from o200k_base substrate tokens to estimated Claude Opus
/// 4.8 tokens. No offline Opus tokenizer exists (Anthropic publishes none
/// for current models and the compiler must stay offline/deterministic),
/// so this is the empirical bridge. Refine with more `(o200k, Opus)`
/// measurement pairs; assumes the default `o200k_base` substrate (a
/// different encoding needs a different factor).
pub const DEFAULT_MODEL_CALIBRATION: f64 = 2.5;

/// Law 1 green threshold (lean hub): `own_interface / subtree_interface`
/// at or below `NUM/DEN` (= 1/3) is a lean conduit. Stricter than the
/// majority line so the gate leans hard into deep modularization.
pub const HUB_LEAN_GREEN_NUM: usize = 1;
/// Denominator of [`HUB_LEAN_GREEN_NUM`].
pub const HUB_LEAN_GREEN_DEN: usize = 3;
/// Law 1 red threshold: `own_interface / subtree_interface` above
/// `NUM/DEN` (= 1/2, the majority) is a hoarding reservoir.
pub const HUB_LEAN_RED_NUM: usize = 1;
/// Denominator of [`HUB_LEAN_RED_NUM`].
pub const HUB_LEAN_RED_DEN: usize = 2;

/// Budget thresholds + tokenizer selection. [`Self::locked_defaults`] is
/// the spec §4 starting calibration.
///
/// **Gate B is scale-free structural (2026-06-08).** It earlier gated an
/// absolute spine (8000 → 30000), then the spine *overhead*; both carried
/// a token number that has to be re-tuned as the codebase grows — the one
/// thing nature never does (an aorta has no token budget). The redesign
/// replaces the spine budget with two dimensionless laws over the
/// directory tree: **Law 1 (lean hub)** — a directory with children must
/// hold a minority of its own subtree's interface (`own/subtree ≤ 1/3`
/// green, `> 1/2` red); **Law 2 (no vine)** — a single-child directory
/// isn't a branch, merge it. Together with **Law 3** (the per-node read
/// cap + atomic-defer for indivisible "seed" leaves) the spine bounds
/// itself logarithmically with no spine number to maintain. The only
/// absolute left is the read cap, and it is legitimate precisely because
/// it is reader-grounded (a single-sitting ingestion limit), not a
/// structure budget — so it never scales with the codebase.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BudgetConfig {
    /// Gate A green ceiling — at or below this is healthy (default 6000).
    pub node_green_max: usize,
    /// Gate A red threshold — above this is a hard violation (default 6000).
    /// Also the Law 1 subtree floor + the Law 2 merge cap: the read cap is
    /// the one absolute, reused everywhere.
    pub node_amber_max: usize,
    /// BPE encoding to measure against (default `o200k_base`).
    pub encoding: TokenEncoding,
    /// Fallback chars-per-token ratio, used only when the BPE tokenizer
    /// is unavailable (default 3.5).
    pub chars_per_token: f64,
    /// Substrate-token → consuming-model-token multiplier (default
    /// [`DEFAULT_MODEL_CALIBRATION`], ≈2.5 for o200k → Opus 4.8). Applied
    /// to every measured node cost so the ceilings can be expressed in
    /// the consuming model's tokens.
    pub model_calibration: f64,
}

impl BudgetConfig {
    /// Spec §4 starting calibration. Treat the constants as tunable
    /// against observed agent performance — the durable artifact is the
    /// gate structure, not these numbers.
    pub const fn locked_defaults() -> Self {
        BudgetConfig {
            node_green_max: 6_000,
            node_amber_max: 6_000,
            encoding: TokenEncoding::O200kBase,
            chars_per_token: 3.5,
            model_calibration: DEFAULT_MODEL_CALIBRATION,
        }
    }
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self::locked_defaults()
    }
}

/// One directory's measured node cost and band.
#[derive(Debug, Clone)]
pub struct NodeMeasure {
    /// Absolute directory path whose `index.toon` was measured.
    pub dir: PathBuf,
    /// Real token cost of the directory's serialized `index.toon`.
    pub token_cost: usize,
    /// Count of the node's own declared exports (types + functions),
    /// parsed from the rendered `types[N]` / `functions[N]` headers.
    pub export_count: usize,
    /// Gate A band for this node.
    pub band: Band,
}

/// Legacy spine-marginal-contributor type. No longer constructed since
/// Gate B became scale-free structural (the flagged directory is named
/// directly); retained for wire stability of [`WorklistEntry`].
#[derive(Debug, Clone)]
pub struct MarginalContributor {
    /// The directory.
    pub dir: PathBuf,
    /// Its node token cost.
    pub token_cost: usize,
}

/// One row of the restructuring worklist.
#[derive(Debug, Clone)]
pub struct WorklistEntry {
    /// Which gate fired.
    pub gate: Gate,
    /// The flagged directory (Gate A: the over-cap node; Gate B: the
    /// hoarding hub or the vine link).
    pub node: PathBuf,
    /// Gate A: the node's token cost. Gate B Law 1: the hub's own
    /// interface cost. Gate B Law 2: the vine's combined own interface.
    pub token_cost: usize,
    /// The (possibly dynamic) green ceiling the cost is measured against —
    /// Gate A: `node_green_max`; Law 1: the lean-conduit share of the
    /// subtree; Law 2: the read cap.
    pub ceiling: usize,
    /// The sort key (descending) — see the invariant above.
    pub overage: usize,
    /// Legacy spine field — always `None` since Gate B became structural.
    pub worst_path: Option<PathBuf>,
    /// Legacy spine field — always `None` since Gate B became structural.
    pub marginal_contributor: Option<MarginalContributor>,
    /// The self-classifying directive for this entry. `None` for advisory
    /// (amber) entries. Gate A: `WideSplit`/`Partition`; Gate B:
    /// `HoardingHub` (Law 1) / `Flatten` (Law 2) / `AtomicHoard`.
    pub directive: Option<Directive>,
    /// The band that produced this entry.
    pub band: Band,
}

/// Result of [`analyze_budget`]: per-node measures plus the sorted
/// worklist.
#[derive(Debug, Clone)]
pub struct BudgetReport {
    /// One entry per measured directory, in lexical path order.
    pub measures: Vec<NodeMeasure>,
    /// The full violation set across both gates, sorted by overage.
    pub worklist: Vec<WorklistEntry>,
    /// Whether real BPE tokens or the `chars_per_token` estimate were
    /// used — the driver warns on [`TokenizerKind::Fallback`].
    pub tokenizer_kind: TokenizerKind,
    /// The package root the tree was measured against.
    pub root: PathBuf,
}

/// Run both budget gates over the rendered `index.toon` tree.
///
/// `files` is the path → TOON text mapping from [`crate::emit`]; `root`
/// is the package root; `cfg` carries the thresholds + tokenizer
/// selection. When `tree` is `Some`, each Gate A red node is classified
/// into a self-classifying [`Directive`] from its call graph AND the Gate
/// B structural laws run; pass `None` for measurement-only use (tests),
/// which runs Gate A alone.
pub fn analyze_budget(
    files: &BTreeMap<PathBuf, String>,
    root: &Path,
    cfg: &BudgetConfig,
    tree: Option<&Tree>,
) -> BudgetReport {
    let tokenizer = Tokenizer::new(cfg.encoding, cfg.chars_per_token);

    // dir -> (token_cost, export_count)
    let mut nodes: BTreeMap<PathBuf, (usize, usize)> = BTreeMap::new();
    for (path, text) in files {
        if path.file_name() != Some(std::ffi::OsStr::new(INDEX_FILENAME)) {
            continue;
        }
        let dir = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.to_path_buf());
        // Measure exactly in the substrate BPE, then scale to the
        // consuming model's tokens so the cost is comparable to the
        // Opus-4.8-denominated ceilings.
        let raw = tokenizer.count(text);
        let token_cost = (raw as f64 * cfg.model_calibration).round() as usize;
        let export_count = parse_export_count(text);
        nodes.insert(dir, (token_cost, export_count));
    }

    let mut measures = Vec::with_capacity(nodes.len());
    let mut worklist = Vec::new();

    // Gate A — per node. The node is banded against the PROJECTION
    // (`raw − Σ exempt-atomic interface`): a lone non-partitionable file is
    // something the gate cannot act on (it has no seam to split and its own
    // leaf would still be over-cap), so it is subtracted out and rendered in
    // full. The gate then governs only the content it CAN act on — fat
    // partitionable files and call-disjoint groups. Hoards of ≥2 exempt
    // files re-enter the budget via Gate B's `AtomicHoard`.
    for (dir, (token_cost, export_count)) in &nodes {
        let exempt_sum = tree
            .and_then(|t| t.dirs.get(dir))
            .map(|entry| exempt_atomic_files(entry, &tokenizer, cfg).iter().map(|(_, c)| c).sum())
            .unwrap_or(0usize);
        let projected = token_cost.saturating_sub(exempt_sum);
        let band = node_band(projected, cfg);
        measures.push(NodeMeasure {
            dir: dir.clone(),
            token_cost: *token_cost,
            export_count: *export_count,
            band,
        });
        if matches!(band, Band::Green) {
            continue;
        }
        // Generated output is not the consumer's structure to fix — it is
        // measured (NodeMeasure above) but never gated.
        if is_generated_dir(dir, root) {
            continue;
        }
        // Only red nodes get a self-classifying directive; amber is
        // advisory (no agent action). `classify_red_node` operates on the
        // non-exempt content (it filters exempt-atomic files internally),
        // so it never produces an atomic directive — that case is exempt.
        let directive = if matches!(band, Band::Red) {
            tree.and_then(|t| t.dirs.get(dir))
                .and_then(|entry| classify_red_node(entry, projected, cfg.node_amber_max, &tokenizer, cfg))
        } else {
            None
        };
        let overage = projected.saturating_sub(cfg.node_green_max);
        worklist.push(WorklistEntry {
            gate: Gate::PerNode,
            node: dir.clone(),
            token_cost: projected,
            ceiling: cfg.node_green_max,
            overage,
            worst_path: None,
            marginal_contributor: None,
            directive,
            band,
        });
    }

    // Gate B — scale-free structure (Law 1 lean hub + Law 2 no vine).
    // Needs the tree for per-directory OWN interface costs; measurement-
    // only callers (tests) pass `tree = None` and get Gate A alone.
    if let Some(t) = tree {
        gate_b_structure(t, root, &tokenizer, cfg, &mut worklist);
    }

    worklist.sort_by(|a, b| b.overage.cmp(&a.overage).then_with(|| a.node.cmp(&b.node)));

    BudgetReport {
        measures,
        worklist,
        tokenizer_kind: tokenizer.kind(),
        root: root.to_path_buf(),
    }
}

/// Classify a red directory into its single self-classifying directive,
/// considering only the directory's non-exempt (partitionable) files.
fn classify_red_node(
    entry: &DirEntry,
    _node_cost: usize,
    ceiling: usize,
    tokenizer: &Tokenizer,
    cfg: &BudgetConfig,
) -> Option<Directive> {
    let exempt = exempt_file_set(entry);
    let file_costs: Vec<(String, usize)> = file_costs_descending(entry, tokenizer, cfg)
        .into_iter()
        .filter(|(file, _)| !exempt.contains(file))
        .collect();

    // 1. Fat non-exempt files — own interface alone over ceiling. Each has
    //    a valid internal seam (non-exempt ⟺ `classify_call_graph -> Some`),
    //    so it is always a partition, never an atomic defer.
    let mut fat_files: Vec<FatFile> = Vec::new();
    for (file, cost) in &file_costs {
        if *cost <= ceiling {
            continue;
        }
        if let Some(seam) = crate::cohesion::classify_call_graph(&entry.functions, &file_fn_idxs(entry, file)) {
            fat_files.push(FatFile {
                file: file.clone(),
                tokens: *cost,
                // Smallest cluster — fewest connected obligations, smallest
                // follow-up rewrite at call sites.
                cluster: seam.components.last().cloned().unwrap_or_default(),
                dispatcher: seam.dispatcher,
            });
        }
    }

    // 2. ALL call-disjoint non-exempt file groups (one per WCC).
    let groups = dir_file_components_excluding(entry, &exempt);

    // 3. Wide fan-out: ≥2 disjoint groups and/or a fat file.
    if groups.len() >= 2 || !fat_files.is_empty() {
        return Some(Directive::WideSplit { groups, fat_files });
    }

    // 4. Single partitionable file: split it at its seam.
    for (file, _) in &file_costs {
        if let Some(seam) = crate::cohesion::classify_call_graph(&entry.functions, &file_fn_idxs(entry, file)) {
            return Some(Directive::Partition {
                file: file.clone(),
                cluster: seam.components.last().cloned().unwrap_or_default(),
                dispatcher: seam.dispatcher,
            });
        }
    }
    None
}

/// The set of a directory's exempt-atomic file basenames (functions ∪
/// types). A file with no functions (types-only) has an empty call graph →
/// no seam → exempt.
fn exempt_file_set(entry: &DirEntry) -> BTreeSet<String> {
    let mut files: BTreeSet<String> = BTreeSet::new();
    for f in &entry.functions {
        files.insert(f.file.clone());
    }
    for t in &entry.types {
        files.insert(t.file.clone());
    }
    files
        .into_iter()
        .filter(|file| {
            crate::cohesion::classify_call_graph(&entry.functions, &file_fn_idxs(entry, file))
                .is_none()
        })
        .collect()
}

/// The directory's exempt-atomic files paired with their interface token
/// cost. Reused by the Gate-A projection and the Gate-B atomic-hoard gate.
fn exempt_atomic_files(
    entry: &DirEntry,
    tokenizer: &Tokenizer,
    cfg: &BudgetConfig,
) -> Vec<(String, usize)> {
    exempt_file_set(entry)
        .into_iter()
        .map(|file| {
            let rows = crate::emit::render_file_rows(entry, &file);
            let cost = (tokenizer.count(&rows) as f64 * cfg.model_calibration).round() as usize;
            (file, cost)
        })
        .collect()
}

/// Call-disjoint file components restricted to files NOT in `exempt`.
fn dir_file_components_excluding(entry: &DirEntry, exempt: &BTreeSet<String>) -> Vec<Vec<String>> {
    let functions: Vec<crate::model::FunctionEntry> = entry
        .functions
        .iter()
        .filter(|f| !exempt.contains(&f.file))
        .cloned()
        .collect();
    crate::cohesion::dir_file_components(&functions)
}

/// Call-disjoint file components restricted to files IN `keep`.
fn dir_file_components_only(entry: &DirEntry, keep: &BTreeSet<String>) -> Vec<Vec<String>> {
    let functions: Vec<crate::model::FunctionEntry> = entry
        .functions
        .iter()
        .filter(|f| keep.contains(&f.file))
        .cloned()
        .collect();
    crate::cohesion::dir_file_components(&functions)
}

/// Indices into `entry.functions` of the functions declared in `file`.
fn file_fn_idxs(entry: &DirEntry, file: &str) -> Vec<usize> {
    entry
        .functions
        .iter()
        .enumerate()
        .filter(|(_, fe)| fe.file == file)
        .map(|(i, _)| i)
        .collect()
}

/// Per-file interface token costs for a directory, descending.
fn file_costs_descending(
    entry: &DirEntry,
    tokenizer: &Tokenizer,
    cfg: &BudgetConfig,
) -> Vec<(String, usize)> {
    let mut files: BTreeSet<String> = BTreeSet::new();
    for f in &entry.functions {
        files.insert(f.file.clone());
    }
    for t in &entry.types {
        files.insert(t.file.clone());
    }
    let mut out: Vec<(String, usize)> = files
        .into_iter()
        .map(|file| {
            let rows = crate::emit::render_file_rows(entry, &file);
            let cost = (tokenizer.count(&rows) as f64 * cfg.model_calibration).round() as usize;
            (file, cost)
        })
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// Sum the `types[N]` and `functions[N]` header counts in a rendered
/// `index.toon`. These are the node's own interface items; the
/// `children[N]` row is counts-only routing and is deliberately excluded.
fn parse_export_count(text: &str) -> usize {
    let mut total = 0;
    for line in text.lines() {
        let trimmed = line.trim_start();
        for prefix in ["types[", "functions["] {
            if let Some(rest) = trimmed.strip_prefix(prefix) {
                if let Some(end) = rest.find(']') {
                    if let Ok(n) = rest[..end].parse::<usize>() {
                        total += n;
                    }
                }
            }
        }
    }
    total
}

/// Gate A band for one node.
fn node_band(token_cost: usize, cfg: &BudgetConfig) -> Band {
    if token_cost > cfg.node_amber_max {
        Band::Red
    } else if token_cost > cfg.node_green_max {
        Band::Amber
    } else {
        Band::Green
    }
}

/// Gate B: apply Law 1 (lean hub) + Law 2 (earn-your-place) over `tree`.
fn gate_b_structure(
    tree: &Tree,
    root: &Path,
    tokenizer: &Tokenizer,
    cfg: &BudgetConfig,
    worklist: &mut Vec<WorklistEntry>,
) {
    let read_cap = cfg.node_amber_max;

    // own[D] = D's own (non-deferred) interface token cost.
    let own: BTreeMap<&Path, usize> = tree
        .dirs
        .iter()
        .map(|(d, e)| (d.as_path(), own_interface(e, tokenizer, cfg)))
        .collect();

    // Immediate child directories per directory (index.toon-bearing).
    let mut child_dirs: BTreeMap<&Path, Vec<&Path>> = BTreeMap::new();
    for d in tree.dirs.keys() {
        if let Some(p) = d.parent() {
            if tree.dirs.contains_key(p) {
                child_dirs.entry(p).or_default().push(d.as_path());
            }
        }
    }

    // subtree[D] = own[D] + Σ subtree[child] (DFS, memoised).
    let mut subtree: BTreeMap<&Path, usize> = BTreeMap::new();
    for d in tree.dirs.keys() {
        subtree_interface(d.as_path(), &own, &child_dirs, &mut subtree, 0);
    }

    for (dir, entry) in &tree.dirs {
        if is_generated_dir(dir, root) {
            continue; // generated output is never gated
        }

        // --- AtomicHoard: ≥2 call-disjoint exempt-atomic files whose
        //     combined interface exceeds the read cap. Each is individually
        //     irreducible (no seam, own leaf still over cap), so a lone one
        //     is exempt; but ≥2 INDEPENDENT ones co-located force a reader
        //     to ingest multiple over-budget interfaces in one sitting —
        //     give each its own leaf so each over-cap read is isolated.
        //     Applies to leaves and hubs alike (it is about the files IN
        //     this directory, not its children). ---
        let exempts = exempt_atomic_files(entry, tokenizer, cfg);
        if exempts.len() >= 2 {
            let combined: usize = exempts.iter().map(|(_, c)| c).sum();
            let exempt_set: BTreeSet<String> = exempts.iter().map(|(f, _)| f.clone()).collect();
            let groups = dir_file_components_only(entry, &exempt_set);
            if combined > read_cap && groups.len() >= 2 {
                worklist.push(WorklistEntry {
                    gate: Gate::Spine,
                    node: dir.clone(),
                    token_cost: combined,
                    ceiling: read_cap,
                    overage: combined.saturating_sub(read_cap),
                    worst_path: None,
                    marginal_contributor: None,
                    directive: Some(Directive::AtomicHoard {
                        files: exempt_set.into_iter().collect(),
                        tokens: combined,
                        groups,
                    }),
                    band: Band::Red,
                });
            }
        }

        let kids = child_dirs.get(dir.as_path()).map(Vec::as_slice).unwrap_or(&[]);
        if kids.is_empty() {
            continue; // leaf — Gate A / Law 3 governs the endpoint
        }
        let own_d = own[dir.as_path()];

        // --- Law 2: every directory must EARN its place — CARRY (≥1 of
        //     its own `.ea` files) or BRANCH (≥2 children). One that does
        //     neither — 0 own files AND exactly 1 child — is a pure
        //     conduit-of-one (a vine link holding nothing); merge it into
        //     its child. A single child that DOES carry is a legitimate
        //     thin node — it falls through to Law 1, not flagged here.
        if kids.len() == 1 && entry.modules.is_empty() && dir.as_path() != root {
            worklist.push(WorklistEntry {
                gate: Gate::Spine,
                node: dir.clone(),
                token_cost: own[kids[0]], // the child content this empty level wraps
                ceiling: 0,               // an empty wrapper earns no own budget
                overage: own[kids[0]],    // sort heuristic (bigger wrapped child first)
                worst_path: None,
                marginal_contributor: None,
                directive: Some(Directive::Flatten),
                band: Band::Red,
            });
            continue;
        }

        // --- Law 1: a hub with ≥2 children must be a lean conduit. ---
        let sub = subtree[dir.as_path()];
        // Below the read cap the whole subsystem reads in one sitting — a
        // seed; nothing to distribute, exempt regardless of internal shape.
        if sub <= read_cap {
            continue;
        }
        let band = hub_band(own_d, sub);
        if matches!(band, Band::Green) {
            continue;
        }
        let percent = own_d.saturating_mul(100) / sub.max(1);
        let green_share = sub.saturating_mul(HUB_LEAN_GREEN_NUM) / HUB_LEAN_GREEN_DEN;
        let groups = crate::cohesion::dir_file_components(&entry.functions);
        worklist.push(WorklistEntry {
            gate: Gate::Spine,
            node: dir.clone(),
            token_cost: own_d,
            ceiling: green_share, // the lean-hub budget at this subtree size
            overage: own_d.saturating_sub(green_share),
            worst_path: None,
            marginal_contributor: None,
            directive: Some(Directive::HoardingHub { percent, groups }),
            band,
        });
    }
}

//   `target/`) ONLY when it is an immediate child of `root` —
//   spec-instantiated artifacts (`codegen/`) and build output
//   (`.mir`/`.o` under `target/edda/<triple>/`, the synthesised
//   `target/edda/properties/properties.ea` PBT harness)
//   are compiler output, not the package's structure to fix; a source
//   dir that happens to be NAMED `codegen` or `target` deeper in the
//   tree is still gated (both root namespaces are reserved per
//   build-system.md §4 / CLAUDE.md "Build, run, runtime", so a
//   top-level collision cannot be hand-written source)
/// Whether `dir` lies inside one of the package's repo-tier
/// generated-output roots (`codegen/`, `target/`).
fn is_generated_dir(dir: &Path, root: &Path) -> bool {
    match dir.strip_prefix(root) {
        Ok(rel) => matches!(
            rel.components().next(),
            Some(std::path::Component::Normal(name))
                if name == "codegen" || name == "target"
        ),
        Err(_) => false,
    }
}

/// One directory's own (partitionable) interface token cost (consuming-model tokens).
fn own_interface(entry: &DirEntry, tokenizer: &Tokenizer, cfg: &BudgetConfig) -> usize {
    let exempt = exempt_file_set(entry);
    let mut files: BTreeSet<&str> = BTreeSet::new();
    for f in &entry.functions {
        if !exempt.contains(&f.file) {
            files.insert(f.file.as_str());
        }
    }
    for t in &entry.types {
        if !exempt.contains(&t.file) {
            files.insert(t.file.as_str());
        }
    }
    files
        .into_iter()
        .map(|file| {
            let rows = crate::emit::render_file_rows(entry, file);
            (tokenizer.count(&rows) as f64 * cfg.model_calibration).round() as usize
        })
        .sum()
}

/// Fill `memo` with `subtree[d]` = `own[d] + Σ subtree[child]`.
fn subtree_interface<'a>(
    d: &'a Path,
    own: &BTreeMap<&'a Path, usize>,
    child_dirs: &BTreeMap<&'a Path, Vec<&'a Path>>,
    memo: &mut BTreeMap<&'a Path, usize>,
    depth: usize,
) -> usize {
    if let Some(v) = memo.get(d) {
        return *v;
    }
    let mut total = own.get(d).copied().unwrap_or(0);
    if depth < 128 {
        if let Some(kids) = child_dirs.get(d) {
            for &c in kids {
                total = total.saturating_add(subtree_interface(c, own, child_dirs, memo, depth + 1));
            }
        }
    }
    memo.insert(d, total);
    total
}

/// Gate B Law 1 band for a hub's own/subtree interface ratio.
fn hub_band(own: usize, subtree: usize) -> Band {
    if own.saturating_mul(HUB_LEAN_RED_DEN) > subtree.saturating_mul(HUB_LEAN_RED_NUM) {
        Band::Red
    } else if own.saturating_mul(HUB_LEAN_GREEN_DEN) > subtree.saturating_mul(HUB_LEAN_GREEN_NUM) {
        Band::Amber
    } else {
        Band::Green
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> BudgetConfig {
        BudgetConfig::locked_defaults()
    }

    #[test]
    fn node_band_boundaries() {
        let c = cfg();
        // Green ceiling now meets the red threshold (both 6000), so the
        // amber band is empty: at or below the ceiling is green, one over
        // is red. Thinness is never a violation — small-but-complete nodes
        // are healthy (the under-provisioned band was removed).
        assert_eq!(node_band(3_000, &c), Band::Green);
        assert_eq!(node_band(6_000, &c), Band::Green);
        assert_eq!(node_band(6_001, &c), Band::Red);
        assert_eq!(node_band(50, &c), Band::Green);
    }

    #[test]
    fn hub_band_boundaries() {
        // own/subtree: ≤1/3 green, (1/3,1/2] amber, >1/2 red.
        assert_eq!(hub_band(33, 100), Band::Green); // 33% ≤ 1/3
        assert_eq!(hub_band(34, 100), Band::Amber); // 34% > 1/3
        assert_eq!(hub_band(50, 100), Band::Amber); // exactly 1/2 is not > 1/2
        assert_eq!(hub_band(51, 100), Band::Red); // majority → reservoir
        // Scale-free: the same ratios hold at 1000× the absolute size.
        assert_eq!(hub_band(33_000, 100_000), Band::Green);
        assert_eq!(hub_band(60_000, 100_000), Band::Red);
    }

    /// Build a `(Tree, files)` pair from `(dir_rel, fn_count)` specs — all
    /// of a dir's functions go in one `mod.ea` so `own_interface` scales
    /// with `fn_count`. The functions form TWO interleaved call-chains (each
    /// `f{i}` calls `f{i+2}`), so for `fn_count >= 10` the file carries a
    /// valid partition seam (two WCCs ≥5, coverage ≥10) and is therefore
    /// NON-exempt — `own_interface` counts it (`own` excludes
    /// only irreducible exempt-atomic files). A dir with `fn_count == 0`
    /// carries no files (no `ModuleEntry`) — a pure router, the Law 2
    /// empty-wrapper candidate.
    fn tree_with(root: &Path, dirs: &[(&str, usize)]) -> (crate::model::Tree, BTreeMap<PathBuf, String>) {
        let mut tree = crate::model::Tree { dirs: BTreeMap::new() };
        let mut files = BTreeMap::new();
        for (rel, n) in dirs {
            let dir = if rel.is_empty() { root.to_path_buf() } else { root.join(rel) };
            let mut entry = crate::model::DirEntry::default();
            if *n > 0 {
                entry.modules.push(crate::model::ModuleEntry {
                    name: "mod".to_string(),
                    file: "mod.ea".to_string(),
                    line: 1,
                    visibility: crate::model::Visibility::Public,
                });
            }
            for i in 0..*n {
                let next = i + 2;
                let calls: Vec<String> = if next < *n {
                    vec![format!("mod.f{next}")]
                } else {
                    Vec::new()
                };
                let refs: Vec<&str> = calls.iter().map(String::as_str).collect();
                entry.functions.push(fe(&format!("mod.f{i}"), "mod.ea", &refs));
            }
            tree.dirs.insert(dir.clone(), entry);
            files.insert(dir.join(INDEX_FILENAME), format!("loc: {rel}\n"));
        }
        (tree, files)
    }

    fn spine_entry_at<'a>(report: &'a BudgetReport, rel: &str, root: &Path) -> Option<&'a WorklistEntry> {
        let dir = if rel.is_empty() { root.to_path_buf() } else { root.join(rel) };
        report
            .worklist
            .iter()
            .find(|e| e.gate == Gate::Spine && e.node == dir)
    }

    /// Law 1: a hub holding the majority of its own subtree is a reservoir
    /// → red `HoardingHub`; the same large subtree distributed into the
    /// children (thin hub) is a lean conduit → no Gate B entry.
    #[test]
    fn law1_flags_hoarding_hub_passes_lean_conduit() {
        let root = Path::new("/pkg");
        // Hoarding: src/core holds 250 fns (subtree clears the read-cap
        // floor), two children 10 each.
        let (tree, files) = tree_with(
            root,
            &[("", 0), ("src", 0), ("src/core", 250), ("src/core/a", 10), ("src/core/b", 10)],
        );
        let report = analyze_budget(&files, root, &cfg(), Some(&tree));
        let e = spine_entry_at(&report, "src/core", root).expect("core must be flagged");
        assert_eq!(e.band, Band::Red);
        assert!(matches!(e.directive, Some(Directive::HoardingHub { .. })));
        // Lean: src/core holds 10, children hold 130 each — same big
        // subtree, distributed → conduit → no Gate B entry at core.
        let (tree2, files2) = tree_with(
            root,
            &[("", 0), ("src", 0), ("src/core", 10), ("src/core/a", 130), ("src/core/b", 130)],
        );
        let report2 = analyze_budget(&files2, root, &cfg(), Some(&tree2));
        assert!(spine_entry_at(&report2, "src/core", root).is_none(), "lean conduit must pass");
    }

    /// Law 2 (earn-your-place): only an EMPTY WRAPPER (0 own files + 1
    /// child) is a vine. A single-child directory that CARRIES even one
    /// file earns its place and is spared; the package root is exempt.
    #[test]
    fn law2_flags_empty_wrapper_only() {
        let root = Path::new("/pkg");
        let (tree, files) = tree_with(
            root,
            &[
                ("", 1),                  // root: carries + (2 children below)
                ("wrap", 0), ("wrap/real", 5), // wrap: 0 files + 1 child → EMPTY WRAPPER
                ("carry", 2), ("carry/sub", 3), // carry: 2 files + 1 child → earns its place
            ],
        );
        let report = analyze_budget(&files, root, &cfg(), Some(&tree));
        // Empty wrapper → vine.
        assert_eq!(
            spine_entry_at(&report, "wrap", root).and_then(|e| e.directive.clone()),
            Some(Directive::Flatten),
            "an empty wrapper (0 files, 1 child) is a vine",
        );
        // Carrying single-child dir is NOT a vine — it earns its place.
        assert!(
            spine_entry_at(&report, "carry", root).is_none(),
            "a single-child directory that holds a file earns its place",
        );
        // Root is exempt.
        assert!(spine_entry_at(&report, "", root).is_none(), "root is exempt");
    }

    /// A small whole-readable subsystem is a seed: below the read cap there
    /// is nothing to distribute, so Law 1 never fires regardless of shape.
    #[test]
    fn law1_exempts_subtree_under_read_cap() {
        let root = Path::new("/pkg");
        // core holds the majority but the whole subtree is tiny.
        let (tree, files) = tree_with(root, &[("", 0), ("core", 3), ("core/a", 2), ("core/b", 2)]);
        let report = analyze_budget(&files, root, &cfg(), Some(&tree));
        assert!(spine_entry_at(&report, "core", root).is_none(), "seed subsystem is exempt");
    }

    /// Repo-tier `codegen/` output is the compiler's structure, not the
    /// consumer's — its dirs are measured but never gated. A
    /// deeper source dir merely NAMED `codegen` is still gated.
    #[test]
    fn generated_codegen_subtree_is_never_gated() {
        let root = Path::new("/pkg");
        assert!(is_generated_dir(&root.join("codegen"), root));
        assert!(is_generated_dir(&root.join("codegen/p"), root));
        assert!(!is_generated_dir(root, root));
        assert!(!is_generated_dir(&root.join("src/codegen"), root));
        assert!(!is_generated_dir(Path::new("/elsewhere/codegen"), root));
        // codegen/p is a textbook empty wrapper (0 files + 1 child) — it
        // must NOT surface a Law 2 entry; the structurally identical
        // wrapper under src/ must.
        let (tree, files) = tree_with(
            root,
            &[
                ("", 1),
                ("codegen", 0), ("codegen/p", 0), ("codegen/p/inst", 5),
                ("src", 0), ("src/real", 5),
            ],
        );
        let report = analyze_budget(&files, root, &cfg(), Some(&tree));
        assert!(
            spine_entry_at(&report, "codegen/p", root).is_none(),
            "generated wrapper must not be gated",
        );
        assert_eq!(
            spine_entry_at(&report, "src", root).and_then(|e| e.directive.clone()),
            Some(Directive::Flatten),
            "the same shape under src/ is still a vine",
        );
    }

    /// Repo-tier `target/` build output (`.mir`/`.o` under
    /// `target/edda/<triple>/`, the `target/edda/properties/`
    /// PBT-harness tree `edda test --properties` synthesises) is the
    /// compiler's structure, not the consumer's — never gated
    /// (same class as the `codegen/` exemption). A
    /// deeper source dir merely NAMED `target` is still gated.
    #[test]
    fn generated_target_subtree_is_never_gated() {
        let root = Path::new("/pkg");
        assert!(is_generated_dir(&root.join("target"), root));
        assert!(is_generated_dir(&root.join("target/edda"), root));
        assert!(is_generated_dir(&root.join("target/edda/properties"), root));
        assert!(!is_generated_dir(&root.join("src/target"), root));
        assert!(!is_generated_dir(Path::new("/elsewhere/target"), root));
        // target/edda/properties is a textbook empty-wrapper chain (0
        // files + 1 child, twice over) — it must NOT surface a Law 2
        // entry; the structurally identical wrapper under src/ must.
        let (tree, files) = tree_with(
            root,
            &[
                ("", 1),
                ("target", 0), ("target/edda", 0), ("target/edda/properties", 1),
                ("src", 0), ("src/real", 5),
            ],
        );
        let report = analyze_budget(&files, root, &cfg(), Some(&tree));
        assert!(
            spine_entry_at(&report, "target/edda", root).is_none(),
            "generated wrapper must not be gated",
        );
        assert_eq!(
            spine_entry_at(&report, "src", root).and_then(|e| e.directive.clone()),
            Some(Directive::Flatten),
            "the same shape under src/ is still a vine",
        );
    }

    #[test]
    fn parse_export_count_sums_types_and_functions_only() {
        let text = "children[3]{path}:\nmodules[2]{x}:\ntypes[5]{a}:\nfunctions[9]{b}:\ninvariants[7]{c}:\n";
        assert_eq!(parse_export_count(text), 14);
    }

    /// Build a files map from (relative-dir, token-shaped-text) pairs.
    /// The text is padded so the fallback tokenizer yields a known count;
    /// with BPE on the absolute numbers differ but the structural
    /// assertions (monotonicity, which gate fires) hold regardless.
    fn files_from(root: &Path, specs: &[(&str, usize)]) -> BTreeMap<PathBuf, String> {
        let mut m = BTreeMap::new();
        for (rel, exports) in specs {
            let dir = if rel.is_empty() { root.to_path_buf() } else { root.join(rel) };
            let text = format!("types[{}]{{a}}:\nfunctions[0]{{b}}:\n", exports);
            m.insert(dir.join(INDEX_FILENAME), text);
        }
        m
    }

    #[test]
    fn spine_reported_at_leaf_not_internal_node() {
        let root = Path::new("/pkg");
        let files = files_from(&root, &[("", 1), ("a", 1), ("a/b", 1)]);
        let report = analyze_budget(&files, root, &cfg(), None);
        // No node is over budget here, so worklist is empty — the point
        // is the measures cover every dir and the spine math runs without
        // double-counting internal nodes.
        assert_eq!(report.measures.len(), 3);
    }

    fn fe(qname: &str, file: &str, calls: &[&str]) -> crate::model::FunctionEntry {
        crate::model::FunctionEntry {
            qualified_name: qname.to_string(),
            file: file.to_string(),
            line: 1,
            end: 1,
            visibility: crate::model::Visibility::Module,
            stability: crate::model::StabilityMarker::Absent,
            sig: "() -> i32".to_string(),
            calls: calls.iter().map(|s| s.to_string()).collect(),
            effect_cone: vec!["allocator".to_string(), "err: IoError".to_string()],
            declared_effects: vec!["allocator".to_string(), "err: IoError".to_string()],
        }
    }

    fn tok() -> Tokenizer {
        Tokenizer::new(TokenEncoding::O200kBase, 3.5)
    }

    /// Push a file with two disjoint 5-chains — a valid internal partition
    /// seam (`classify_call_graph -> Some`), so the file is NON-exempt. The
    /// `module` segment of each qualified name is the file stem, so chains
    /// in different files never cross-link.
    fn push_seamed_file(entry: &mut crate::model::DirEntry, file: &str) {
        let stem = file.strip_suffix(".ea").unwrap_or(file);
        for chain in ['a', 'b'] {
            for i in 0..5 {
                let q = format!("{stem}.{chain}{}", i + 1);
                let next = if i < 4 {
                    vec![format!("{stem}.{chain}{}", i + 2)]
                } else {
                    Vec::new()
                };
                let next_refs: Vec<&str> = next.iter().map(String::as_str).collect();
                entry.functions.push(fe(&q, file, &next_refs));
            }
        }
    }

    /// Push a mutual-recursion core file (dominant SCC, no valid cut) — an
    /// exempt-atomic file (`classify_call_graph -> None`).
    fn push_atomic_file(entry: &mut crate::model::DirEntry, file: &str) {
        let stem = file.strip_suffix(".ea").unwrap_or(file);
        let q = |n: &str| format!("{stem}.{n}");
        entry.functions.push(fe(&q("lower_block"), file, &[q("lower_stmt").as_str()]));
        entry.functions.push(fe(&q("lower_stmt"), file, &[q("lower_expr").as_str()]));
        entry.functions.push(fe(&q("lower_expr"), file, &[q("lower_arm").as_str(), q("lower_block").as_str()]));
        entry.functions.push(fe(&q("lower_arm"), file, &[q("lower_expr").as_str()]));
        entry.functions.push(fe(&q("lower_for"), file, &[q("lower_block").as_str()]));
        entry.functions.push(fe(&q("synth_a"), file, &[]));
        entry.functions.push(fe(&q("synth_b"), file, &[]));
        entry.functions.push(fe(&q("synth_c"), file, &[]));
    }

    #[test]
    fn classify_widesplit_emits_every_disjoint_group() {
        // fa.ea, fb.ea, fc.ea each carry an internal seam (non-exempt) and
        // have no cross-file call edges -> 3 components -> WideSplit emits
        // ALL THREE groups.
        let mut entry = crate::model::DirEntry::default();
        push_seamed_file(&mut entry, "fa.ea");
        push_seamed_file(&mut entry, "fb.ea");
        push_seamed_file(&mut entry, "fc.ea");
        match classify_red_node(&entry, 100_000, 6_000, &tok(), &cfg()) {
            Some(Directive::WideSplit { groups, fat_files }) => {
                assert_eq!(groups.len(), 3, "every disjoint group must be emitted");
                assert!(fat_files.is_empty(), "no file is fat at this ceiling");
            }
            other => panic!("expected WideSplit, got {other:?}"),
        }
    }

    #[test]
    fn classify_widesplit_flags_fat_file_partition() {
        // One separable file whose own interface exceeds a tiny ceiling ->
        // fat file with a partition seam, surfaced via WideSplit.
        let mut entry = crate::model::DirEntry::default();
        push_seamed_file(&mut entry, "x.ea");
        match classify_red_node(&entry, 100_000, 10, &tok(), &cfg()) {
            Some(Directive::WideSplit { fat_files, .. }) => {
                assert_eq!(fat_files.len(), 1);
                assert_eq!(fat_files[0].file, "x.ea");
                assert!(!fat_files[0].cluster.is_empty());
            }
            other => panic!("expected WideSplit with a fat file, got {other:?}"),
        }
    }

    #[test]
    fn classify_partition_when_separable_file() {
        // Single file, two disjoint 5-chains -> separable -> partition.
        let mut entry = crate::model::DirEntry::default();
        push_seamed_file(&mut entry, "x.ea");
        match classify_red_node(&entry, 100_000, 6_000, &tok(), &cfg()) {
            Some(Directive::Partition { file, cluster, .. }) => {
                assert_eq!(file, "x.ea");
                assert!(!cluster.is_empty());
            }
            other => panic!("expected Partition, got {other:?}"),
        }
    }

    #[test]
    fn exempt_set_separates_atomic_from_seamed() {
        // A mutual-recursion core file is exempt-atomic; a two-chain file is
        // not (it has a seam the gate can act on).
        let mut entry = crate::model::DirEntry::default();
        push_atomic_file(&mut entry, "body.ea");
        push_seamed_file(&mut entry, "x.ea");
        let exempt = exempt_file_set(&entry);
        assert!(exempt.contains("body.ea"), "mutual-recursion core is exempt");
        assert!(!exempt.contains("x.ea"), "a partitionable file is not exempt");
    }

    #[test]
    fn lone_atomic_file_is_not_flagged() {
        // A single cohesive non-partitionable file: subtracted from the
        // Gate-A projection (-> green) and below the ≥2 AtomicHoard trigger,
        // so nothing surfaces — the core guarantee.
        let root = Path::new("/pkg");
        let mut entry = crate::model::DirEntry::default();
        push_atomic_file(&mut entry, "body.ea");
        let mut tree = crate::model::Tree { dirs: BTreeMap::new() };
        tree.dirs.insert(root.to_path_buf(), entry);
        let mut files = BTreeMap::new();
        files.insert(root.join(INDEX_FILENAME), "loc: .\n".to_string());
        let mut c = cfg();
        c.node_green_max = 1;
        c.node_amber_max = 1;
        let report = analyze_budget(&files, root, &c, Some(&tree));
        assert!(report.worklist.is_empty(), "a lone atomic file must never be flagged");
    }

    #[test]
    fn atomic_hoard_flags_two_disjoint_cohesive_files() {
        // Two call-disjoint exempt-atomic files over the read cap -> Gate B
        // AtomicHoard naming both files + both prospective leaves.
        let root = Path::new("/pkg");
        let mut entry = crate::model::DirEntry::default();
        push_atomic_file(&mut entry, "alpha.ea");
        push_atomic_file(&mut entry, "beta.ea");
        let mut tree = crate::model::Tree { dirs: BTreeMap::new() };
        tree.dirs.insert(root.to_path_buf(), entry);
        let mut files = BTreeMap::new();
        files.insert(root.join(INDEX_FILENAME), "loc: .\n".to_string());
        let mut c = cfg();
        c.node_green_max = 1;
        c.node_amber_max = 1; // read cap = 1, well below the combined interface
        let report = analyze_budget(&files, root, &c, Some(&tree));
        let e = report
            .worklist
            .iter()
            .find(|e| matches!(e.directive, Some(Directive::AtomicHoard { .. })))
            .expect("two disjoint atomic files over cap must surface AtomicHoard");
        match &e.directive {
            Some(Directive::AtomicHoard { files, groups, .. }) => {
                assert_eq!(files.len(), 2);
                assert_eq!(groups.len(), 2, "each atomic file is its own prospective leaf");
            }
            other => panic!("expected AtomicHoard, got {other:?}"),
        }
    }

    #[test]
    fn model_calibration_scales_measured_cost() {
        // The substrate count is measured exactly; the calibration factor
        // scales it linearly, so doubling the factor doubles the reported
        // node cost (within rounding).
        let root = Path::new("/pkg");
        let files = files_from(&root, &[("", 8)]);
        let mut c1 = cfg();
        c1.model_calibration = 1.0;
        let mut c2 = cfg();
        c2.model_calibration = 2.0;
        let t1 = analyze_budget(&files, root, &c1, None).measures[0].token_cost;
        let t2 = analyze_budget(&files, root, &c2, None).measures[0].token_cost;
        assert!(t1 > 0);
        assert!(t2 >= 2 * t1 - 1 && t2 <= 2 * t1 + 1, "t1={t1} t2={t2}");
    }
}
