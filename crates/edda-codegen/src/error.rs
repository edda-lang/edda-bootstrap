//! Error type and diagnostic mapping for the codegen pass.
//!
//! The staging-cascade pipeline is driven by this pass-level error
//! surface. Variants fall into two families:
//!
//! - [`CodegenError::Cache`] — wrapped failures bubbling out of
//!   `edda-cache` (filesystem IO, manifest parse, staging contract).
//!   Routed through [`edda_cache::CacheError::to_diagnostic`] for
//!   rendering.
//! - Codegen-specific contract violations
//!   ([`CodegenError::InvalidArtifactName`],
//!   [`CodegenError::DuplicateStaged`]) — signal misuse or a wire-
//!   format regression at this crate's surface.
//!
//! Per `docs/tooling/build-system.md` §8 the locked diagnostic class
//! set does not yet carry a `codegen_error` class; until the set is
//! reopened, this module falls back to `parse_error` for the codegen-
//! specific variants. `Cache` defers to `edda-cache`'s already-
//! established mapping. This matches the policy in
//! [`edda_cache::CacheError::to_diagnostic`].

use edda_cache::{ArtifactHash, CacheError};
use edda_diag::{Diagnostic, DiagnosticClass, Severity};
use edda_span::Span;
use smol_str::SmolStr;

/// Codegen-pass error.
#[derive(Debug)]
pub enum CodegenError {
    /// Wrapped cache failure (filesystem IO, manifest parse, staging
    /// contract). The inner error already carries its own path /
    /// message; the codegen layer just propagates.
    Cache(CacheError),

    /// The mangled short name was rejected by
    /// [`edda_cache::ArtifactName::new`]. Indicates either a contract
    /// violation upstream (resolver emitted a non-identifier qualified
    /// name) or a wire-format regression in [`crate::mangle`].
    InvalidArtifactName {
        /// Spec's resolved qualified name (`std.option.Option`).
        spec_qualified: SmolStr,
    },

    /// The same content hash was staged twice in one cascade. The
    /// cascade dedups by hash (content-addressed model); two artifacts
    /// with the same hash are the same artifact and the caller should
    /// supply one entry, not two.
    DuplicateStaged {
        /// The content hash already in this session's pending list.
        hash: ArtifactHash,
    },

    /// The artifact graph contains a cycle reachable from the cascade
    /// roots. The artifact graph is meant to be a DAG (no spec body
    /// transitively invokes itself through nested invocations); a
    /// cycle signals a corrupt manifest or a manifest-vs-source
    /// divergence the build cannot reason about. The remediation is
    /// `edda clean` followed by a from-source rebuild.
    CascadeCycle {
        /// Short names of the artifacts that participate in the cycle
        /// (the nodes left unresolved by Kahn's topological pass).
        involved: Vec<SmolStr>,
    },

    /// A spec invocation supplied an argument tuple whose length does
    /// not match the spec's declared comptime-parameter count. Caught
    /// at [`crate::SubstitutionMap::bind`] before any walk runs.
    MonomorphArityMismatch {
        /// Spec's resolved qualified name (`std.option.Option`).
        spec_qualified: SmolStr,
        /// Number of comptime parameters declared on the spec.
        expected: usize,
        /// Number of arguments supplied in the [`crate::ArgumentTuple`].
        found: usize,
    },

    /// A position-wise mismatch between a spec's declared comptime-
    /// parameter kind and the argument kind supplied. `Type`-kind
    /// generics must receive [`crate::Argument::Type`]; `Comptime`-kind
    /// generics must receive a value argument.
    MonomorphKindMismatch {
        /// Spec's resolved qualified name.
        spec_qualified: SmolStr,
        /// Source name of the generic parameter at `position`.
        generic_name: SmolStr,
        /// 0-based parameter position.
        position: usize,
        /// Generic parameter's declared kind (`"type"` or `"comptime"`).
        generic_kind: &'static str,
        /// On-disk kind tag of the argument that was supplied. See
        /// [`storage.md` §3 / [`crate::Argument::kind_tag`]] for the
        /// tag table.
        argument_kind_tag: u8,
    },

