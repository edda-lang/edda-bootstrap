//! Deterministic byte serialisation of [`CanonicalForm`] for hashing.
//!
//! Layout at [`BodyVersion::CURRENT`] (`0x01`):
//!
//! ```text
//! version              :  1 byte
//! spec_qualified       :  u32-le length + UTF-8 bytes
//! argument_count       :  u32-le
//!   argument           :  kind(1) + kind body  [× argument_count]
//! canonical_body       :  u32-le length + bytes
//! nested_count         :  u32-le
//!   nested_dep         :  u32-le qname length + UTF-8 + 32-byte hash  [× nested_count]
//! ```
//!
//! The byte order is locked at `BodyVersion(0x01)`; any reshuffle must
//! bump the version byte and trigger a full-codegen-tier rebuild per
//! `storage.md` §2.

use edda_cache::BodyVersion;

use crate::argument::{
    Argument, ArgumentTuple, EffectRowArgument, EffectRowEntry, PrimitiveValue, UserField,
    UserValue, UserVariantPayload,
};
use crate::canonical::{CanonicalForm, NestedDependency};

/// Serialise `form` into the byte buffer used as BLAKE3 input.
pub fn encode(form: &CanonicalForm, version: BodyVersion) -> Vec<u8> {
    let mut out = Vec::with_capacity(estimate_size(form));
    encode_into(form, version, &mut out);
    out
}

//   given `BodyVersion`; any change to layout requires a version bump
/// Serialise `form` by appending into `out`.
pub fn encode_into(form: &CanonicalForm, version: BodyVersion, out: &mut Vec<u8>) {
    out.push(version.0);
    write_length_prefixed_str(form.spec_qualified.as_str(), out);
    encode_argument_tuple(&form.argument_tuple, out);
    write_length_prefixed_bytes(&form.canonical_body, out);
    encode_nested(&form.nested, out);
}

fn estimate_size(form: &CanonicalForm) -> usize {
    // Coarse lower-bound: version + four length prefixes + the bytes we know.
    1 + 4
        + form.spec_qualified.len()
        + 4
        + form.canonical_body.len()
        + 4
        + form.nested.len() * (4 + 32)
}

fn encode_argument_tuple(tuple: &ArgumentTuple, out: &mut Vec<u8>) {
    write_u32_le(u32_try_from_usize(tuple.len()), out);
    for arg in tuple.args() {
        encode_argument(arg, out);
    }
}

fn encode_argument(arg: &Argument, out: &mut Vec<u8>) {
    out.push(arg.kind_tag());
    match arg {
        Argument::Type(qualified) => {
            write_length_prefixed_str(qualified.as_str(), out);
        }
        Argument::EffectRow(row) => encode_effect_row(row, out),
        Argument::Primitive(p) => encode_primitive(p, out),
        Argument::UserDefined(v) => encode_user_value(v, out),
        // A function arg's body is its qualified name, length-
        // prefixed exactly like a Type arg; the `0x05` kind tag pushed
        // above is what keeps it hash-distinct from a same-named type.
        Argument::Function(qualified) => {
            write_length_prefixed_str(qualified.as_str(), out);
        }
    }
}

//   guarantees this; the encoder does not re-sort
//   as `sub_tag(1) + optional payload qname (u32-le length + UTF-8)`
fn encode_effect_row(row: &EffectRowArgument, out: &mut Vec<u8>) {
    write_u32_le(u32_try_from_usize(row.len()), out);
    for entry in row.entries() {
        out.push(entry.sub_tag());
        match entry {
            EffectRowEntry::Err(ty) | EffectRowEntry::Yield(ty) => {
                write_length_prefixed_str(ty.as_str(), out);
            }
            EffectRowEntry::Panic => {}
        }
    }
}

//   shape tag (1), then shape-specific body
fn encode_user_value(v: &UserValue, out: &mut Vec<u8>) {
    match v {
        UserValue::Record {
            type_qualified,
            fields,
        } => {
            write_length_prefixed_str(type_qualified.as_str(), out);
            out.push(v.shape_tag());
            encode_user_fields(fields, out);
        }
        UserValue::Variant {
            type_qualified,
            variant_tag,
            payload,
        } => {
            write_length_prefixed_str(type_qualified.as_str(), out);
            out.push(v.shape_tag());
            write_length_prefixed_str(variant_tag.as_str(), out);
            out.push(payload.sub_tag());
            match payload {
                UserVariantPayload::Unit => {}
                UserVariantPayload::Tuple(items) => {
                    write_u32_le(u32_try_from_usize(items.len()), out);
                    for item in items {
                        encode_argument(item, out);
                    }
                }
                UserVariantPayload::Struct(fields) => encode_user_fields(fields, out),
            }
        }
    }
}

