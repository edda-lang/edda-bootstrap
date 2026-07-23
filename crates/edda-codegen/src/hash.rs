//! BLAKE3 hashing of [`CanonicalForm`] — the entry point for
//! producing an [`ArtifactHash`] from a spec invocation's hash inputs.
//!
//! Per `docs/codegen/storage.md` §2, the canonical-form bytes are fed
//! through BLAKE3 keyed by the project-wide constant (here, the version
//! byte that prefixes the input). Two invocations whose hashes match
//! share a single artifact regardless of which file invoked which.

use edda_cache::{ArtifactHash, BodyVersion, hash_bytes};

use crate::canonical::CanonicalForm;
use crate::encode::encode;

/// Compute the content-addressed [`ArtifactHash`] for `form` under
/// `version`. The byte input fed to BLAKE3 is the deterministic
/// serialisation defined by [`crate::encode::encode`].
pub fn compute_hash(form: &CanonicalForm, version: BodyVersion) -> ArtifactHash {
    let bytes = encode(form, version);
    hash_bytes(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::argument::{Argument, ArgumentTuple, PrimitiveValue};
    use crate::canonical::{CanonicalForm, NestedDependency};

    fn form(name: &str, args: ArgumentTuple) -> CanonicalForm {
        CanonicalForm::new(
            name,
            args,
            Vec::<u8>::new(),
            Vec::<NestedDependency>::new(),
        )
    }

    #[test]
    fn same_inputs_produce_same_hash() {
        let a = compute_hash(&form("std.option.Option", ArgumentTuple::empty()), BodyVersion::CURRENT);
        let b = compute_hash(&form("std.option.Option", ArgumentTuple::empty()), BodyVersion::CURRENT);
        assert_eq!(a, b);
    }

    #[test]
    fn different_spec_names_hash_differently() {
        let a = compute_hash(&form("std.option.Option", ArgumentTuple::empty()), BodyVersion::CURRENT);
        let b = compute_hash(&form("std.option.Result", ArgumentTuple::empty()), BodyVersion::CURRENT);
        assert_ne!(a, b);
    }

    #[test]
    fn different_arguments_hash_differently() {
        let i32_arg = ArgumentTuple::new(vec![Argument::Type("i32".into())]);
        let u32_arg = ArgumentTuple::new(vec![Argument::Type("u32".into())]);
        let a = compute_hash(&form("std.option.Option", i32_arg), BodyVersion::CURRENT);
        let b = compute_hash(&form("std.option.Option", u32_arg), BodyVersion::CURRENT);
        assert_ne!(a, b);
    }

    #[test]
    fn primitive_width_affects_hash() {
        // Same value, different declared parameter type — different hash.
        let u32_one = ArgumentTuple::new(vec![Argument::Primitive(PrimitiveValue::U32(1))]);
        let u64_one = ArgumentTuple::new(vec![Argument::Primitive(PrimitiveValue::U64(1))]);
        let a = compute_hash(&form("spec", u32_one), BodyVersion::CURRENT);
        let b = compute_hash(&form("spec", u64_one), BodyVersion::CURRENT);
        assert_ne!(a, b);
    }

    #[test]
    fn body_version_bump_invalidates_every_artifact() {
        let f = form("std.option.Option", ArgumentTuple::empty());
        let v1 = compute_hash(&f, BodyVersion(0x01));
        let v2 = compute_hash(&f, BodyVersion(0x02));
        assert_ne!(v1, v2);
    }

    #[test]
    fn canonical_body_bytes_contribute_to_hash() {
        let a = CanonicalForm::new(
            "spec",
            ArgumentTuple::empty(),
            Vec::<u8>::new(),
            Vec::<NestedDependency>::new(),
        );
        let b = CanonicalForm::new(
            "spec",
            ArgumentTuple::empty(),
            vec![0xde, 0xad, 0xbe, 0xef],
            Vec::<NestedDependency>::new(),
        );
        assert_ne!(
            compute_hash(&a, BodyVersion::CURRENT),
            compute_hash(&b, BodyVersion::CURRENT),
        );
    }

    #[test]
    fn nested_dependency_changes_hash() {
        let no_nested = CanonicalForm::new(
            "outer",
            ArgumentTuple::empty(),
            Vec::<u8>::new(),
            Vec::<NestedDependency>::new(),
        );
        let with_nested = CanonicalForm::new(
            "outer",
            ArgumentTuple::empty(),
            Vec::<u8>::new(),
            vec![NestedDependency::new(
                "inner",
                ArtifactHash::from_bytes([0xab; 32]),
            )],
        );
        assert_ne!(
            compute_hash(&no_nested, BodyVersion::CURRENT),
            compute_hash(&with_nested, BodyVersion::CURRENT),
        );
    }

    #[test]
    fn nested_dep_argument_hash_is_load_bearing() {
        // Same nested-spec name, different argument-tuple hash.
        let a = CanonicalForm::new(
            "outer",
            ArgumentTuple::empty(),
            Vec::<u8>::new(),
            vec![NestedDependency::new(
                "inner",
                ArtifactHash::from_bytes([0xa; 32]),
            )],
        );
        let b = CanonicalForm::new(
            "outer",
            ArgumentTuple::empty(),
            Vec::<u8>::new(),
            vec![NestedDependency::new(
                "inner",
                ArtifactHash::from_bytes([0xb; 32]),
            )],
        );
        assert_ne!(
            compute_hash(&a, BodyVersion::CURRENT),
            compute_hash(&b, BodyVersion::CURRENT),
        );
    }
}
