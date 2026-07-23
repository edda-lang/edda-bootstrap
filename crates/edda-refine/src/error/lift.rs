//! AST refinement-expression lifter error (`error/lift.rs`).
//!
//! `LiftError` is produced by [`lift_predicate`](crate::lift_predicate) and
//! [`lift_clause`](crate::lift_clause) when an AST expression falls outside
//! the predicate fragment, or when the [`PredicateEnv`](crate::PredicateEnv)
//! lookups required to lift it fail. The typechecker projects each variant
//! into its own diagnostic; the [`span`](LiftError::span) accessor identifies
//! the offending sub-expression.

use std::fmt;

use smol_str::SmolStr;

use edda_span::Span;

//            can render as its own diagnostic — `NotAdmittedInPredicate`
//            is the most common
//          offending source position
/// Why the lifter refused an AST expression.
#[derive(Clone, Debug)]
pub enum LiftError {
    /// The expression form is not in the predicate fragment. Cite
    /// `refinements.md` *The predicate fragment* (admitted column)
    /// and *Not admitted in predicates* (rejected column) in the
    /// resulting diagnostic.
    NotAdmittedInPredicate {
        /// Short human-readable description of the offending form.
        form: &'static str,
        /// Source position where the form appeared.
        span: Span,
    },
    /// The form is admitted but not yet implemented. Lift this
    /// as a `typecheck_error` and direct the user at the deferral; do not
    /// route to `@trust` automatically.
    Unsupported {
        /// Description of the unsupported form / sort.
        what: String,
        /// Source position where the form appeared.
        span: Span,
    },
    /// The [`PredicateEnv`](crate::PredicateEnv) could not resolve a path.
    /// The typechecker is responsible for resolution — an unresolved path
    /// here points at a typechecker bug or a use-of-undeclared-binding
    /// diagnostic that should already have been emitted upstream.
    UnresolvedPath {
        /// Source position of the offending path.
        span: Span,
    },
    /// The [`PredicateEnv`](crate::PredicateEnv) returned an unexpected sort.
    /// Typically the typechecker mis-inferred something — e.g. a `Field`
    /// lookup where the receiver wasn't a record sort.
    SortMismatch {
        /// Source position.
        span: Span,
        /// What the lifter expected (free-form description).
        expected: String,
    },
    /// A field name didn't resolve against the receiver's record sort.
    UnknownField {
        /// Source position of the field access.
        span: Span,
        /// Field name (resolved via [`PredicateEnv::ident_name`](crate::PredicateEnv::ident_name)).
        field: SmolStr,
    },
    /// A cast target type couldn't be projected to a [`Sort::Int`](crate::Sort::Int).
    /// Only integer-to-integer casts are admitted.
    UnsupportedCastTarget {
        /// Source position of the cast expression.
        span: Span,
    },
    /// An integer literal couldn't be reconciled with the
    /// [`PredicateEnv`](crate::PredicateEnv)-supplied target sort (e.g. value
    /// out of range for the inferred sort).
    IntLitOutOfRange {
        /// Source position of the literal.
        span: Span,
        /// String form of the offending value.
        value: String,
    },
    /// A block expression carried statements; only `{ trailing }` blocks
    /// are admissible in predicate position.
    NonTrivialBlock {
        /// Source position of the block.
        span: Span,
    },
}

impl fmt::Display for LiftError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LiftError::NotAdmittedInPredicate { form, .. } => {
                write!(f, "`{form}` is not admitted in the predicate fragment")
            }
            LiftError::Unsupported { what, .. } => {
                write!(f, "not yet supported: {what}")
            }
            LiftError::UnresolvedPath { .. } => f.write_str("unresolved path in refinement"),
            LiftError::SortMismatch { expected, .. } => {
                write!(f, "sort mismatch: expected {expected}")
            }
            LiftError::UnknownField { field, .. } => {
                write!(f, "unknown field `{field}` on record")
            }
            LiftError::UnsupportedCastTarget { .. } => {
                f.write_str("cast target is not an integer sort")
            }
            LiftError::IntLitOutOfRange { value, .. } => {
                write!(f, "integer literal `{value}` out of range for inferred sort")
            }
            LiftError::NonTrivialBlock { .. } => {
                f.write_str("block with statements is not admitted in predicate position")
            }
        }
    }
}

impl std::error::Error for LiftError {}

impl LiftError {
    /// Source position the error is attributed to. Used by diagnostic
    /// rendering to highlight the offending sub-expression.
    pub fn span(&self) -> Span {
        match self {
            LiftError::NotAdmittedInPredicate { span, .. }
            | LiftError::Unsupported { span, .. }
            | LiftError::UnresolvedPath { span }
            | LiftError::SortMismatch { span, .. }
            | LiftError::UnknownField { span, .. }
            | LiftError::UnsupportedCastTarget { span }
            | LiftError::IntLitOutOfRange { span, .. }
            | LiftError::NonTrivialBlock { span } => *span,
        }
    }
}