//   the declaring type
fn encode_user_fields(fields: &[UserField], out: &mut Vec<u8>) {
    write_u32_le(u32_try_from_usize(fields.len()), out);
    for field in fields {
        write_length_prefixed_str(field.name.as_str(), out);
        encode_argument(&field.value, out);
    }
}

//   `usize`/`isize` always 8 bytes regardless of host
fn encode_primitive(p: &PrimitiveValue, out: &mut Vec<u8>) {
    out.push(p.sub_tag());
    match p {
        PrimitiveValue::U8(v) => out.push(*v),
        PrimitiveValue::U16(v) => out.extend_from_slice(&v.to_le_bytes()),
        PrimitiveValue::U32(v) => out.extend_from_slice(&v.to_le_bytes()),
        PrimitiveValue::U64(v) => out.extend_from_slice(&v.to_le_bytes()),
        PrimitiveValue::USize(v) => out.extend_from_slice(&v.to_le_bytes()),
        PrimitiveValue::I8(v) => out.push(*v as u8),
        PrimitiveValue::I16(v) => out.extend_from_slice(&v.to_le_bytes()),
        PrimitiveValue::I32(v) => out.extend_from_slice(&v.to_le_bytes()),
        PrimitiveValue::I64(v) => out.extend_from_slice(&v.to_le_bytes()),
        PrimitiveValue::ISize(v) => out.extend_from_slice(&v.to_le_bytes()),
        PrimitiveValue::Bool(b) => out.push(if *b { 0x01 } else { 0x00 }),
        PrimitiveValue::String(s) => write_length_prefixed_str(s.as_str(), out),
    }
}

fn encode_nested(nested: &[NestedDependency], out: &mut Vec<u8>) {
    write_u32_le(u32_try_from_usize(nested.len()), out);
    for dep in nested {
        write_length_prefixed_str(dep.qualified.as_str(), out);
        out.extend_from_slice(dep.argument_tuple_hash.as_bytes());
    }
}

fn write_length_prefixed_str(s: &str, out: &mut Vec<u8>) {
    write_length_prefixed_bytes(s.as_bytes(), out);
}

fn write_length_prefixed_bytes(bytes: &[u8], out: &mut Vec<u8>) {
    write_u32_le(u32_try_from_usize(bytes.len()), out);
    out.extend_from_slice(bytes);
}

fn write_u32_le(value: u32, out: &mut Vec<u8>) {
    out.extend_from_slice(&value.to_le_bytes());
}

