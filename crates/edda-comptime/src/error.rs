//! Comptime-evaluation error type and its `edda-diag` projection.
//!
//! Every fallible comptime operation surfaces failure as a
//! [`ComptimeError`]. The variants split into three families:
//!
//! - **Built-in misuse** (`ArityMismatch`, `ArgumentKindMismatch`,
//!   `LayoutUnavailable`, `OffsetOfNotYetSupported`,
//!   `FieldIntrospection`) — call-site shape problems caught
//!   before any layout computation produces a value.
//! - **Target-feature lookup** (`UnknownTargetFeature`) — the
//!   tri-valued result of `TargetCfg::target_has` projected onto the
//!   locked `unknown_target_feature` diagnostic class
//!   (`build-system.md` §8).
//! - **Comptime panic** (`Panic`) — a `panic <expr>` reached during
//!   comptime evaluation, surfaced as a compile error at the call
//!   site per `comptime.md` *Comptime-pure functions*.
//!
//! Errors are projected to an `edda-diag` `Diagnostic` via
//! [`ComptimeError::to_diagnostic`].

use std::fmt;

use edda_diag::{Diagnostic, DiagnosticClass, Severity};
use edda_span::Span;

use crate::builtin::{Builtin, BuiltinParamKind};
use crate::layout::LayoutUnsupported;
use crate::value::ValueKind;

/// Comptime-evaluation error.
#[derive(Clone, Debug)]
pub enum ComptimeError {
    /// A built-in call site supplied the wrong number of arguments.
    ArityMismatch {
        /// Call site.
        span: Span,
        /// Built-in that was called.
        builtin: Builtin,
        /// Arity required by the locked signature.
        expected: usize,
        /// Argument count supplied at the call site.
        found: usize,
    },

    /// A built-in argument was of the wrong shape (e.g. a `String`
    /// supplied where a `Type` is required).
    ArgumentKindMismatch {
        /// Call site.
        span: Span,
        /// Built-in that was called.
        builtin: Builtin,
        /// 0-based parameter index that failed.
        param_index: usize,
        /// Surface kind required by the locked signature.
        expected: BuiltinParamKind,
        /// Surface kind actually supplied.
        found: ValueKind,
    },

    /// A `size_of` or `align_of` was called on a [`edda_types::TyId`]
    /// whose layout this wave cannot compute. The carried
    /// [`LayoutUnsupported`] names the specific blocker.
    LayoutUnavailable {
        /// Call site.
        span: Span,
        /// Built-in that was called.
        builtin: Builtin,
        /// `Display` rendering of the type, for the diagnostic message.
        ty_display: String,
        /// Reason layout was unavailable.
        reason: LayoutUnsupported,
    },

    /// An `offset_of` call reached the evaluator.
    /// `@layout`-driven offsets are not yet implemented; the
    /// built-in is in the catalogue so it cannot be redefined, but
    /// the call is rejected until layout attributes land.
    OffsetOfNotYetSupported {
        /// Call site.
        span: Span,
    },

    /// A reflective-introspection built-in (`field_count`,
    /// `field_name_at`, `field_type_at`) could not produce a result
    /// for the supplied type — a non-aggregate type, an out-of-range
    /// index, a missing type-decl lookup, or a composite variant
    /// payload that has no single field type. The carried message is
    /// pre-rendered in the self-hosted reference's wording style.
    FieldIntrospection {
        /// Call site.
        span: Span,
        /// Built-in that produced the error.
        builtin: Builtin,
        /// Human-readable explanation.
        message: String,
    },

    /// `target_has(feature)` was called with a feature name outside
    /// the locked catalogue for the active arch. Projects onto the
    /// `unknown_target_feature` diagnostic class
    /// (`build-system.md` §8).
    UnknownTargetFeature {
        /// Call site.
        span: Span,
        /// Feature string that was queried.
        feature: String,
        /// Active target arch name.
        arch: String,
    },

    /// A `panic <expr>` was reached during comptime evaluation
    /// (`comptime.md` *Comptime-pure functions*). The current
    /// dispatch does not produce this variant — the HIR
    /// evaluator does — but the variant is part of the locked
    /// surface so the HIR evaluator can populate it without a breaking change.
    Panic {
        /// Call site that triggered the panic.
        span: Span,
        /// Panic message.
        message: String,
    },
}

impl ComptimeError {
    /// Source span of this error.
    pub const fn span(&self) -> Span {
        match self {
            Self::ArityMismatch { span, .. }
            | Self::ArgumentKindMismatch { span, .. }
            | Self::LayoutUnavailable { span, .. }
            | Self::OffsetOfNotYetSupported { span }
            | Self::FieldIntrospection { span, .. }
            | Self::UnknownTargetFeature { span, .. }
            | Self::Panic { span, .. } => *span,
        }
    }

    /// Project this error to an `edda-diag` `Diagnostic`.
    pub fn to_diagnostic(&self) -> Diagnostic {
        Diagnostic::new(
            self.diagnostic_class(),
            Severity::Error,
            self.span(),
            self.to_string(),
        )
    }

    /// Class assignment per `build-system.md` §8.
    fn diagnostic_class(&self) -> DiagnosticClass {
        match self {
            Self::UnknownTargetFeature { .. } => DiagnosticClass::UnknownTargetFeature,
            _ => DiagnosticClass::TypecheckError,
        }
    }
}

impl fmt::Display for ComptimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ArityMismatch {
                builtin,
                expected,
                found,
                ..
            } => {
                write!(
                    f,
                    "`{builtin}` takes {expected} argument(s), {found} supplied"
                )
            }
            Self::ArgumentKindMismatch {
                builtin,
                param_index,
                expected,
                found,
                ..
            } => {
                write!(
                    f,
                    "`{builtin}` argument {n} must be `{expected}`, got `{found}`",
                    n = param_index + 1
                )
            }
            Self::LayoutUnavailable {
                builtin,
                ty_display,
                reason,
                ..
            } => {
                write!(
                    f,
                    "`{builtin}` on type `{ty_display}`: {}",
                    reason.message()
                )
            }
            Self::OffsetOfNotYetSupported { .. } => {
                f.write_str("`offset_of` is not yet supported (@layout attributes pending)")
            }
            Self::FieldIntrospection {
                builtin, message, ..
            } => {
                write!(f, "`{builtin}`: {message}")
            }
            Self::UnknownTargetFeature { feature, arch, .. } => {
                write!(f, "unknown target feature `{feature}` for arch `{arch}`")
            }
            Self::Panic { message, .. } => {
                write!(f, "comptime panic: {message}")
            }
        }
    }
}

impl std::error::Error for ComptimeError {}