    /// The argument tuple supplies a kind ([`crate::Argument::EffectRow`]
    /// or [`crate::Argument::UserDefined`]) the substitution
    /// walker does not yet implement. Caught at bind time so the
    /// caller sees the limitation up front rather than mid-walk.
    MonomorphUnsupportedArgument {
        /// Spec's resolved qualified name.
        spec_qualified: SmolStr,
        /// Source name of the generic parameter at `position`.
        generic_name: SmolStr,
        /// 0-based parameter position.
        position: usize,
        /// On-disk kind tag of the unsupported argument.
        argument_kind_tag: u8,
    },
}

impl CodegenError {
    /// Project to an `edda-diag` `Diagnostic` for user-facing rendering.
    ///
    /// Carries [`Span::DUMMY`] — codegen errors are not source-bound
    /// at this layer. The driver may attach a spec-invocation span
    /// before pushing into a `Diagnostics` take.
    pub fn to_diagnostic(&self) -> Diagnostic {
        if let CodegenError::Cache(inner) = self {
            return inner.to_diagnostic();
        }
        Diagnostic::new(
            self.diagnostic_class(),
            Severity::Error,
            Span::DUMMY,
            self.to_string(),
        )
    }

    /// Class assignment. Same fallback policy as `edda-cache` until the
    /// locked class set in `build-system.md` §8 is reopened.
    fn diagnostic_class(&self) -> DiagnosticClass {
        match self {
            CodegenError::Cache(_) => DiagnosticClass::ParseError,
            CodegenError::InvalidArtifactName { .. } => DiagnosticClass::ParseError,
            CodegenError::DuplicateStaged { .. } => DiagnosticClass::ParseError,
            CodegenError::CascadeCycle { .. } => DiagnosticClass::ParseError,
            CodegenError::MonomorphArityMismatch { .. } => DiagnosticClass::ParseError,
            CodegenError::MonomorphKindMismatch { .. } => DiagnosticClass::ParseError,
            CodegenError::MonomorphUnsupportedArgument { .. } => DiagnosticClass::ParseError,
        }
    }
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodegenError::Cache(inner) => write!(f, "{}", inner),
            CodegenError::InvalidArtifactName { spec_qualified } => write!(
                f,
                "spec `{}` mangled to a name `ArtifactName::new` rejected",
                spec_qualified
            ),
            CodegenError::DuplicateStaged { hash } => {
                write!(f, "artifact `{}` staged twice in the same cascade", hash)
            }
            CodegenError::CascadeCycle { involved } => {
                write!(
                    f,
                    "artifact graph cycle reachable from cascade roots: {} node(s) participate",
                    involved.len()
                )
            }
            CodegenError::MonomorphArityMismatch {
                spec_qualified,
                expected,
                found,
            } => write!(
                f,
                "spec `{}` expects {} comptime argument(s), found {}",
                spec_qualified, expected, found
            ),
            CodegenError::MonomorphKindMismatch {
                spec_qualified,
                generic_name,
                position,
                generic_kind,
                argument_kind_tag,
            } => write!(
                f,
                "spec `{}` parameter `{}` at position {} is declared `{}`, but the argument has kind tag 0x{:02x}",
                spec_qualified, generic_name, position, generic_kind, argument_kind_tag
            ),
            CodegenError::MonomorphUnsupportedArgument {
                spec_qualified,
                generic_name,
                position,
                argument_kind_tag,
            } => write!(
                f,
                "spec `{}` parameter `{}` at position {} received argument kind tag 0x{:02x} which monomorphization does not yet support",
                spec_qualified, generic_name, position, argument_kind_tag
            ),
        }
    }
}

impl std::error::Error for CodegenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CodegenError::Cache(inner) => Some(inner),
            _ => None,
        }
    }
}

impl From<CacheError> for CodegenError {
    fn from(e: CacheError) -> Self {
        CodegenError::Cache(e)
    }
}
