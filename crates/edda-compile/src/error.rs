//! Compile-pass error type and diagnostic mapping.
//!
//! Every fallible call into this crate that surfaces to a user returns
//! `Result<T, CompileError>`. The renderer in `edda-driver` / `edda-cli`
//! consumes the [`Diagnostic`] produced by [`CompileError::to_diagnostic`].
//!
//! # Variant scope
//!
//! - [`CompileError::Io`] — filesystem IO during object-file emission.
//! - [`CompileError::SimdUnsupported`] — target-level SIMD validation.
//! - [`CompileError::UnsupportedTarget`] — defensive guard for direct
//!   crate callers that constructed a triple outside the v0.1 set.
//! - [`CompileError::LlvmInit`] — target initialisation
//!   / `TargetMachine` construction failures coming out of the inkwell
//!   binding. Only reachable when the `llvm` cargo feature is enabled,
//!   but the variant itself is always compiled so the diagnostic
//!   surface stays cargo-feature-stable.
//! - [`CompileError::ObjectEmit`] — `TargetMachine::write_to_file` /
//!   inkwell-binding failures during object-file emission. Produced
//!   by [`crate::Emitter::write_object`] and
//!   [`crate::Emitter::compile_program_to_object`]. The string `reason`
//!   carries LLVM's verbatim message; the path is the destination the
//!   caller asked us to write.
//!
//! Layout-attribute violations land alongside their producer once
//! that lowering is implemented.

use std::fmt;
use std::io;
use std::path::PathBuf;

use edda_diag::{Diagnostic, DiagnosticClass, Severity};
use edda_span::Span;
use edda_target::TargetTriple;

/// Compile-pass error.
#[derive(Debug)]
pub enum CompileError {
    /// Filesystem operation failed while emitting an object or staging an
    /// artifact. Mirrors `edda-cache`'s `Io` shape so the driver can render
    /// the two uniformly.
    Io {
        /// Path the operation was targeting.
        path: PathBuf,
        /// Short verb naming the failed operation: `"write"`,
        /// `"create_dir_all"`, `"rename"`, `"remove"`.
        op: &'static str,
        /// Underlying OS error.
        source: io::Error,
    },

    /// A SIMD operation was requested at a width or feature combination
    /// the active target does not support. Maps to
    /// `simd_target_unsupported` (build-system.md §8).
    SimdUnsupported {
        /// Active build target.
        triple: TargetTriple,
        /// SIMD vector width in bits the caller requested.
        width_bits: u32,
        /// Source span of the SIMD-using site, if known.
        span: Span,
        /// Why the request was rejected.
        reason: SimdRejection,
    },

    /// The (arch, os, abi) tuple does not name a supported v0.1 target.
    /// Defensive — the driver gates triple validity at the CLI and
    /// manifest boundary, so this is a "should never happen" guard for
    /// direct crate callers that constructed a triple by hand.
    UnsupportedTarget {
        /// Triple that fell through validation.
        triple: TargetTriple,
    },

    /// LLVM target-machine initialisation failed. Raised by the inkwell
    /// emitter when `Target::from_triple` returns an error, when
    /// `create_target_machine` returns `None`, or when the per-arch
    /// `Target::initialize_*` family rejects the requested arch.
    ///
    /// Only producible behind the `llvm` cargo feature. The variant is
    /// always compiled so callers can match exhaustively regardless of
    /// feature state.
    LlvmInit {
        /// Triple the emitter was trying to bind.
        triple: TargetTriple,
        /// Stage that failed: `"from_triple"`,
        /// `"create_target_machine"`, `"initialize_target"`, ...
        stage: &'static str,
        /// Human-readable detail surfaced from LLVM (verbatim) or from
        /// the inkwell binding.
        reason: String,
    },

