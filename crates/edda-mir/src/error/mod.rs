//! MIR error type and its diagnostic-class mapping.
//!
//! [`MirError`] is the top-level fallible-call return for everything in this
//! crate. It wraps [`ValidationError`] variants and the [`LoweringError`]
//! family used by the typed-HIR -> MIR lowering pass.
//! Each variant carries enough structural context to look the offending entity
//! up in a [`crate::MirProgram`] for human-readable rendering.

mod lowering;
mod validation;

use edda_diag::DiagnosticClass;
use edda_span::Span;

pub use lowering::LoweringError;
pub use validation::ValidationError;

/// Top-level error type for the `edda-mir` crate.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum MirError {
    /// A structural validation pass found a problem in the program.
    Validation(ValidationError),
    /// The typed-HIR -> MIR lowering pass found a problem in the input HIR.
    Lowering(LoweringError),
}

impl MirError {
    /// Map this error to the locked [`DiagnosticClass`] best describing it.
    ///
    /// Both validation and lowering failures map to
    /// [`DiagnosticClass::TypecheckError`] — the closest fit in the locked
    /// set. This may be refined in the future when layout resolution
    /// introduces classes that are not strictly type-check errors.
    pub fn class(&self) -> DiagnosticClass {
        match self {
            MirError::Validation(_) | MirError::Lowering(_) => DiagnosticClass::TypecheckError,
        }
    }

    /// Source span best associated with this error, or [`Span::DUMMY`] when no
    /// span is recorded.
    ///
    /// [`ValidationError`] is structural and encodes body / block / local ids
    /// rather than spans, so all validation variants return [`Span::DUMMY`].
    /// [`LoweringError`] always carries a span borrowed from the originating
    /// HIR view; this accessor surfaces it so callers can render the
    /// diagnostic against the source file.
    pub fn span(&self) -> Span {
        match self {
            MirError::Validation(_) => Span::DUMMY,
            MirError::Lowering(l) => l.span(),
        }
    }
}

impl std::fmt::Display for MirError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MirError::Validation(v) => std::fmt::Display::fmt(v, f),
            MirError::Lowering(l) => std::fmt::Display::fmt(l, f),
        }
    }
}

impl std::error::Error for MirError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MirError::Validation(v) => Some(v),
            MirError::Lowering(l) => Some(l),
        }
    }
}

impl From<ValidationError> for MirError {
    fn from(v: ValidationError) -> Self {
        MirError::Validation(v)
    }
}

impl From<LoweringError> for MirError {
    fn from(l: LoweringError) -> Self {
        MirError::Lowering(l)
    }
}
