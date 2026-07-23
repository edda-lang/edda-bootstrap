//! Internal model: per-directory [`DirEntry`] and the [`Tree`] index.
//!
//! Populated by [`crate::walk`] and consumed by [`crate::emit`].

use std::collections::BTreeMap;
use std::path::PathBuf;

/// `schema_version` field emitted in every `index.toon` header.
///
/// - `1` — initial codex v0.1 surface.
/// - `2` — adds `generics` column to types[]; `signature` carries the
///   rendered effect row instead of the `with { ... }` placeholder;
///   functions[] `calls` is populated from a body-walk over `Call` /
///   `MethodCall`; functions[] `effect_cone` is populated by an
///   intra-package fixed-point pass; doc lines are coalesced into at
///   most one entry per (target, tier).
/// - `3` — emitted filename settled on `index.toon` so Edda's
///   compiler-native structure map is visually distinct from the
///   language-agnostic files an external comment-scraper emits for
///   non-Edda code. Structure-density
///   guidance (codex §5.6) emits `structure_map_too_dense` and
///   `filename_encodes_hierarchy` diagnostics during cascade passes;
///   the toon-side schema is unchanged.
/// - `4` — retires the doc-comment surface (D-17).
///   The `doc_high[]` / `doc_med[]` / `doc_low[]` tables are removed
///   from every emitted file: per the locked V1.0 design rule, `.ea`
///   source admits no comments of any tier, so there are no doc lines
///   to derive and the tables would always be empty. Item descriptions
///   live in the checked facts (signature + effects + refinements +
///   `calls` + `effect_cone`), not in scraped prose. No other table changes.
/// - `5` — adds the `deferred[]` table (always emitted, possibly empty).
///   When the structure-budget gate classifies a file as an *atomic
///   module* (an inseparable mutual-recursion core that has no valid
///   call-graph cut), the emitter omits that file's `types[]`/
///   `functions[]` rows from the directory's `index.toon` and records a
///   one-line routing entry in `deferred[]` instead. The file's
///   interface is then loaded on demand only when an agent opens the
///   file — so it stops summing into the directory's node cost and into
///   every ancestor spine. This is the automatic accounting rule that
///   keeps a dense-cohesive core (e.g. `types/check/body.ea`) under
///   budget without splitting it.
/// - `6` — adaptive + deduped + walkable redesign. Five changes:
///   (1) **Adaptive sections** — a section table is emitted only when it
///   has ≥1 row; empty `children[0]`/`types[0]`/`functions[0]`/
///   `modules[0]`/`deferred[0]`/`invariants[0]`/`patterns[0]`/
///   `trust_points[0]` lines are never written. (2) **Slim header** —
///   the root file keeps `project`/`compiler_version`/`schema_version`;
///   every non-root file carries a single `loc: <path-relative-to-
///   package-root>` line and nothing else. `generated_at` is dropped
///   everywhere (it caused per-file churn and the change-detector
///   already ignored it); the constant `parent: ../index.toon` line is
///   replaced by `loc:`. (3) **Effect-row legend + cone dedup** — an
///   `effects[N]{id,row}:` table lists the distinct declared effect-rows
///   among this file's functions; `functions[]` references rows by `id`
///   instead of repeating the row text on every line. (4) **`functions[]`
///   recolumn** to `{name,file,line,visibility,stability,sig,eff,cone,
///   calls}`: `sig` is `(params) -> Ret` only (no leading
///   `public function <name>`, no trailing `with { … }`); `eff` is the
///   legend id of the declared row; `cone` is `=` when the effect cone
///   equals the declared row else the legend id of the cone row. The
///   `requires`/`ensures`/`decreases` columns are dropped — those clauses
///   are already emitted in `invariants[]` with line numbers. (5)
///   **Walkable `children[]`** recolumned to `{path,types,functions,
///   public}`: `types`/`functions` are the child's own counts and
///   `public` is a space-joined headline of the child's public item names
///   so a parent is a usable table of contents. The workspace aggregator
///   emits real rolled-up member totals + the same `public` headline.
/// - `7` — retires the information-destroying atomic-defer.
///   The `deferred[]` table is dropped
///   and no file's interface is ever collapsed to a count. A single
///   call-cohesive file with no internal call-graph seam is now
///   *exempt-atomic*: rendered in FULL (every signature, effect row, cone,
///   call list — all rules still apply) with its interface cost subtracted
///   from the directory's Gate-A token projection, so the read cap stops
///   counting the one thing it cannot act on. The consequence — a
///   directory hosting an exempt-atomic file has an `index.toon` that
///   exceeds the nominal read cap — is accepted by design (far cheaper than
///   collapsing the file's interface to a count). Multiple cohesive files
///   re-enter the budget via the new `AtomicHoard` gate (Gate B): ≥2
///   call-disjoint exempt-atomic files whose combined interface exceeds the
///   read cap are flagged for distribution into per-file leaves. No table
///   columns change; the only schema-visible change is the removal of
///   `deferred[]`.
/// - `8` — adds an `end` column to `types[]` and `functions[]` rows and
///   normalizes the `line` (start) column to include leading attributes.
///   Each row now projects the full
///   item AST span: `line` is the first source line of the item
///   *including* any `@name(...)` attribute lines that precede the
///   declaration keyword, and `end` is the item's closing line
///   (inclusive of the body's closing brace). `[line, end]` is therefore
///   a precise, self-contained read range for the item — an agent can
///   bound a targeted read without inferring the end from the next item's
///   start (which fails for the last item in a file and mis-bounds when
///   attributes or `requires`/`ensures`/`decreases` clauses sit between
///   signature and body). No other columns or tables change.
pub const SCHEMA_VERSION: u32 = 8;

