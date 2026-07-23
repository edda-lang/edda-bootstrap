//! Local-allocation helper used by [`crate::builder::BodyBuilder`].
//!
//! Kept as a separate file so the LocalDecl-construction policy (mutability,
//! source, span) does not bleed into the public-facing builder API. All
//! callers go through [`push`]; `BodyBuilder` is the only consumer.

use crate::body::{Body, LocalDecl};
use crate::ids::LocalId;

/// Append a [`LocalDecl`] to `body.locals` and return its [`LocalId`].
pub(super) fn push(body: &mut Body, decl: LocalDecl) -> LocalId {
    body.locals.push(decl)
}
