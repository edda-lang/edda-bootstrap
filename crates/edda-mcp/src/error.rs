//! Error model — locked class catalogue and the integer-code table.
//!
//! Per `language/06-tooling.md` §2.3 (`edda-codex`; superseded the
//! archived `mcp-protocol.md` §13 draft this module originally cited):
//!
//! - JSON-RPC standard codes (`-32700` parse error, `-32600` invalid
//!   request, `-32601` method not found, `-32602` invalid params,
//!   `-32603` internal error) are reserved as the spec defines them.
//! - JSON-RPC integer codes `1000`..=`1999` are reserved for Edda; the
//!   1001–1401 mapping is locked (§2.3's table). A class with no clear
//!   family in that table falls back to the implementation-defined
//!   `-32000` server-error slot rather than a misleading 1000-series
//!   guess.
//! - The `class` string is the canonical identifier; the integer is a
//!   compat shim for non-class-aware clients. Because [`ErrorClass`] is
//!   far finer-grained than the 15-entry locked table, many classes
//!   share one locked code (e.g. every typecheck-phase diagnostic class
//!   carries `1201 typecheck_failed`) — the `class` string carries the
//!   precise distinction.
//!
//! Every variant of [`ErrorClass`] has exactly one [`ErrorCode`] mapping
//! and exactly one `lowercase_snake_case` name. Changing either is a
//! wire break; the table is frozen.

use serde::{Deserialize, Serialize};

use crate::wire::ErrorObject;

/// Integer codes carried in [`ErrorObject::code`].
///
/// Values are wire-locked. Adding a code requires a spec edit.
#[derive(Copy, Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
#[serde(into = "i32", try_from = "i32")]
#[repr(i32)]
pub enum ErrorCode {
    /// JSON-RPC `-32700`. The server received text that is not valid JSON.
    ParseError = -32700,
    /// JSON-RPC `-32600`. The message is valid JSON but does not match
    /// the JSON-RPC envelope.
    InvalidRequest = -32600,
    /// JSON-RPC `-32601`. The named method does not exist.
    MethodNotFound = -32601,
    /// JSON-RPC `-32602`. The `params` shape did not match the
    /// operation's schema.
    InvalidParams = -32602,
    /// JSON-RPC `-32603`. The server hit an unrecoverable internal
    /// failure (panic, IO that should never have failed, etc.).
    InternalError = -32603,
    /// `1001`. A non-handshake/lifecycle operation violated the MCP
    /// session protocol (e.g. issued before handshake, or a duplicate
    /// `client.open_project`).
    ProtocolViolation = 1001,
    /// `1002`. The named method is not serviced by this daemon build —
    /// the locked catalogue admits the name, but no route implements it
    /// yet.
    UnknownMethod = 1002,
    /// `1003`. The `params` object did not match the operation's
    /// schema, or named an entity the operation cannot resolve (an
    /// ambiguous short hash, an unknown field, an opaque-repr layout
    /// query). Distinct from the JSON-RPC `-32602` slot, which is
    /// reserved for envelope-level parameter mismatches.
    EddaInvalidParams = 1003,
    /// `1100`. The session attempted an operation that requires an open
    /// project, but none is open.
    ProjectNotOpen = 1100,
    /// `1101`. The session attempted an operation on a document that is
    /// not open.
    DocumentNotOpen = 1101,
    /// `1102`. A `client.apply_change` (or equivalent) carried a
    /// document version that does not match the daemon's tracked
    /// version.
    DocumentVersionMismatch = 1102,
    /// `1200`. A build-phase operation (parse, import resolution,
    /// manifest, lint, cascade) reported a non-success outcome.
    BuildFailed = 1200,
    /// `1201`. A typecheck-phase operation (typecheck, effect-row,
    /// mode, stability, coherence) reported a non-success outcome.
    TypecheckFailed = 1201,
    /// `1202`. A refinement or termination obligation was not proven by
    /// the SMT discharge.
    RefinementUnprovenCode = 1202,
    /// `1300`. A structural edit was rejected.
    EditRejected = 1300,
    /// `1301`. A structural edit targeted a generated (spec-materialized)
    /// artifact, which is immutable.
    GeneratedArtifactImmutable = 1301,
    /// `1302`. A structural edit's target could not be resolved.
    EditTargetNotFound = 1302,
    /// `1303`. A structural edit would violate an invariant the daemon
    /// enforces (e.g. removing a still-referenced declaration).
    EditWouldViolateInvariant = 1303,
    /// `1400`. `inspect.synthesize` found no candidate satisfying the
    /// signature spec.
    SynthesisNoCandidates = 1400,
    /// `1401`. `inspect.synthesize` exceeded its search time budget.
    SynthesisTimeout = 1401,
    /// Edda implementation-defined fallback. Used when a class has no
    /// locked-table family — e.g. an internal daemon-init failure, or a
    /// cancelled-in-flight request outcome.
    ServerError = -32000,
}

