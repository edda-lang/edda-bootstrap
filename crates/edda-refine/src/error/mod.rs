//! Crate-wide error types. Each sub-module owns one error enum produced at
//! a distinct stage of the refinement pipeline.
//!
//! - [`annotation::AnnotationError`] — rejected at
//!   [`UnverifiedAnnotation::new`](crate::UnverifiedAnnotation::new) /
//!   [`TrustAnnotation::new`](crate::TrustAnnotation::new) when the audit
//!   `reason` string is empty.
//! - [`lift::LiftError`] — surfaced by the AST → [`Predicate`](crate::Predicate)
//!   lifter when an expression falls outside the predicate fragment.
//! - [`translate::TranslationError`] — surfaced by the
//!   [`Translator`](crate::translate::Translator) when an IR construct or
//!   sort can't be projected to Z3. Only compiled with the `refine` feature.
//! - [`discharge`] — the discharge-failure surface
//!   ([`RefineError`](discharge::RefineError) +
//!   [`DischargeFailure`](discharge::DischargeFailure)) consumed by the
//!   typechecker's diagnostic renderer.

mod annotation;
mod discharge;
mod lift;
#[cfg(feature = "refine")]
mod translate;

pub use annotation::AnnotationError;
pub use discharge::{DischargeFailure, RefineError};
pub use lift::LiftError;
#[cfg(feature = "refine")]
pub use translate::TranslationError;