/// Filename the compiler writes in every directory containing Edda
/// source. Distinct from the language-agnostic files an external
/// comment-scraper produces for non-Edda code — both conventions can
/// coexist in the same tree without ambiguity.
pub const INDEX_FILENAME: &str = "index.toon";

/// Per-package directory tree of structure maps.
pub(crate) struct Tree {
    pub(crate) dirs: BTreeMap<PathBuf, DirEntry>,
}

/// One emitted `index.toon`'s worth of items.
#[derive(Default)]
pub(crate) struct DirEntry {
    pub(crate) children: Vec<ChildIndex>,
    pub(crate) modules: Vec<ModuleEntry>,
    pub(crate) types: Vec<TypeEntry>,
    pub(crate) functions: Vec<FunctionEntry>,
    pub(crate) invariants: Vec<InvariantEntry>,
    pub(crate) patterns: Vec<PatternEntry>,
    pub(crate) trust_points: Vec<TrustEntry>,
}

// subtree rollup) for per-package emission; the workspace aggregator
// reuses this struct with rolled-up member totals. `public` is a
// space-joined headline of the child's public item names (public types
// then public functions, names only) capped at ~12 names + `…` — it
// turns a parent `index.toon` into a usable table of contents
/// One entry in `children[]` — immediate sub-directory with own item
/// counts and a public-surface headline.
pub(crate) struct ChildIndex {
    pub(crate) path: String,
    pub(crate) types: u32,
    pub(crate) functions: u32,
    pub(crate) public: String,
}

/// One entry in `modules[]` — one `.ea` file in this directory.
pub(crate) struct ModuleEntry {
    pub(crate) name: String,
    pub(crate) file: String,
    pub(crate) line: u32,
    pub(crate) visibility: Visibility,
}

/// One entry in `types[]`.
pub(crate) struct TypeEntry {
    pub(crate) name: String,
    pub(crate) kind: TypeKind,
    pub(crate) file: String,
    /// First source line of the item — schema v8 includes any leading
    /// `@name(...)` attribute lines (the start of the full item span),
    /// not just the declaration keyword.
    pub(crate) line: u32,
    /// Closing source line of the item, inclusive of the body's closing
    /// brace (schema v8). `[line, end]` is a precise read range.
    pub(crate) end: u32,
    pub(crate) visibility: Visibility,
    pub(crate) stability: StabilityMarker,
    /// Generic / comptime parameter names — populated for `kind = Spec`
    /// from `Spec::generics`; empty for `Struct` / `Enum` (type
    /// declarations don't expose generics on the structmap surface yet).
    pub(crate) generics: Vec<String>,
    /// Field/variant summary — names only, space-joined inside the
    /// column. Empty for `kind = Spec` (specs have no fields; their
    /// generic-param list lives in [`Self::generics`]).
    pub(crate) fields_or_variants: String,
    /// Attached refinement clauses, source order.
    pub(crate) refinements: Vec<String>,
}