impl From<ErrorCode> for i32 {
    fn from(c: ErrorCode) -> i32 {
        c as i32
    }
}

impl TryFrom<i32> for ErrorCode {
    type Error = String;
    fn try_from(value: i32) -> Result<Self, <Self as TryFrom<i32>>::Error> {
        match value {
            -32700 => Ok(ErrorCode::ParseError),
            -32600 => Ok(ErrorCode::InvalidRequest),
            -32601 => Ok(ErrorCode::MethodNotFound),
            -32602 => Ok(ErrorCode::InvalidParams),
            -32603 => Ok(ErrorCode::InternalError),
            1001 => Ok(ErrorCode::ProtocolViolation),
            1002 => Ok(ErrorCode::UnknownMethod),
            1003 => Ok(ErrorCode::EddaInvalidParams),
            1100 => Ok(ErrorCode::ProjectNotOpen),
            1101 => Ok(ErrorCode::DocumentNotOpen),
            1102 => Ok(ErrorCode::DocumentVersionMismatch),
            1200 => Ok(ErrorCode::BuildFailed),
            1201 => Ok(ErrorCode::TypecheckFailed),
            1202 => Ok(ErrorCode::RefinementUnprovenCode),
            1300 => Ok(ErrorCode::EditRejected),
            1301 => Ok(ErrorCode::GeneratedArtifactImmutable),
            1302 => Ok(ErrorCode::EditTargetNotFound),
            1303 => Ok(ErrorCode::EditWouldViolateInvariant),
            1400 => Ok(ErrorCode::SynthesisNoCandidates),
            1401 => Ok(ErrorCode::SynthesisTimeout),
            -32000 => Ok(ErrorCode::ServerError),
            other => Err(format!("unknown jsonrpc error code {other}")),
        }
    }
}