    /// The MIR walker encountered a shape it does not yet handle.
    /// Only primitive-typed function signatures are currently
    /// supported; compound types ([`edda_mir::MirTypeKind::Adt`],
    /// `Tuple`, `Slice`, `FnPtr`) and the `Str` slice primitive
    /// produce this variant until their lowering is implemented.
    UnsupportedMirShape {
        /// Short label naming the unsupported shape:
        /// `"adt-return"`, `"tuple-param"`, `"slice-param"`,
        /// `"fnptr-param"`, `"str-as-scalar"`, ...
        shape: &'static str,
        /// Human-readable detail. May include the body name, param
        /// index, or other locating information.
        detail: String,
    },

    /// LLVM `TargetMachine::write_to_file` (or an inkwell-binding
    /// failure during object emission) rejected the write. Distinct
    /// from [`CompileError::Io`] because the failure surface is LLVM's
    /// string message, not an [`io::Error`] — LLVM has already
    /// translated the underlying OS condition into its own diagnostic
    /// by the time inkwell hands us the error.
    ///
    /// Only producible behind the `llvm` cargo feature; the variant is
    /// always compiled so callers can match exhaustively regardless of
    /// feature state.
    ObjectEmit {
        /// Path the emitter was trying to write.
        path: PathBuf,
        /// LLVM's verbatim message (e.g. `"could not open file"`,
        /// `"target does not support generation of this file type"`).
        reason: String,
    },
}

/// Why a SIMD-width request was rejected.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum SimdRejection {
    /// The width has no representable v0.1 SIMD ISA on the active arch
    /// (e.g., 256-bit on aarch64, any SIMD on riscv64 or wasm64).
    UnsupportedWidth,
    /// The width maps to a known feature, but it is not enabled in the
    /// active target's feature set. The caller may suggest enabling the
    /// feature in `package.toml`'s `default_features`.
    MissingFeature {
        /// Feature name (e.g., `"avx2"`, `"neon"`, `"simd128"`).
        feature: &'static str,
    },
}

impl CompileError {
    /// Project this error to an `edda-diag` [`Diagnostic`].
    ///
    /// The §8 diagnostic class set does not currently include a
    /// compile-pass-specific class, so [`CompileError::Io`] and
    /// [`CompileError::UnsupportedTarget`] fall back to
    /// [`DiagnosticClass::ParseError`] — matching `edda-cache`'s mapping
    /// (see its `error.rs`). A future §8 reopen can introduce a dedicated
    /// class without changing call sites.
    pub fn to_diagnostic(&self) -> Diagnostic {
        let class = self.diagnostic_class();
        let span = self.primary_span();
        Diagnostic::new(class, Severity::Error, span, self.to_string())
    }

    fn diagnostic_class(&self) -> DiagnosticClass {
        match self {
            CompileError::SimdUnsupported { .. } => DiagnosticClass::SimdTargetUnsupported,
            CompileError::Io { .. }
            | CompileError::UnsupportedTarget { .. }
            | CompileError::LlvmInit { .. }
            | CompileError::UnsupportedMirShape { .. }
            | CompileError::ObjectEmit { .. } => DiagnosticClass::ParseError,
        }
    }

    fn primary_span(&self) -> Span {
        match self {
            CompileError::SimdUnsupported { span, .. } => *span,
            _ => Span::DUMMY,
        }
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompileError::Io { op, path, source } => {
                write!(f, "{} {}: {}", op, path.display(), source)
            }
            CompileError::SimdUnsupported {
                triple,
                width_bits,
                reason,
                ..
            } => match reason {
                SimdRejection::UnsupportedWidth => write!(
                    f,
                    "SIMD width {width_bits} bits is not supported on target {triple}"
                ),
                SimdRejection::MissingFeature { feature } => write!(
                    f,
                    "SIMD width {width_bits} bits on target {triple} requires feature {feature:?}"
                ),
            },
            CompileError::UnsupportedTarget { triple } => {
                write!(f, "target {triple} is not in the v0.1 supported set")
            }
            CompileError::LlvmInit {
                triple,
                stage,
                reason,
            } => {
                write!(f, "LLVM {stage} failed for target {triple}: {reason}")
            }
            CompileError::UnsupportedMirShape { shape, detail } => {
                write!(f, "MIR shape {shape:?} not yet supported by emitter: {detail}")
            }
            CompileError::ObjectEmit { path, reason } => {
                write!(f, "LLVM object emission to {} failed: {reason}", path.display())
            }
        }
    }
}

