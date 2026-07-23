//! Locked diagnostic class enum and the lowercase_snake_case name table.

use crate::severity::Severity;

/// Locked set of diagnostic classes (build-system.md §8).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
#[repr(u8)]
pub enum DiagnosticClass {
    /// Parser surface-syntax error.
    ParseError,
    /// Module import could not be resolved to a file or package.
    ImportResolutionError,
    /// Module-import graph contains a cycle.
    ImportCycle,
    /// Typecheck pass rejected the program.
    TypecheckError,
    /// Refinement predicate was not proven by the SMT discharge.
    RefinementUnproven,
    /// Effect-row check found a missing or extra effect.
    EffectRowMismatch,
    /// Parameter-mode (let/mutable/take/set) constraint violated.
    ModeViolation,
    /// Requested SIMD width is unsupported on the active target.
    SimdTargetUnsupported,
    /// A `target_has` query or manifest entry named an unknown feature.
    UnknownTargetFeature,
    /// A `@stable` item changed signature without a revision bump.
    StableContractRevision,
    /// Field access bypassed the type's declared alignment.
    UnalignedFieldAccess,
    /// A `comptime`-marked function lost purity in a refactor.
    ComptimePurityLoss,
    /// Use of a `@deprecated` item.
    DeprecatedUse,
    /// Imported item was never used in the file.
    UnusedImport,
    /// `package.toml` contained an unrecognised key.
    UnknownManifestKey,
    /// Garbage-collection summary output.
    GcRecoverable,
    /// Graded effect bound exceeded — caller's declared bound for a graded
    /// kind (`alloc`, `io`, `time`) is too small for the body's combined
    /// callee bounds. Per `02-modes-effects-refinements.md` §5.4 and §5.8.
    EffectGradedBoundExceeded,
    /// Stable function calls a non-stable function — violates the
    /// recursive callee-whitelist in `03-verification.md` §7 (Rule 2).
    StabilityCallee,
    /// Stable function's effect row contains a disallowed entry —
    /// violates the effect-row whitelist in `03-verification.md` §7
    /// (Rule 1). Permitted entries: `err: T`, `panic`, `yield: T`,
    /// `alloc`, `alloc(bytes <= N)`, `time(ops <= N)`.
    StabilityEffect,
    /// Stable function directly iterates a `HashMap`/`HashSet` —
    /// hash-iteration order varies across runs; use
    /// `iter_sorted_by_key` or `iter_in_insertion_order` instead. Per
    /// `03-verification.md` §7 (Rule 3).
    StabilityHashIter,
    /// `@unverified` on a stable function — stability is itself a
    /// verification claim, so the combination is rejected. Per
    /// `03-verification.md` §7 (*`@unverified` on a stable function is
    /// rejected*).
    StabilityUnverified,
    /// `decreases` termination obligation could not be proven —
    /// recursion or unbounded loop's measure does not strictly
    /// decrease. Per `03-verification.md` §5.
    TerminationUnproven,
    /// Recursive function or unbounded loop lacks a `decreases`
    /// measure but does not admit `divergence` in its effect row.
    /// Per `03-verification.md` §5 (positive divergence admission).
    DivergenceNotAdmitted,
    /// Coherence region committed a value violating the refinement
    /// of a `mutable` parameter — the region's body may have mutated
    /// the parameter in a way that breaks its `where` clause. Per
    /// `05-concurrency-coherence.md` §3.
    CoherenceMutableRefinementInvalidated,
    /// `init` parameter was written inside a `scope(coherence)`
    /// region — the Uninit→Valid transition would be exposed as
    /// observationally atomic, which contradicts the parameter's
    /// linear init discipline. Symmetric to the `mutable` refinement
    /// rule. Per `05-concurrency-coherence.md` §3.
    CoherenceInitParamWritten,
    /// Projected `index.toon` line count for a directory exceeds the
    /// configured warn threshold (default 250). Driver emits one per
    /// over-threshold directory during `check`, `parse-roundtrip`, and
    /// `build`. Per `06-tooling.md` §5.6.1.
    StructureMapTooDense,
    /// A `.ea` filename in the directory either shares a leading
    /// underscore-delimited token with another sibling, or contains an
    /// underscore in its stem at all. Per `06-tooling.md` §5.6.2.
    FilenameEncodesHierarchy,
    /// Private function declares a parameter with a stronger mode than the
    /// body uses — `mutable` without any write, or `take` without any
    /// move. Public functions are excluded (a stable API may legitimately
    /// reserve the mode for a future implementation); the discipline that
    /// modes describe actual use exists to keep effect rows tight, and
    /// this lint is the local enforcement on private helpers.
    ModeOvergrab,
    /// Per-directory ratio of `@unverified` + `@trust` items to total
    /// verifiable items exceeds the threshold configured in
    /// `[lints.trust_hatch_density]` in `package.toml`. The package's
    /// trust-hatch density is audit-relevant by construction.
    TrustHatchTooDense,
    /// A `requires`, `ensures`, or `where` refinement clause canonically
    /// reduces to `True` — the predicate carries zero verification
    /// information. Most common shape is `requires true` / `ensures true`
    /// left behind as scaffolding without the actual predicate filled in.
    RefinementTriviallyTrue,
    /// A `var` binding was declared but never reassigned within its
    /// scope. Should be `let` to match the actual usage discipline.
    BindingShouldBeLet,
    /// The same module path appears in more than one `import` statement
    /// within a single file. Pure noise; the second import has no effect.
    DuplicateImport,
    /// A private function has no references anywhere in the package —
    /// neither called nor passed as a function-pointer value. Dead code;
    /// remove the declaration.
    DeadPrivateFunction,
    /// A closure literal's `captures { ... }` list names bindings the
    /// body never references. The captures slot is part of the closure's
    /// content-addressed identity, so unused entries change the hash
    /// without changing behaviour.
    UnusedClosureCapture,
    /// A `scope(exec) <name>` block contains no `<name>.spawn` call. The
    /// structured-concurrency scope was declared but no task lives in it;
    /// the scope adds nothing observational.
    ExecScopeWithoutSpawn,
    /// A `spec PATH(ARGS)` invocation appears more than once with
    /// lexically identical arguments within the same scope (top-of-file,
    /// or inside the same parent `spec` body). Both invocations content-
    /// address to the same module; the duplicate is pure noise.
    DuplicateSpecInvocation,
    /// A function declares (in its parameter list, return type, or
    /// effect row) a capability type that the active build target does
    /// not support — e.g. `Subprocess` on `wasm32-wasi-wasipreview1`,
    /// or any I/O capability on a `baremetal` target. Per the codex
    /// cap-availability table (Model 1 from
    /// `corpus/edda-codex/language/effects.md` § per-target
    /// capabilities); the predicate
    /// `edda_target::TargetTriple::supports_capability` is the
    /// ground-truth lookup.
    CapabilityNotAvailableOnTarget,
    /// A dependency update's `effect_hash` change would push the
    /// consumer's transitive effect-row union past the ceiling declared
    /// in the manifest's `max_effects` field. Per
    /// `08-packages.md` §6.3 (max_effects / capability_escalation) and
    /// §9. The update is refused until the consumer either widens its
    /// `max_effects` declaration or pins the dependency at the previous
    /// version.
    CapabilityEscalation,
    /// The `package.lock.toml` file's `lockfile_hash` trailer
    /// mismatches the BLAKE3 recomputed over the file's `[[rune]]`
    /// entries' canonical encoding. Per `08-packages.md` §7.2. The
    /// build refuses to proceed with a tampered lockfile.
    LockfileTampered,
    /// A `.ea` source file's intra-file call graph has multiple
    /// disjoint clusters of functions (≥2 weakly-connected components
    /// of ≥3 functions each) AND the file is large enough (≥400 lines)
    /// that an agent cannot ingest the disjoint concerns in one read.
    /// The structural analog to `structure_map_too_dense` applied at
    /// file scope: raw LOC alone is a bad split metric; cohesion of
    /// the intra-file call graph is the right one.
    FileLowCohesion,
    /// An `@name` attribute outside the closed-nine whitelist
    /// (`@layout @align @repr @abi @unverified @trust @deprecated
    /// @property @target_requires`). Per D-18: the `@`-namespace is a
    /// closed set; invariants go in `where`/`requires`/`ensures`,
    /// patterns in a `spec`, and stability in the `stable`/`unstable`
    /// keywords. Matches the native compiler's `unknown_attribute` code.
    UnknownAttribute,
    /// Edda source contains a comment — a `//` line comment, a `/* */`
    /// block comment, or a legacy doc tier (`///`, `//!`, `/!!`, `!!!`).
    /// Per the V1.0 no-comment design lock, `.ea` source admits no comments: claims
    /// live in effect rows / refinements / attributes / the tracker, and
    /// item descriptions are derived into the structure map. Matches the
    /// native compiler's `comment_not_admitted` code.
    CommentNotAdmitted,
    /// A `match` over a sum type omits one or more variants and has no
    /// `case _` wildcard (or other irrefutable, unguarded arm). The
    /// uncovered variant can be reached at runtime, so the match is a
    /// soundness gap. Matches the native compiler's `non_exhaustive_match`
    /// code; the bootstrap previously had no exhaustiveness pass and
    /// accepted such matches as a false-negative.
    NonExhaustiveMatch,
    /// A `__edda_*` runtime-extern symbol is referenced by an emitted
    /// link input (object file or archive member) but is neither
    /// inlined away nor defined by any input in the same link — the
    /// pre-link gate that turns a cryptic `lld-link: undefined symbol`
    /// into an attributable compiler diagnostic. Matches the native
    /// compiler's `unprovided_runtime_extern` code.
    UnprovidedRuntimeExtern,
    /// A `scope(exec) <name> { ... }` block was opened, but the
    /// enclosing function's declared effect row carries no bare
    /// capability entry naming an `Executor`-typed parameter. Per
    /// `05-concurrency-coherence.md` §2.2 (*Mandatory `Executor`
    /// capability*): the `Executor` capability is what makes
    /// `scope(exec)` admissible, and a function without it in its row
    /// cannot open an `exec` scope.
    ExecutorMissingInRow,
    /// A `__edda_*` runtime-extern symbol is defined by more than one
    /// scanned link input (object file or archive member) -- the linker
    /// resolves the clash to one definition arbitrarily, so a stray
    /// duplicate silently shadows the intended one. Advisory, not fatal
    /// (the link proceeds): keep a single definition in `edda_rt.lib`.
    /// Matches the native compiler's `duplicate_runtime_extern` code
    /// (a parity gap in the same area).
    DuplicateRuntimeExtern,
}