/// Canonical error-class catalogue.
///
/// The class is carried in [`ErrorObject::class`] as a
/// `lowercase_snake_case` string — the canonical identifier
/// class-aware clients dispatch on. The integer code in
/// [`ErrorObject::code`] is the compat shim: each class buckets onto
/// the locked code for its family per [`ErrorClass::code`].
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum ErrorClass {
    // ---- MCP protocol layer (language/06-tooling.md §2.3) ----
    /// A non-handshake operation was issued before the session
    /// completed `client.handshake`.
    HandshakeRequired,
    /// `client.handshake` was issued but the protocol-version
    /// intersection is empty.
    UnsupportedProtocolVersion,
    /// An operation required a feature neither side declared.
    UnsupportedProtocolFeature,
    /// The request was cancelled mid-flight via `client.cancel`.
    Cancelled,
    /// A short artifact name resolves to multiple artifacts in the
    /// active project.
    HashPrefixAmbiguous,
    /// `layout.offset_of` against a `@repr(opaque)` type.
    ReprOpaqueNoOffsets,
    /// A `layout.*` query named a field the type does not declare.
    FieldNotFound,
    /// The named method is not implemented in this daemon build. Wave
    /// 0.1 returns this for every operation the daemon does not yet
    /// service end-to-end.
    MethodNotImplemented,
    /// The `params` object did not match the operation's schema
    /// (unrecognised key, missing required field, wrong type).
    ArgShapeInvalid,
    /// The session attempted an operation that requires an open
    /// project, but `client.open_project` (or its equivalent) has not
    /// been called.
    NoProjectOpen,
    /// `client.open_project` was issued while a project is already
    /// open in this session.
    ProjectAlreadyOpen,
    /// The underlying [`edda_daemon`] driver could not initialise
    /// (missing manifest, missing src/, etc.).
    DriverInit,
    /// The cascade was attempted but reported a non-success exit code
    /// (build error or system error). The accompanying diagnostics
    /// carry the detail.
    CascadeFailed,
    // ---- build-system.md §8 — projected from `edda_diag::DiagnosticClass` ----
    /// Parser surface-syntax error.
    ParseErrorClass,
    /// Module import could not be resolved.
    ImportResolutionError,
    /// Module-import graph contains a cycle.
    ImportCycle,
    /// Typecheck pass rejected the program.
    TypecheckError,
    /// Refinement predicate was not proven by the SMT discharge.
    RefinementUnproven,
    /// Effect-row check found a missing or extra effect.
    EffectRowMismatch,
    /// Parameter-mode constraint violated.
    ModeViolation,
    /// Requested SIMD width is unsupported on the active target.
    SimdTargetUnsupported,
    /// A `target_has` query or manifest entry named an unknown feature.
    UnknownTargetFeature,
    /// A `@stable` item changed signature without a revision bump.
    StableContractRevision,
    /// Field access bypassed the type's declared alignment.
    UnalignedFieldAccess,
    /// A `comptime`-marked function lost purity.
    ComptimePurityLoss,
    /// Use of a `@deprecated` item.
    DeprecatedUse,
    /// Imported item was never used in the file.
    UnusedImport,
    /// `package.toml` contained an unrecognised key.
    UnknownManifestKey,
    /// Garbage-collection summary output.
    GcRecoverable,
    /// Graded effect bound exceeded — caller's bound is too small for
    /// the body's combined callee bounds, or the bound surface is
    /// malformed (mixing rule, unknown kind, wrong shape).
    EffectGradedBoundExceeded,
    /// Stable function calls a non-stable function.
    StabilityCallee,
    /// Stable function's effect row contains a disallowed entry.
    StabilityEffect,
    /// Stable function directly iterates a hashed container.
    StabilityHashIter,
    /// `@unverified` on a stable function.
    StabilityUnverified,
    /// `decreases` termination obligation could not be proven.
    TerminationUnproven,
    /// Recursion or unbounded loop without `decreases` and without
    /// admitting `divergence` in the row.
    DivergenceNotAdmitted,
    /// Coherence region's exit re-validation rejected a `mutable`
    /// parameter's refinement.
    CoherenceMutableRefinementInvalidated,
    /// `init` parameter was written inside a coherence region.
    CoherenceInitParamWritten,
    /// Directory's projected `index.toon` exceeds the LOC threshold.
    StructureMapTooDense,
    /// `.ea` filename in the directory clusters with another or
    /// contains an underscore in its stem.
    FilenameEncodesHierarchy,
    /// Private function parameter declared `mutable`/`take` but the body
    /// shows no write/move evidence.
    ModeOvergrab,
    /// Directory's `@unverified` + `@trust` ratio crosses the threshold.
    TrustHatchTooDense,
    /// `requires`/`ensures`/`where` clause whose predicate is `true`.
    RefinementTriviallyTrue,
    /// `var` binding never reassigned in its scope.
    BindingShouldBeLet,
    /// Same `import` path appears more than once within a file.
    DuplicateImport,
    /// Private function with no references anywhere in the package.
    DeadPrivateFunction,
    /// Closure literal's `captures { ... }` lists a name the body never
    /// references.
    UnusedClosureCapture,
    /// `scope(exec) <name>` block with no `<name>.spawn` inside.
    ExecScopeWithoutSpawn,
    /// `spec PATH(ARGS)` invocation appears more than once with
    /// identical arguments within the same scope.
    DuplicateSpecInvocation,
    /// Function signature names a capability the active build target
    /// does not provide (e.g. `Subprocess` on `wasm32-wasi-wasipreview1`).
    CapabilityNotAvailableOnTarget,
    /// `edda update` would widen the consumer's transitive effect-row
    /// union past the ceiling declared in the manifest's `max_effects`
    /// field. Per `08-packages.md` §6.3 / §9. The update is refused
    /// until the consumer either widens its `max_effects` declaration
    /// or pins the dependency at the previous version.
    CapabilityEscalation,
    /// The `package.lock.toml` file's `lockfile_hash` trailer mismatches
    /// the BLAKE3 recomputed over the file's `[[rune]]` entries'
    /// canonical encoding. Per `08-packages.md` §7.2.
    LockfileTampered,
    /// A `.ea` source file's intra-file call graph has multiple disjoint
    /// clusters and the file is ≥400 lines long.
    FileLowCohesion,
}