impl std::error::Error for CompileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CompileError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edda_target::{AbiVariant, Arch, Os};
    use std::error::Error as _;

    fn triple() -> TargetTriple {
        TargetTriple::new(Arch::X86_64, Os::Linux, AbiVariant::Gnu)
    }

    #[test]
    fn simd_unsupported_uses_simd_target_unsupported_class() {
        let err = CompileError::SimdUnsupported {
            triple: triple(),
            width_bits: 512,
            span: Span::DUMMY,
            reason: SimdRejection::MissingFeature { feature: "avx512f" },
        };
        let d = err.to_diagnostic();
        assert_eq!(d.class, DiagnosticClass::SimdTargetUnsupported);
        assert_eq!(d.severity, Severity::Error);
    }

    #[test]
    fn unsupported_target_falls_back_to_parse_error() {
        let err = CompileError::UnsupportedTarget { triple: triple() };
        assert_eq!(err.to_diagnostic().class, DiagnosticClass::ParseError);
    }

    #[test]
    fn io_carries_source_and_path() {
        let err = CompileError::Io {
            path: PathBuf::from("/tmp/foo.o"),
            op: "write",
            source: io::Error::new(io::ErrorKind::PermissionDenied, "denied"),
        };
        assert!(err.source().is_some());
        let rendered = format!("{err}");
        assert!(rendered.contains("/tmp/foo.o"), "got: {rendered}");
        assert!(rendered.contains("write"), "got: {rendered}");
    }

    #[test]
    fn simd_missing_feature_message_names_feature_and_width() {
        let err = CompileError::SimdUnsupported {
            triple: triple(),
            width_bits: 256,
            span: Span::DUMMY,
            reason: SimdRejection::MissingFeature { feature: "avx2" },
        };
        let s = format!("{err}");
        assert!(s.contains("256"));
        assert!(s.contains("avx2"));
    }

    #[test]
    fn llvm_init_renders_stage_and_reason() {
        let err = CompileError::LlvmInit {
            triple: triple(),
            stage: "create_target_machine",
            reason: "no backend registered".to_string(),
        };
        let s = format!("{err}");
        assert!(s.contains("create_target_machine"), "got: {s}");
        assert!(s.contains("no backend registered"), "got: {s}");
        assert!(s.contains("x86-64"), "got: {s}");
        assert_eq!(err.to_diagnostic().class, DiagnosticClass::ParseError);
    }

    #[test]
    fn object_emit_renders_path_and_reason() {
        let err = CompileError::ObjectEmit {
            path: PathBuf::from("/tmp/out.o"),
            reason: "could not open file".to_string(),
        };
        let s = format!("{err}");
        assert!(s.contains("/tmp/out.o"), "got: {s}");
        assert!(s.contains("could not open file"), "got: {s}");
        assert_eq!(err.to_diagnostic().class, DiagnosticClass::ParseError);
    }

    #[test]
    fn simd_unsupported_width_message_omits_feature_name() {
        let err = CompileError::SimdUnsupported {
            triple: TargetTriple::new(Arch::Riscv64, Os::Linux, AbiVariant::Gnu),
            width_bits: 128,
            span: Span::DUMMY,
            reason: SimdRejection::UnsupportedWidth,
        };
        let s = format!("{err}");
        assert!(s.contains("128"));
        assert!(s.contains("riscv64"));
        assert!(!s.contains("requires feature"));
    }
}