/// Number of locked diagnostic classes. Bumps require a spec change.
pub const CLASS_COUNT: usize = 46;

impl DiagnosticClass {
    /// Every class in declaration order. Useful for iterating the `lints`
    /// block of `package.toml` or rendering a `--help` listing.
    pub const ALL: [DiagnosticClass; CLASS_COUNT] = [
        Self::ParseError,
        Self::ImportResolutionError,
        Self::ImportCycle,
        Self::TypecheckError,
        Self::RefinementUnproven,
        Self::EffectRowMismatch,
        Self::ModeViolation,
        Self::SimdTargetUnsupported,
        Self::UnknownTargetFeature,
        Self::StableContractRevision,
        Self::UnalignedFieldAccess,
        Self::ComptimePurityLoss,
        Self::DeprecatedUse,
        Self::UnusedImport,
        Self::UnknownManifestKey,
        Self::GcRecoverable,
        Self::EffectGradedBoundExceeded,
        Self::StabilityCallee,
        Self::StabilityEffect,
        Self::StabilityHashIter,
        Self::StabilityUnverified,
        Self::TerminationUnproven,
        Self::DivergenceNotAdmitted,
        Self::CoherenceMutableRefinementInvalidated,
        Self::CoherenceInitParamWritten,
        Self::StructureMapTooDense,
        Self::FilenameEncodesHierarchy,
        Self::ModeOvergrab,
        Self::TrustHatchTooDense,
        Self::RefinementTriviallyTrue,
        Self::BindingShouldBeLet,
        Self::DuplicateImport,
        Self::DeadPrivateFunction,
        Self::UnusedClosureCapture,
        Self::ExecScopeWithoutSpawn,
        Self::DuplicateSpecInvocation,
        Self::CapabilityNotAvailableOnTarget,
        Self::CapabilityEscalation,
        Self::LockfileTampered,
        Self::FileLowCohesion,
        Self::UnknownAttribute,
        Self::CommentNotAdmitted,
        Self::NonExhaustiveMatch,
        Self::UnprovidedRuntimeExtern,
        Self::ExecutorMissingInRow,
        Self::DuplicateRuntimeExtern,
    ];