/// One entry in `functions[]`.
#[derive(Clone)]
pub(crate) struct FunctionEntry {
    pub(crate) qualified_name: String,
    pub(crate) file: String,
    /// First source line of the item — schema v8 includes any leading
    /// `@name(...)` attribute lines (the start of the full item span),
    /// not just the `function` keyword.
    pub(crate) line: u32,
    /// Closing source line of the item, inclusive of the body's closing
    /// brace (schema v8). `[line, end]` is a precise read range.
    pub(crate) end: u32,
    pub(crate) visibility: Visibility,
    pub(crate) stability: StabilityMarker,
    /// Signature in `(params) -> Ret` form — the leading
    /// `public function <name>` and the trailing `with { … }` effect row
    /// are stripped (schema v6). The effect row lives in
    /// [`Self::declared_effects`] and surfaces via the `eff` legend id;
    /// `requires`/`ensures`/`decreases` clauses live in `invariants[]`.
    pub(crate) sig: String,
    /// Calls graph — qualified names of functions called by this body.
    /// Schema v2: populated by the body walker from `Call { callee:
    /// Path(...) }` and `MethodCall` sites. Bare-path callees are
    /// best-effort module-qualified against the active package; method
    /// receivers are recorded as `.method_name` (the receiver type is a
    /// typecheck concern, not visible to this pass).
    pub(crate) calls: Vec<String>,
    /// Effect cone — the transitive union of declared effects across
    /// every intra-package callee reachable from this function's body,
    /// rendered using the same source-form syntax as [`Self::signature`].
    /// Computed by [`crate::analyze::compute_effect_cones`] after the
    /// walk completes. Out-of-package callees (stdlib, externs) do not
    /// contribute additional row members because the structmap does not
    /// load their function tables; the cone is therefore a conservative
    /// under-approximation, not a complete propagation.
    pub(crate) effect_cone: Vec<String>,
    /// Declared effect-row members (the row written on this function's
    /// own signature). Populated alongside [`Self::signature`]; the
    /// renderer doesn't emit it as its own column — it is the seed for
    /// [`crate::analyze::compute_effect_cones`].
    pub(crate) declared_effects: Vec<String>,
}

/// One entry in `invariants[]` — one refinement clause attached to an item.
pub(crate) struct InvariantEntry {
    pub(crate) target: String,
    pub(crate) file: String,
    pub(crate) line: u32,
    pub(crate) rule: String,
}

/// One entry in `patterns[]` — a spec invocation; the pattern name is the
/// invocation's qualified path per codex §5.2.
pub(crate) struct PatternEntry {
    pub(crate) name: String,
    pub(crate) file: String,
    pub(crate) line: u32,
}

/// One entry in `trust_points[]` — `@unverified(...)` or `@trust(...)`.
pub(crate) struct TrustEntry {
    pub(crate) target: String,
    pub(crate) kind: TrustKind,
    pub(crate) file: String,
    pub(crate) line: u32,
    pub(crate) reason: String,
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub(crate) enum Visibility {
    Public,
    Module,
}

impl Visibility {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Visibility::Public => "public",
            Visibility::Module => "module",
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub(crate) enum StabilityMarker {
    Stable,
    Unstable,
    Absent,
}

impl StabilityMarker {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            StabilityMarker::Stable => "stable",
            StabilityMarker::Unstable => "unstable",
            StabilityMarker::Absent => "",
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub(crate) enum TypeKind {
    /// Record / product type.
    Struct,
    /// Sum / variant type.
    Enum,
    /// Spec declaration (codex's "spec" type kind).
    Spec,
}

impl TypeKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            TypeKind::Struct => "struct",
            TypeKind::Enum => "enum",
            TypeKind::Spec => "spec",
        }
    }
}

#[derive(Copy, Clone, Eq, PartialEq)]
pub(crate) enum TrustKind {
    Unverified,
    Trust,
}

impl TrustKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            TrustKind::Unverified => "unverified",
            TrustKind::Trust => "trust",
        }
    }
}
