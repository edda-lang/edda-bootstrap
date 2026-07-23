//! Canonical-form hash inputs — the §2 tuple that BLAKE3 hashes to
//! produce an [`ArtifactHash`].
//!
//! Per `docs/codegen/storage.md` §2, the hash input for a spec invocation
//! is the tuple `(body_version, spec_qualified_name, argument_tuple,
//! canonical_body, nested_invocation_set)`. [`CanonicalForm`] is the
//! Rust representation of that tuple. The `canonical_body` field is
//! opaque bytes at this layer; the AST → bytes serialisation lives in
//! the encoder.

use edda_cache::ArtifactHash;
use smol_str::SmolStr;

use crate::argument::ArgumentTuple;

/// The §2 canonical-form hash input bundle.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct CanonicalForm {
    /// Fully qualified spec name (resolved through imports), dot-joined.
    pub spec_qualified: SmolStr,
    /// The comptime argument tuple, in declaration order.
    pub argument_tuple: ArgumentTuple,
    /// The canonical body bytes (`storage.md` §4): opaque input from
    /// the caller, produced deterministically by the AST encoder.
    pub canonical_body: Box<[u8]>,
    /// The transitive nested-invocation set (`storage.md` §2 item 5).
    pub nested: Box<[NestedDependency]>,
}

impl CanonicalForm {
    /// Construct from the four hash inputs.
    pub fn new(
        spec_qualified: impl Into<SmolStr>,
        argument_tuple: ArgumentTuple,
        canonical_body: impl Into<Box<[u8]>>,
        nested: impl Into<Box<[NestedDependency]>>,
    ) -> Self {
        CanonicalForm {
            spec_qualified: spec_qualified.into(),
            argument_tuple,
            canonical_body: canonical_body.into(),
            nested: nested.into(),
        }
    }
}

//   spec; same convention as [`CanonicalForm::spec_qualified`]
//   own argument tuple computed at this enclosing-spec instantiation
//   (per `storage.md` §2 item 5)
/// One element of [`CanonicalForm::nested`].
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct NestedDependency {
    /// The nested spec's fully qualified name.
    pub qualified: SmolStr,
    /// The 32-byte BLAKE3 hash of the nested spec's argument tuple.
    pub argument_tuple_hash: ArtifactHash,
}

impl NestedDependency {
    /// Construct from the two fields.
    pub fn new(qualified: impl Into<SmolStr>, argument_tuple_hash: ArtifactHash) -> Self {
        NestedDependency {
            qualified: qualified.into(),
            argument_tuple_hash,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_form_round_trips_through_new() {
        let form = CanonicalForm::new(
            "std.option.Option",
            ArgumentTuple::empty(),
            Vec::<u8>::new(),
            Vec::<NestedDependency>::new(),
        );
        assert_eq!(form.spec_qualified.as_str(), "std.option.Option");
        assert!(form.argument_tuple.is_empty());
        assert!(form.canonical_body.is_empty());
        assert!(form.nested.is_empty());
    }

    #[test]
    fn nested_dependency_holds_both_fields() {
        let h = ArtifactHash::from_bytes([0xab; 32]);
        let dep = NestedDependency::new("std.option.Option", h);
        assert_eq!(dep.qualified.as_str(), "std.option.Option");
        assert_eq!(dep.argument_tuple_hash, h);
    }
}
