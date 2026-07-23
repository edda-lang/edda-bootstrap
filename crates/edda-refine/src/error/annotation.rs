//! Annotation-construction error (`error/annotation.rs`).
//!
//! `AnnotationError` is returned by
//! [`UnverifiedAnnotation::new`](crate::UnverifiedAnnotation::new) and
//! [`TrustAnnotation::new`](crate::TrustAnnotation::new) when the supplied
//! `reason` string violates the audit-surface contract from
//! `docs/types/refinement-decidability.md` §9.

use std::fmt;

//            typechecker needs to discriminate
/// Why [`UnverifiedAnnotation::new`](crate::UnverifiedAnnotation::new) or
/// [`TrustAnnotation::new`](crate::TrustAnnotation::new) refused.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum AnnotationError {
    /// Reason string was empty. Refinement-decidability.md §9 requires every
    /// `@unverified` / `@trust` annotation to carry a non-empty reason — the
    /// reason is the audit surface.
    EmptyReason,
}

impl fmt::Display for AnnotationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AnnotationError::EmptyReason => {
                f.write_str("annotation reason string must be non-empty")
            }
        }
    }
}

impl std::error::Error for AnnotationError {}