impl ErrorClass {
    /// `lowercase_snake_case` class name carried in [`ErrorObject::class`].
    pub const fn name(self) -> &'static str {
        match self {
            // language/06-tooling.md §2.3
            ErrorClass::HandshakeRequired => "handshake_required",
            ErrorClass::UnsupportedProtocolVersion => "unsupported_protocol_version",
            ErrorClass::UnsupportedProtocolFeature => "unsupported_protocol_feature",
            ErrorClass::Cancelled => "cancelled",
            ErrorClass::HashPrefixAmbiguous => "hash_prefix_ambiguous",
            ErrorClass::ReprOpaqueNoOffsets => "repr_opaque_no_offsets",
            ErrorClass::FieldNotFound => "field_not_found",
            ErrorClass::MethodNotImplemented => "method_not_implemented",
            ErrorClass::ArgShapeInvalid => "arg_shape_invalid",
            ErrorClass::NoProjectOpen => "no_project_open",
            ErrorClass::ProjectAlreadyOpen => "project_already_open",
            ErrorClass::DriverInit => "driver_init",
            ErrorClass::CascadeFailed => "cascade_failed",
            // build-system.md §8
            ErrorClass::ParseErrorClass => "parse_error",
            ErrorClass::ImportResolutionError => "import_resolution_error",
            ErrorClass::ImportCycle => "import_cycle",
            ErrorClass::TypecheckError => "typecheck_error",
            ErrorClass::RefinementUnproven => "refinement_unproven",
            ErrorClass::EffectRowMismatch => "effect_row_mismatch",
            ErrorClass::ModeViolation => "mode_violation",
            ErrorClass::SimdTargetUnsupported => "simd_target_unsupported",
            ErrorClass::UnknownTargetFeature => "unknown_target_feature",
            ErrorClass::StableContractRevision => "stable_contract_revision",
            ErrorClass::UnalignedFieldAccess => "unaligned_field_access",
            ErrorClass::ComptimePurityLoss => "comptime_purity_loss",
            ErrorClass::DeprecatedUse => "deprecated_use",
            ErrorClass::UnusedImport => "unused_import",
            ErrorClass::UnknownManifestKey => "unknown_manifest_key",
            ErrorClass::GcRecoverable => "gc_recoverable",
            ErrorClass::EffectGradedBoundExceeded => "effect_graded_bound_exceeded",
            ErrorClass::StabilityCallee => "stability_callee",
            ErrorClass::StabilityEffect => "stability_effect",
            ErrorClass::StabilityHashIter => "stability_hash_iter",
            ErrorClass::StabilityUnverified => "stability_unverified",
            ErrorClass::TerminationUnproven => "termination_unproven",
            ErrorClass::DivergenceNotAdmitted => "divergence_not_admitted",
            ErrorClass::CoherenceMutableRefinementInvalidated => {
                "coherence_mutable_refinement_invalidated"
            }
            ErrorClass::CoherenceInitParamWritten => "coherence_init_param_written",
            ErrorClass::StructureMapTooDense => "structure_map_too_dense",
            ErrorClass::FilenameEncodesHierarchy => "filename_encodes_hierarchy",
            ErrorClass::ModeOvergrab => "mode_overgrab",
            ErrorClass::TrustHatchTooDense => "trust_hatch_too_dense",
            ErrorClass::RefinementTriviallyTrue => "refinement_trivially_true",
            ErrorClass::BindingShouldBeLet => "binding_should_be_let",
            ErrorClass::DuplicateImport => "duplicate_import",
            ErrorClass::DeadPrivateFunction => "dead_private_function",
            ErrorClass::UnusedClosureCapture => "unused_closure_capture",
            ErrorClass::ExecScopeWithoutSpawn => "exec_scope_without_spawn",
            ErrorClass::DuplicateSpecInvocation => "duplicate_spec_invocation",
            ErrorClass::CapabilityNotAvailableOnTarget => "capability_not_available_on_target",
            ErrorClass::CapabilityEscalation => "capability_escalation",
            ErrorClass::LockfileTampered => "lockfile_tampered",
            ErrorClass::FileLowCohesion => "file_low_cohesion",
        }
    }

    /// Parse a class name from the wire form. Returns `None` outside
    /// the locked catalogue.
    pub fn from_name(s: &str) -> Option<Self> {
        // Linear scan — there are <50 classes; this is called once per
        // inbound error response decode, not a hot path.
        Self::ALL.iter().copied().find(|class| class.name() == s)
    }

    /// Integer code carried alongside the class.
    pub const fn code(self) -> ErrorCode {
        match self {
            // ---- MCP protocol layer -> 1001/1002/1003/1100 ----
            ErrorClass::HandshakeRequired => ErrorCode::ProtocolViolation,
            ErrorClass::UnsupportedProtocolVersion => ErrorCode::ProtocolViolation,
            ErrorClass::UnsupportedProtocolFeature => ErrorCode::ProtocolViolation,
            ErrorClass::ProjectAlreadyOpen => ErrorCode::ProtocolViolation,
            ErrorClass::MethodNotImplemented => ErrorCode::UnknownMethod,
            ErrorClass::ArgShapeInvalid => ErrorCode::EddaInvalidParams,
            ErrorClass::HashPrefixAmbiguous => ErrorCode::EddaInvalidParams,
            ErrorClass::ReprOpaqueNoOffsets => ErrorCode::EddaInvalidParams,
            ErrorClass::FieldNotFound => ErrorCode::EddaInvalidParams,
            ErrorClass::NoProjectOpen => ErrorCode::ProjectNotOpen,
            // `Cancelled` and `DriverInit` have no locked-table family —
            // a cancelled-in-flight outcome and an internal daemon-init
            // failure are not client mistakes, so they fall through to
            // the `_` arm below rather than a misleading 1000-series code.
            ErrorClass::CascadeFailed => ErrorCode::BuildFailed,

            // ---- build-system.md §8 diagnostics -> 1200/1201/1202 ----
            // Parse/import/manifest-time and structural-lint classes
            // bucket onto `build_failed`; typecheck-phase and
            // stability/coherence classes onto `typecheck_failed`;
            // refinement- and termination-obligation classes onto
            // `refinement_unproven`. The `class` string carries the
            // precise distinction the 15-entry locked table cannot.
            ErrorClass::ParseErrorClass => ErrorCode::BuildFailed,
            ErrorClass::ImportResolutionError => ErrorCode::BuildFailed,
            ErrorClass::ImportCycle => ErrorCode::BuildFailed,
            ErrorClass::TypecheckError => ErrorCode::TypecheckFailed,
            ErrorClass::RefinementUnproven => ErrorCode::RefinementUnprovenCode,
            ErrorClass::EffectRowMismatch => ErrorCode::TypecheckFailed,
            ErrorClass::ModeViolation => ErrorCode::TypecheckFailed,
            ErrorClass::SimdTargetUnsupported => ErrorCode::TypecheckFailed,
            ErrorClass::UnknownTargetFeature => ErrorCode::BuildFailed,
            ErrorClass::StableContractRevision => ErrorCode::TypecheckFailed,
            ErrorClass::UnalignedFieldAccess => ErrorCode::TypecheckFailed,
            ErrorClass::ComptimePurityLoss => ErrorCode::TypecheckFailed,
            ErrorClass::DeprecatedUse => ErrorCode::TypecheckFailed,
            ErrorClass::UnusedImport => ErrorCode::TypecheckFailed,
            ErrorClass::UnknownManifestKey => ErrorCode::BuildFailed,
            ErrorClass::GcRecoverable => ErrorCode::BuildFailed,
            ErrorClass::EffectGradedBoundExceeded => ErrorCode::TypecheckFailed,
            ErrorClass::StabilityCallee => ErrorCode::TypecheckFailed,
            ErrorClass::StabilityEffect => ErrorCode::TypecheckFailed,
            ErrorClass::StabilityHashIter => ErrorCode::TypecheckFailed,
            ErrorClass::StabilityUnverified => ErrorCode::TypecheckFailed,
            ErrorClass::TerminationUnproven => ErrorCode::RefinementUnprovenCode,
            ErrorClass::DivergenceNotAdmitted => ErrorCode::TypecheckFailed,
            ErrorClass::CoherenceMutableRefinementInvalidated => {
                ErrorCode::RefinementUnprovenCode
            }
            ErrorClass::CoherenceInitParamWritten => ErrorCode::TypecheckFailed,
            ErrorClass::StructureMapTooDense => ErrorCode::BuildFailed,
            ErrorClass::FilenameEncodesHierarchy => ErrorCode::BuildFailed,
            ErrorClass::ModeOvergrab => ErrorCode::TypecheckFailed,
            ErrorClass::TrustHatchTooDense => ErrorCode::BuildFailed,
            ErrorClass::RefinementTriviallyTrue => ErrorCode::RefinementUnprovenCode,
            ErrorClass::BindingShouldBeLet => ErrorCode::BuildFailed,
            ErrorClass::DuplicateImport => ErrorCode::BuildFailed,
            ErrorClass::DeadPrivateFunction => ErrorCode::BuildFailed,
            ErrorClass::UnusedClosureCapture => ErrorCode::TypecheckFailed,
            ErrorClass::ExecScopeWithoutSpawn => ErrorCode::TypecheckFailed,
            ErrorClass::DuplicateSpecInvocation => ErrorCode::BuildFailed,
            ErrorClass::CapabilityNotAvailableOnTarget => ErrorCode::TypecheckFailed,
            ErrorClass::CapabilityEscalation => ErrorCode::BuildFailed,
            ErrorClass::LockfileTampered => ErrorCode::BuildFailed,
            ErrorClass::FileLowCohesion => ErrorCode::BuildFailed,

            // `Cancelled` / `DriverInit`: no locked-table family.
            _ => ErrorCode::ServerError,
        }
    }

    /// Every locked class in declaration order. Useful for iterating
    /// the catalogue when reporting capability surfaces.
    pub const ALL: &'static [ErrorClass] = &[
        ErrorClass::HandshakeRequired,
        ErrorClass::UnsupportedProtocolVersion,
        ErrorClass::UnsupportedProtocolFeature,
        ErrorClass::Cancelled,
        ErrorClass::HashPrefixAmbiguous,
        ErrorClass::ReprOpaqueNoOffsets,
        ErrorClass::FieldNotFound,
        ErrorClass::MethodNotImplemented,
        ErrorClass::ArgShapeInvalid,
        ErrorClass::NoProjectOpen,
        ErrorClass::ProjectAlreadyOpen,
        ErrorClass::DriverInit,
        ErrorClass::CascadeFailed,
        ErrorClass::ParseErrorClass,
        ErrorClass::ImportResolutionError,
        ErrorClass::ImportCycle,
        ErrorClass::TypecheckError,
        ErrorClass::RefinementUnproven,
        ErrorClass::EffectRowMismatch,
        ErrorClass::ModeViolation,
        ErrorClass::SimdTargetUnsupported,
        ErrorClass::UnknownTargetFeature,
        ErrorClass::StableContractRevision,
        ErrorClass::UnalignedFieldAccess,
        ErrorClass::ComptimePurityLoss,
        ErrorClass::DeprecatedUse,
        ErrorClass::UnusedImport,
        ErrorClass::UnknownManifestKey,
        ErrorClass::GcRecoverable,
        ErrorClass::EffectGradedBoundExceeded,
        ErrorClass::StabilityCallee,
        ErrorClass::StabilityEffect,
        ErrorClass::StabilityHashIter,
        ErrorClass::StabilityUnverified,
        ErrorClass::TerminationUnproven,
        ErrorClass::DivergenceNotAdmitted,
        ErrorClass::CoherenceMutableRefinementInvalidated,
        ErrorClass::CoherenceInitParamWritten,
        ErrorClass::StructureMapTooDense,
        ErrorClass::FilenameEncodesHierarchy,
        ErrorClass::ModeOvergrab,
        ErrorClass::TrustHatchTooDense,
        ErrorClass::RefinementTriviallyTrue,
        ErrorClass::BindingShouldBeLet,
        ErrorClass::DuplicateImport,
        ErrorClass::DeadPrivateFunction,
        ErrorClass::UnusedClosureCapture,
        ErrorClass::ExecScopeWithoutSpawn,
        ErrorClass::DuplicateSpecInvocation,
        ErrorClass::CapabilityNotAvailableOnTarget,
        ErrorClass::CapabilityEscalation,
        ErrorClass::LockfileTampered,
        ErrorClass::FileLowCohesion,
    ];
}