    /// `lowercase_snake_case` name used in diagnostic output and the `lints`
    /// block of `package.toml`. Stable across compiler versions; spec-locked.
    pub const fn name(self) -> &'static str {
        match self {
            Self::ParseError => "parse_error",
            Self::ImportResolutionError => "import_resolution_error",
            Self::ImportCycle => "import_cycle",
            Self::TypecheckError => "typecheck_error",
            Self::RefinementUnproven => "refinement_unproven",
            Self::EffectRowMismatch => "effect_row_mismatch",
            Self::ModeViolation => "mode_violation",
            Self::SimdTargetUnsupported => "simd_target_unsupported",
            Self::UnknownTargetFeature => "unknown_target_feature",
            Self::StableContractRevision => "stable_contract_revision",
            Self::UnalignedFieldAccess => "unaligned_field_access",
            Self::ComptimePurityLoss => "comptime_purity_loss",
            Self::DeprecatedUse => "deprecated_use",
            Self::UnusedImport => "unused_import",
            Self::UnknownManifestKey => "unknown_manifest_key",
            Self::GcRecoverable => "gc_recoverable",
            Self::EffectGradedBoundExceeded => "effect_graded_bound_exceeded",
            Self::StabilityCallee => "stability_callee",
            Self::StabilityEffect => "stability_effect",
            Self::StabilityHashIter => "stability_hash_iter",
            Self::StabilityUnverified => "stability_unverified",
            Self::TerminationUnproven => "termination_unproven",
            Self::DivergenceNotAdmitted => "divergence_not_admitted",
            Self::CoherenceMutableRefinementInvalidated => {
                "coherence_mutable_refinement_invalidated"
            }
            Self::CoherenceInitParamWritten => "coherence_init_param_written",
            Self::StructureMapTooDense => "structure_map_too_dense",
            Self::FilenameEncodesHierarchy => "filename_encodes_hierarchy",
            Self::ModeOvergrab => "mode_overgrab",
            Self::TrustHatchTooDense => "trust_hatch_too_dense",
            Self::RefinementTriviallyTrue => "refinement_trivially_true",
            Self::BindingShouldBeLet => "binding_should_be_let",
            Self::DuplicateImport => "duplicate_import",
            Self::DeadPrivateFunction => "dead_private_function",
            Self::UnusedClosureCapture => "unused_closure_capture",
            Self::ExecScopeWithoutSpawn => "exec_scope_without_spawn",
            Self::DuplicateSpecInvocation => "duplicate_spec_invocation",
            Self::CapabilityNotAvailableOnTarget => "capability_not_available_on_target",
            Self::CapabilityEscalation => "capability_escalation",
            Self::LockfileTampered => "lockfile_tampered",
            Self::FileLowCohesion => "file_low_cohesion",
            Self::UnknownAttribute => "unknown_attribute",
            Self::CommentNotAdmitted => "comment_not_admitted",
            Self::NonExhaustiveMatch => "non_exhaustive_match",
            Self::UnprovidedRuntimeExtern => "unprovided_runtime_extern",
            Self::ExecutorMissingInRow => "executor_missing_in_row",
            Self::DuplicateRuntimeExtern => "duplicate_runtime_extern",
        }
    }

    /// Parse a class name from `lowercase_snake_case`. Returns `None` if the
    /// name is not in the locked set; the manifest parser maps that into the
    /// `unknown_manifest_key` warning.
    pub fn from_name(s: &str) -> Option<Self> {
        // Linear scan is fine — the locked set is small and this is called
        // once per `lints` entry, not in a hot loop.
        Self::ALL.iter().copied().find(|c| c.name() == s)
    }

    /// The class's locked default severity. The `lints` block and
    /// `--warn-as-error` CLI flag can override this default; see
    /// `LintConfig::effective`.
    pub const fn default_severity(self) -> Severity {
        match self {
            Self::ParseError
            | Self::ImportResolutionError
            | Self::ImportCycle
            | Self::TypecheckError
            | Self::RefinementUnproven
            | Self::EffectRowMismatch
            | Self::ModeViolation
            | Self::SimdTargetUnsupported
            | Self::UnknownTargetFeature
            | Self::EffectGradedBoundExceeded
            | Self::StabilityCallee
            | Self::StabilityEffect
            | Self::StabilityHashIter
            | Self::StabilityUnverified
            | Self::TerminationUnproven
            | Self::DivergenceNotAdmitted
            | Self::CoherenceMutableRefinementInvalidated
            | Self::CoherenceInitParamWritten
            | Self::CapabilityNotAvailableOnTarget
            | Self::CapabilityEscalation
            | Self::LockfileTampered
            | Self::StructureMapTooDense
            | Self::FilenameEncodesHierarchy
            | Self::FileLowCohesion
            | Self::UnusedImport
            | Self::UnknownManifestKey
            | Self::BindingShouldBeLet
            | Self::UnknownAttribute
            | Self::CommentNotAdmitted
            | Self::NonExhaustiveMatch
            | Self::UnprovidedRuntimeExtern
            | Self::ExecutorMissingInRow => Severity::Error,
            Self::StableContractRevision
            | Self::UnalignedFieldAccess
            | Self::ComptimePurityLoss
            | Self::DeprecatedUse
            | Self::ModeOvergrab
            | Self::TrustHatchTooDense
            | Self::RefinementTriviallyTrue
            | Self::DuplicateImport
            | Self::DeadPrivateFunction
            | Self::UnusedClosureCapture
            | Self::ExecScopeWithoutSpawn
            | Self::DuplicateSpecInvocation
            | Self::DuplicateRuntimeExtern => Severity::Warn,
            Self::GcRecoverable => Severity::Info,
        }
    }

    /// Discriminant as a `usize` for array indexing into `LintConfig`.
    #[inline]
    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