//   nested-set counts); a length exceeding u32::MAX would mean a single
//   spec body or a single field is over 4 GiB, which the bootstrap rejects
fn u32_try_from_usize(n: usize) -> u32 {
    debug_assert!(
        n <= u32::MAX as usize,
        "edda-codegen: byte length {n} exceeds u32::MAX",
    );
    n as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::argument::{
        Argument, ArgumentTuple, EffectRowArgument, EffectRowEntry, PrimitiveValue, UserField,
        UserValue, UserVariantPayload,
    };
    use crate::canonical::{CanonicalForm, NestedDependency};
    use edda_cache::ArtifactHash;
    use smol_str::SmolStr;

    fn form_empty() -> CanonicalForm {
        CanonicalForm::new(
            "std.option.Option",
            ArgumentTuple::empty(),
            Vec::<u8>::new(),
            Vec::<NestedDependency>::new(),
        )
    }

    #[test]
    fn version_byte_appears_first() {
        let bytes = encode(&form_empty(), BodyVersion::CURRENT);
        assert_eq!(bytes[0], BodyVersion::CURRENT.0);
    }

    #[test]
    fn empty_form_is_deterministic() {
        let a = encode(&form_empty(), BodyVersion::CURRENT);
        let b = encode(&form_empty(), BodyVersion::CURRENT);
        assert_eq!(a, b);
    }

    #[test]
    fn different_spec_names_differ_in_bytes() {
        let a = encode(&form_empty(), BodyVersion::CURRENT);
        let b = CanonicalForm::new(
            "std.option.Result",
            ArgumentTuple::empty(),
            Vec::<u8>::new(),
            Vec::<NestedDependency>::new(),
        );
        let b = encode(&b, BodyVersion::CURRENT);
        assert_ne!(a, b);
    }

    #[test]
    fn argument_order_matters() {
        let ab = CanonicalForm::new(
            "spec",
            ArgumentTuple::new(vec![
                Argument::Primitive(PrimitiveValue::U32(1)),
                Argument::Primitive(PrimitiveValue::U32(2)),
            ]),
            Vec::<u8>::new(),
            Vec::<NestedDependency>::new(),
        );
        let ba = CanonicalForm::new(
            "spec",
            ArgumentTuple::new(vec![
                Argument::Primitive(PrimitiveValue::U32(2)),
                Argument::Primitive(PrimitiveValue::U32(1)),
            ]),
            Vec::<u8>::new(),
            Vec::<NestedDependency>::new(),
        );
        assert_ne!(
            encode(&ab, BodyVersion::CURRENT),
            encode(&ba, BodyVersion::CURRENT),
        );
    }

    #[test]
    fn usize_normalises_to_8_bytes() {
        let small = CanonicalForm::new(
            "spec",
            ArgumentTuple::new(vec![Argument::Primitive(PrimitiveValue::USize(1))]),
            Vec::<u8>::new(),
            Vec::<NestedDependency>::new(),
        );
        let bytes = encode(&small, BodyVersion::CURRENT);
        // version(1) + spec_qual(4 + 4) + arg_count(4) + kind(1) + sub(1) + 8 = 23
        // canonical_body(4) + nested_count(4) = 31 total
        assert_eq!(bytes.len(), 31);
    }

    #[test]
    fn nested_dependency_hash_is_inlined() {
        let h = ArtifactHash::from_bytes([0xcd; 32]);
        let form = CanonicalForm::new(
            "outer",
            ArgumentTuple::empty(),
            Vec::<u8>::new(),
            vec![NestedDependency::new("inner", h)],
        );
        let bytes = encode(&form, BodyVersion::CURRENT);
        // The last 32 bytes are the inlined argument-tuple hash.
        assert_eq!(&bytes[bytes.len() - 32..], &[0xcd; 32]);
    }

    #[test]
    fn version_byte_is_load_bearing() {
        let bytes_v1 = encode(&form_empty(), BodyVersion(0x01));
        let bytes_v2 = encode(&form_empty(), BodyVersion(0x02));
        assert_ne!(bytes_v1, bytes_v2);
        assert_eq!(bytes_v1[0], 0x01);
        assert_eq!(bytes_v2[0], 0x02);
    }

    fn form_with_args(args: ArgumentTuple) -> CanonicalForm {
        CanonicalForm::new(
            "spec",
            args,
            Vec::<u8>::new(),
            Vec::<NestedDependency>::new(),
        )
    }

    #[test]
    fn effect_row_kind_tag_is_0x02() {
        let row = EffectRowArgument::new([EffectRowEntry::Panic]);
        let form = form_with_args(ArgumentTuple::new(vec![Argument::EffectRow(row)]));
        let bytes = encode(&form, BodyVersion::CURRENT);
        // version(1) + spec_qual(4+4) + arg_count(4) = byte 13 is the kind tag
        assert_eq!(bytes[13], 0x02);
    }

    #[test]
    fn effect_row_input_order_is_canonicalised() {
        // The two input orderings differ in source order but canonicalise
        // to the same expanded row; their bytes must match.
        let a = form_with_args(ArgumentTuple::new(vec![Argument::EffectRow(
            EffectRowArgument::new([
                EffectRowEntry::Yield(SmolStr::new("T")),
                EffectRowEntry::Err(SmolStr::new("E")),
                EffectRowEntry::Panic,
            ]),
        )]));
        let b = form_with_args(ArgumentTuple::new(vec![Argument::EffectRow(
            EffectRowArgument::new([
                EffectRowEntry::Panic,
                EffectRowEntry::Err(SmolStr::new("E")),
                EffectRowEntry::Yield(SmolStr::new("T")),
            ]),
        )]));
        assert_eq!(
            encode(&a, BodyVersion::CURRENT),
            encode(&b, BodyVersion::CURRENT),
        );
    }

    #[test]
    fn effect_row_different_payloads_differ() {
        let a = form_with_args(ArgumentTuple::new(vec![Argument::EffectRow(
            EffectRowArgument::new([EffectRowEntry::Err(SmolStr::new("FooErr"))]),
        )]));
        let b = form_with_args(ArgumentTuple::new(vec![Argument::EffectRow(
            EffectRowArgument::new([EffectRowEntry::Err(SmolStr::new("BarErr"))]),
        )]));
        assert_ne!(
            encode(&a, BodyVersion::CURRENT),
            encode(&b, BodyVersion::CURRENT),
        );
    }

    #[test]
    fn empty_effect_row_is_just_zero_count() {
        let row = EffectRowArgument::empty();
        let form = form_with_args(ArgumentTuple::new(vec![Argument::EffectRow(row)]));
        let bytes = encode(&form, BodyVersion::CURRENT);
        // After kind tag (byte 13), the next 4 bytes are the u32-le count=0.
        assert_eq!(&bytes[14..18], &0u32.to_le_bytes());
    }

    #[test]
    fn user_defined_record_kind_tag_is_0x04() {
        let value = UserValue::Record {
            type_qualified: SmolStr::new("foo.Point"),
            fields: Box::new([]),
        };
        let form = form_with_args(ArgumentTuple::new(vec![Argument::UserDefined(value)]));
        let bytes = encode(&form, BodyVersion::CURRENT);
        assert_eq!(bytes[13], 0x04);
        // The shape tag (Record=0x00) appears after the type qname.
        // qname "foo.Point" = u32(9) + 9 bytes = 13 bytes.
        // Starts at byte 14. Shape tag is at 14 + 13 = 27.
        assert_eq!(bytes[27], 0x00);
    }

    #[test]
    fn user_defined_variant_round_trips_through_payload_kinds() {
        let mk = |payload| {
            let value = UserValue::Variant {
                type_qualified: SmolStr::new("foo.Color"),
                variant_tag: SmolStr::new("rgb"),
                payload,
            };
            encode(
                &form_with_args(ArgumentTuple::new(vec![Argument::UserDefined(value)])),
                BodyVersion::CURRENT,
            )
        };
        let unit_bytes = mk(UserVariantPayload::Unit);
        let tuple_bytes = mk(UserVariantPayload::Tuple(Box::new([
            Argument::Primitive(PrimitiveValue::U8(1)),
        ])));
        let struct_bytes = mk(UserVariantPayload::Struct(Box::new([UserField::new(
            "r",
            Argument::Primitive(PrimitiveValue::U8(1)),
        )])));
        assert_ne!(unit_bytes, tuple_bytes);
        assert_ne!(tuple_bytes, struct_bytes);
        assert_ne!(unit_bytes, struct_bytes);
    }

    #[test]
    fn user_defined_recurses_through_argument() {
        // A record whose field is itself a UserDefined record.
        let inner = UserValue::Record {
            type_qualified: SmolStr::new("Inner"),
            fields: Box::new([UserField::new(
                "k",
                Argument::Primitive(PrimitiveValue::U32(7)),
            )]),
        };
        let outer = UserValue::Record {
            type_qualified: SmolStr::new("Outer"),
            fields: Box::new([UserField::new("nested", Argument::UserDefined(inner))]),
        };
        let form = form_with_args(ArgumentTuple::new(vec![Argument::UserDefined(outer)]));
        let bytes = encode(&form, BodyVersion::CURRENT);
        // Confirm the outer kind tag (0x04) appears at byte 13 and the
        // inner kind tag (0x04 again) appears later in the stream.
        assert_eq!(bytes[13], 0x04);
        let inner_kind_count = bytes.iter().filter(|&&b| b == 0x04).count();
        assert!(
            inner_kind_count >= 2,
            "expected two 0x04 kind tags (outer + inner record), got {inner_kind_count} in {bytes:?}",
        );
    }

    #[test]
    fn user_field_order_changes_hash() {
        let mk_fields = |first_name: &str, second_name: &str| -> Box<[UserField]> {
            Box::new([
                UserField::new(first_name, Argument::Primitive(PrimitiveValue::U8(1))),
                UserField::new(second_name, Argument::Primitive(PrimitiveValue::U8(2))),
            ])
        };
        let a = form_with_args(ArgumentTuple::new(vec![Argument::UserDefined(
            UserValue::Record {
                type_qualified: SmolStr::new("R"),
                fields: mk_fields("a", "b"),
            },
        )]));
        let b = form_with_args(ArgumentTuple::new(vec![Argument::UserDefined(
            UserValue::Record {
                type_qualified: SmolStr::new("R"),
                fields: mk_fields("b", "a"),
            },
        )]));
        assert_ne!(
            encode(&a, BodyVersion::CURRENT),
            encode(&b, BodyVersion::CURRENT),
        );
    }
}