impl std::fmt::Display for ErrorClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Structured server-side error.
///
/// Route handlers return `Result<T, McpError>`; the dispatcher
/// projects an `Err` onto a JSON-RPC error response via
/// [`McpError::into_error_object`].
#[derive(Clone, Debug)]
pub struct McpError {
    /// Canonical class. Drives both `code` and `class` on the wire.
    pub class: ErrorClass,
    /// Human-readable description. Never a contract.
    pub message: String,
    /// Optional structured locator. Free-form per class.
    pub target: Option<serde_json::Value>,
    /// Optional structured suggestions. Free-form per class.
    pub suggestions: Vec<serde_json::Value>,
}

impl McpError {
    /// Construct an error with no target or suggestions.
    pub fn new(class: ErrorClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
            target: None,
            suggestions: Vec::new(),
        }
    }

    /// Attach a structured target locator.
    pub fn with_target(mut self, target: serde_json::Value) -> Self {
        self.target = Some(target);
        self
    }

    /// Append a structured suggestion.
    pub fn with_suggestion(mut self, suggestion: serde_json::Value) -> Self {
        self.suggestions.push(suggestion);
        self
    }

    /// Project to the wire form. Used by the dispatcher.
    pub fn into_error_object(self) -> ErrorObject {
        ErrorObject {
            code: self.class.code().into(),
            message: self.message,
            class: Some(self.class.name().to_string()),
            target: self.target,
            suggestions: self.suggestions,
            streaming: None,
        }
    }
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.class, self.message)
    }
}