impl std::fmt::Display for DiagnosticClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_count_matches_all() {
        assert_eq!(DiagnosticClass::ALL.len(), CLASS_COUNT);
    }

    #[test]
    fn every_class_round_trips_through_name() {
        for class in DiagnosticClass::ALL {
            let name = class.name();
            assert!(
                name.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "class name {:?} is not lowercase_snake_case",
                name
            );
            assert_eq!(DiagnosticClass::from_name(name), Some(class));
        }
    }

    #[test]
    fn from_name_rejects_unknown_and_wrong_case() {
        assert_eq!(DiagnosticClass::from_name(""), None);
        assert_eq!(DiagnosticClass::from_name("PARSE_ERROR"), None);
        assert_eq!(DiagnosticClass::from_name("parse error"), None);
        assert_eq!(DiagnosticClass::from_name("not_a_class"), None);
    }

    #[test]
    fn default_severity_matches_spec_table() {
        // 06-tooling.md §6.2: 27 error / 12 warn /
        // 1 info across the original 40-variant set. Three structural-
        // projection lints plus three hygiene lints (unused_import,
        // binding_should_be_let, unknown_manifest_key)
        // default to `Error` because their resolution is mechanical
        // and warnings would predictably age into noise. The D-18 /
        // V1.0 no-comment design-lock sterility additions (unknown_attribute, comment_not_admitted)
        // bring the set to 29 error / 12 warn / 1 info; non_exhaustive_match
        // (a soundness error) brings the 43-variant set to 30 error / 12
        // warn / 1 info; unprovided_runtime_extern (a link-time soundness
        // gap) brings the 44-variant set to 31 error / 12 warn / 1 info;
        // executor_missing_in_row (mandatory-capability soundness gap for
        // `scope(exec)`) brings the 45-variant set to 32 error / 12 warn /
        // 1 info; the parity-gap duplicate_runtime_extern (advisory, not
        // fatal -- the link proceeds) brings the 46-variant set to 32
        // error / 13 warn / 1 info.
        let mut errors = 0;
        let mut warns = 0;
        let mut infos = 0;
        for class in DiagnosticClass::ALL {
            match class.default_severity() {
                Severity::Error => errors += 1,
                Severity::Warn => warns += 1,
                Severity::Info => infos += 1,
            }
        }
        assert_eq!((errors, warns, infos), (32, 13, 1));
        assert_eq!(
            DiagnosticClass::ParseError.default_severity(),
            Severity::Error
        );
        assert_eq!(
            DiagnosticClass::StableContractRevision.default_severity(),
            Severity::Warn
        );
        assert_eq!(
            DiagnosticClass::GcRecoverable.default_severity(),
            Severity::Info
        );
        for class in [
            DiagnosticClass::StructureMapTooDense,
            DiagnosticClass::FilenameEncodesHierarchy,
            DiagnosticClass::FileLowCohesion,
            DiagnosticClass::UnusedImport,
            DiagnosticClass::BindingShouldBeLet,
            DiagnosticClass::UnknownManifestKey,
        ] {
            assert_eq!(class.default_severity(), Severity::Error);
        }
    }

    #[test]
    fn display_renders_lowercase_snake_case() {
        assert_eq!(format!("{}", DiagnosticClass::ParseError), "parse_error");
        assert_eq!(
            format!("{}", DiagnosticClass::StableContractRevision),
            "stable_contract_revision"
        );
    }

    #[test]
    fn index_matches_position_in_all() {
        for (i, class) in DiagnosticClass::ALL.iter().enumerate() {
            assert_eq!(class.index(), i);
        }
    }
}
