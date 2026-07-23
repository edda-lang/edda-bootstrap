//! Predicate → Z3 translation error (`error/translate.rs`).
//!
//! `TranslationError` surfaces from
//! [`Translator`](crate::translate::Translator) when an IR construct or sort
//! can't be projected to Z3 — either because the translator hasn't implemented it
//! yet, because a literal exceeds Z3's parser limits, or because the typed
//! [`Schema`](crate::Schema) handed to discharge doesn't declare a record /
//! sum the predicate references. The Z3 backend wraps these into
//! [`DischargeOutcome::Unknown`](crate::DischargeOutcome::Unknown) for the
//! caller.

use std::fmt;

use smol_str::SmolStr;

use crate::sort::Sort;

//            diagnostic rendering needs to discriminate
/// Why translation refused a predicate.
#[derive(Clone, Debug)]
pub enum TranslationError {
    /// The predicate variant or sort is not yet supported by the translator.
    Unsupported {
        /// Short description of what was rejected (used in the Unknown reason
        /// string surfaced to diagnostics).
        what: String,
    },
    /// An integer literal exceeded what Z3's `from_i64` / `from_u64` /
    /// `from_str` accepted.
    IntLitOutOfRange {
        /// String form of the offending value.
        value: String,
    },
    /// Sort mismatch detected at translation time (the IR claimed one sort,
    /// the Z3 term ended up with another).
    SortMismatch {
        /// Sort the caller expected.
        expected: Sort,
    },
    /// The predicate references a record or sum that the [`Schema`](crate::Schema)
    /// does not know about. The typechecker is responsible for populating
    /// the schema before discharge; an unknown name signals a typechecker bug.
    UnknownTypeName {
        /// Name of the missing record / sum.
        name: SmolStr,
    },
    /// The predicate references a field or variant that the
    /// [`Schema`](crate::Schema) doesn't carry on the relevant record / sum.
    UnknownMember {
        /// Owning type's name.
        owner: SmolStr,
        /// Field / variant name that was not found.
        member: SmolStr,
    },
}

impl fmt::Display for TranslationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TranslationError::Unsupported { what } => {
                write!(f, "not yet supported: {what}")
            }
            TranslationError::IntLitOutOfRange { value } => {
                write!(f, "integer literal out of range: {value}")
            }
            TranslationError::SortMismatch { expected } => {
                write!(f, "sort mismatch: expected {expected:?}")
            }
            TranslationError::UnknownTypeName { name } => {
                write!(f, "type not in schema: {name}")
            }
            TranslationError::UnknownMember { owner, member } => {
                write!(f, "unknown member {member} on type {owner}")
            }
        }
    }
}

impl std::error::Error for TranslationError {}