impl std::error::Error for McpError {}

/// Project an [`edda_diag::DiagnosticClass`] to its MCP wire class.
///
/// Used by the diagnostic serialiser when ferrying daemon-side
/// diagnostics out over MCP.
pub const fn from_diag_class(c: edda_diag::DiagnosticClass) -> ErrorClass {
    use edda_diag::DiagnosticClass as D;
    match c {
        D::ParseError => ErrorClass::ParseErrorClass,
        D::ImportResolutionError => ErrorClass::ImportResolutionError,
        D::ImportCycle => ErrorClass::ImportCycle,
        D::TypecheckError => ErrorClass::TypecheckError,
        D::RefinementUnproven => ErrorClass::RefinementUnproven,
        D::EffectRowMismatch => ErrorClass::EffectRowMismatch,
        D::ModeViolation => ErrorClass::ModeViolation,
        D::SimdTargetUnsupported => ErrorClass::SimdTargetUnsupported,
        D::UnknownTargetFeature => ErrorClass::UnknownTargetFeature,
        D::StableContractRevision => ErrorClass::StableContractRevision,
        D::UnalignedFieldAccess => ErrorClass::UnalignedFieldAccess,
        D::ComptimePurityLoss => ErrorClass::ComptimePurityLoss,
        D::DeprecatedUse => ErrorClass::DeprecatedUse,
        D::UnusedImport => ErrorClass::UnusedImport,
        D::UnknownManifestKey => ErrorClass::UnknownManifestKey,
        D::GcRecoverable => ErrorClass::GcRecoverable,
        D::EffectGradedBoundExceeded => ErrorClass::EffectGradedBoundExceeded,
        D::StabilityCallee => ErrorClass::StabilityCallee,
        D::StabilityEffect => ErrorClass::StabilityEffect,
        D::StabilityHashIter => ErrorClass::StabilityHashIter,
        D::StabilityUnverified => ErrorClass::StabilityUnverified,
        D::TerminationUnproven => ErrorClass::TerminationUnproven,
        D::DivergenceNotAdmitted => ErrorClass::DivergenceNotAdmitted,
        D::CoherenceMutableRefinementInvalidated => {
            ErrorClass::CoherenceMutableRefinementInvalidated
        }
        D::CoherenceInitParamWritten => ErrorClass::CoherenceInitParamWritten,
        D::StructureMapTooDense => ErrorClass::StructureMapTooDense,
        D::FilenameEncodesHierarchy => ErrorClass::FilenameEncodesHierarchy,
        D::ModeOvergrab => ErrorClass::ModeOvergrab,
        D::TrustHatchTooDense => ErrorClass::TrustHatchTooDense,
        D::RefinementTriviallyTrue => ErrorClass::RefinementTriviallyTrue,
        D::BindingShouldBeLet => ErrorClass::BindingShouldBeLet,
        D::DuplicateImport => ErrorClass::DuplicateImport,
        D::DeadPrivateFunction => ErrorClass::DeadPrivateFunction,
        D::UnusedClosureCapture => ErrorClass::UnusedClosureCapture,
        D::ExecScopeWithoutSpawn => ErrorClass::ExecScopeWithoutSpawn,
        D::DuplicateSpecInvocation => ErrorClass::DuplicateSpecInvocation,
        D::CapabilityNotAvailableOnTarget => ErrorClass::CapabilityNotAvailableOnTarget,
        D::CapabilityEscalation => ErrorClass::CapabilityEscalation,
        D::LockfileTampered => ErrorClass::LockfileTampered,
        D::FileLowCohesion => ErrorClass::FileLowCohesion,
        // D-18 / V1.0 no-comment design-lock sterility additions project onto existing wire
        // classes: the MCP `ErrorClass` wire enum + its integer-code table
        // are locked (language/06-tooling.md §2.3), so these new diagnostic
        // classes reuse the closest existing wire class rather than
        // extending the protocol surface. `unknown_attribute` is emitted
        // at the typecheck phase (attribute validation); `comment_not_admitted`
        // at lex/parse.
        D::UnknownAttribute => ErrorClass::TypecheckError,
        D::CommentNotAdmitted => ErrorClass::ParseErrorClass,
        // Emitted at the typecheck phase (exhaustiveness pass);
        // reuses the locked `typecheck_error` wire class per the same
        // precedent as `unknown_attribute`.
        D::NonExhaustiveMatch => ErrorClass::TypecheckError,
        // Emitted at the pre-link gate (a missing/unsatisfied
        // `__edda_*` runtime symbol is a soundness gap, not a protocol-layer
        // failure); reuses the locked `typecheck_error` wire class per the
        // same precedent as `unknown_attribute` / `non_exhaustive_match`.
        D::UnprovidedRuntimeExtern => ErrorClass::TypecheckError,
        // Emitted at the typecheck phase (the `scope(exec)`
        // mandatory-`Executor`-in-row check); reuses the locked
        // `typecheck_error` wire class per the same precedent as
        // `unknown_attribute` / `non_exhaustive_match`.
        D::ExecutorMissingInRow => ErrorClass::TypecheckError,
        // A parity gap: emitted at the same
        // pre-link gate as `unprovided_runtime_extern`; reuses the locked
        // `typecheck_error` wire class per the same precedent.
        D::DuplicateRuntimeExtern => ErrorClass::TypecheckError,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_class_round_trips_through_name() {
        for class in ErrorClass::ALL.iter().copied() {
            let name = class.name();
            assert!(
                name.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "class name {:?} is not lowercase_snake_case",
                name
            );
            assert_eq!(ErrorClass::from_name(name), Some(class));
        }
    }

    #[test]
    fn error_code_round_trip() {
        for code in [
            ErrorCode::ParseError,
            ErrorCode::InvalidRequest,
            ErrorCode::MethodNotFound,
            ErrorCode::InvalidParams,
            ErrorCode::InternalError,
            ErrorCode::ProtocolViolation,
            ErrorCode::UnknownMethod,
            ErrorCode::EddaInvalidParams,
            ErrorCode::ProjectNotOpen,
            ErrorCode::DocumentNotOpen,
            ErrorCode::DocumentVersionMismatch,
            ErrorCode::BuildFailed,
            ErrorCode::TypecheckFailed,
            ErrorCode::RefinementUnprovenCode,
            ErrorCode::EditRejected,
            ErrorCode::GeneratedArtifactImmutable,
            ErrorCode::EditTargetNotFound,
            ErrorCode::EditWouldViolateInvariant,
            ErrorCode::SynthesisNoCandidates,
            ErrorCode::SynthesisTimeout,
            ErrorCode::ServerError,
        ] {
            let n: i32 = code.into();
            assert_eq!(ErrorCode::try_from(n).unwrap(), code);
        }
        assert!(ErrorCode::try_from(7).is_err());
    }

    #[test]
    fn arg_shape_invalid_maps_to_edda_invalid_params() {
        assert_eq!(
            ErrorClass::ArgShapeInvalid.code(),
            ErrorCode::EddaInvalidParams
        );
    }

    #[test]
    fn method_not_implemented_maps_to_unknown_method() {
        assert_eq!(
            ErrorClass::MethodNotImplemented.code(),
            ErrorCode::UnknownMethod
        );
    }

    #[test]
    fn protocol_and_build_classes_map_to_locked_codes() {
        assert_eq!(
            ErrorClass::HandshakeRequired.code(),
            ErrorCode::ProtocolViolation
        );
        assert_eq!(ErrorClass::NoProjectOpen.code(), ErrorCode::ProjectNotOpen);
        assert_eq!(ErrorClass::CascadeFailed.code(), ErrorCode::BuildFailed);
        assert_eq!(ErrorClass::TypecheckError.code(), ErrorCode::TypecheckFailed);
        assert_eq!(
            ErrorClass::RefinementUnproven.code(),
            ErrorCode::RefinementUnprovenCode
        );
    }

    #[test]
    fn classes_with_no_locked_family_fall_back_to_server_error() {
        assert_eq!(ErrorClass::Cancelled.code(), ErrorCode::ServerError);
        assert_eq!(ErrorClass::DriverInit.code(), ErrorCode::ServerError);
    }
}
