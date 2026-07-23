//! Convert codegen hash-input types ([`CanonicalForm`], [`Argument`])
//! into the cache layer's manifest and header types
//! ([`ArtifactInputs`], [`ArgumentEntry`], [`ArtifactHeader`]).
//!
//! Pure functions; no I/O. The session in [`crate::cascade`] composes
//! these into the staging-handle write path.
//!
//! # `value` strings are inspectable, not load-bearing
//!
//! [`ArgumentEntry::value`] is a [`SmolStr`] the cache layer treats as
//! opaque — it round-trips through the TOON writer but is never
//! re-parsed for semantic meaning. Format choices in this module are
//! therefore free to optimise for *human inspection* of the manifest;
//! the artifact's content-hash is the disambiguator the build relies
//! on.
//!
//! # `nested_short_names` rationale
//!
//! [`crate::canonical::NestedDependency`] stores only
//! `(qualified, argument_tuple_hash)` — the short name (mangled form)
//! is *not* recoverable from those two fields alone. The session API
//! therefore takes `nested_short_names` separately and the caller is
//! responsible for supplying them; in driver use the caller already
//! produced or located each nested artifact and knows its short name.

use edda_cache::{
    ArgumentEntry, ArgumentKind, ArtifactHash, ArtifactHeader, ArtifactInputs, BodyVersion,
    NestedDep,
};
use smol_str::SmolStr;
use std::fmt::Write as _;

use crate::argument::{
    Argument, EffectRowArgument, EffectRowEntry, PrimitiveValue, UserField, UserValue,
    UserVariantPayload,
};
use crate::canonical::CanonicalForm;

/// Build an [`ArtifactInputs`] record for the manifest entry of the
/// artifact whose hash inputs are `form`.
///
/// `nested_short_names` is the **manifest-side** representation of
/// `form.nested`: each nested invocation contributes its short name
/// (mangled form). See the module-level docs for why this is supplied
/// separately rather than derived from `form`.
pub fn to_artifact_inputs(
    form: &CanonicalForm,
    body_version: BodyVersion,
    nested_short_names: &[SmolStr],
) -> ArtifactInputs {
    let argument_tuple = form
        .argument_tuple
        .args()
        .iter()
        .map(to_argument_entry)
        .collect();
    ArtifactInputs {
        body_version,
        spec_qualified_name: form.spec_qualified.clone(),
        argument_tuple,
        nested_deps: nested_short_names.to_vec(),
    }
}

/// Build one manifest [`ArgumentEntry`] for an [`Argument`].
///
/// The `value` field is a human-readable printable form; see the
/// module-level docs for what it is and is not used for.
pub fn to_argument_entry(arg: &Argument) -> ArgumentEntry {
    match arg {
        Argument::Type(qname) => ArgumentEntry {
            kind: ArgumentKind::Type,
            value: qname.clone(),
        },
        Argument::EffectRow(row) => ArgumentEntry {
            kind: ArgumentKind::EffectRow,
            value: format_effect_row(row),
        },
        Argument::Primitive(p) => ArgumentEntry {
            kind: ArgumentKind::PrimitiveValue,
            value: format_primitive(p),
        },
        Argument::UserDefined(v) => ArgumentEntry {
            kind: ArgumentKind::UserValue,
            value: format_user_value(v),
        },
        Argument::Function(qname) => ArgumentEntry {
            kind: ArgumentKind::Function,
            value: qname.clone(),
        },
    }
}

/// Build an [`ArtifactHeader`] for an artifact.
///
/// `spec_invocation` is the **display** form
/// (`"std.option.Option(i32)"`); it appears in the header's `spec:`
/// field verbatim and is not parsed by readers. `hash` is the full
/// BLAKE3 hash from [`crate::compute_hash`]. `nested_for_header` is
/// the caller-resolved nested-dependency list in the cache layer's
/// shape; see the module-level docs.
pub fn to_artifact_header(
    spec_invocation: &str,
    body_version: BodyVersion,
    hash: &ArtifactHash,
    generator: &str,
    nested_for_header: &[NestedDep],
) -> ArtifactHeader {
    ArtifactHeader {
        spec: SmolStr::new(spec_invocation),
        hash: *hash,
        generator: SmolStr::new(generator),
        body_version,
        nested: nested_for_header.to_vec(),
    }
}

/// Format an effect-row argument as a brace-wrapped, comma-separated
/// list for manifest readability: `{err: AllocError, panic, yield: i32}`.
fn format_effect_row(row: &EffectRowArgument) -> SmolStr {
    let mut out = String::with_capacity(2 + row.len() * 12);
    out.push('{');
    let mut first = true;
    for entry in row.entries() {
        if !first {
            out.push_str(", ");
        }
        first = false;
        match entry {
            EffectRowEntry::Err(ty) => {
                out.push_str("err: ");
                out.push_str(ty);
            }
            EffectRowEntry::Panic => out.push_str("panic"),
            EffectRowEntry::Yield(ty) => {
                out.push_str("yield: ");
                out.push_str(ty);
            }
        }
    }
    out.push('}');
    SmolStr::new(&out)
}

/// Format a primitive comptime argument as its printable value.
fn format_primitive(p: &PrimitiveValue) -> SmolStr {
    match p {
        PrimitiveValue::U8(v) => SmolStr::new(v.to_string()),
        PrimitiveValue::U16(v) => SmolStr::new(v.to_string()),
        PrimitiveValue::U32(v) => SmolStr::new(v.to_string()),
        PrimitiveValue::U64(v) => SmolStr::new(v.to_string()),
        PrimitiveValue::USize(v) => SmolStr::new(v.to_string()),
        PrimitiveValue::I8(v) => SmolStr::new(v.to_string()),
        PrimitiveValue::I16(v) => SmolStr::new(v.to_string()),
        PrimitiveValue::I32(v) => SmolStr::new(v.to_string()),
        PrimitiveValue::I64(v) => SmolStr::new(v.to_string()),
        PrimitiveValue::ISize(v) => SmolStr::new(v.to_string()),
        PrimitiveValue::Bool(v) => SmolStr::new(if *v { "true" } else { "false" }),
        PrimitiveValue::String(s) => SmolStr::new(format!("{:?}", s.as_str())),
    }
}

/// Format a user-defined comptime argument as `TypeQName{field: value, ...}`
/// or `TypeQName.variant(...)` for manifest readability.
fn format_user_value(v: &UserValue) -> SmolStr {
    let mut out = String::new();
    match v {
        UserValue::Record { type_qualified, fields } => {
            out.push_str(type_qualified);
            out.push('{');
            write_fields(&mut out, fields);
            out.push('}');
        }
        UserValue::Variant {
            type_qualified,
            variant_tag,
            payload,
        } => {
            out.push_str(type_qualified);
            out.push('.');
            out.push_str(variant_tag);
            match payload {
                UserVariantPayload::Unit => {}
                UserVariantPayload::Tuple(items) => {
                    out.push('(');
                    let mut first = true;
                    for item in items.iter() {
                        if !first {
                            out.push_str(", ");
                        }
                        first = false;
                        let _ = write!(out, "{}", to_argument_entry(item).value);
                    }
                    out.push(')');
                }
                UserVariantPayload::Struct(fields) => {
                    out.push('{');
                    write_fields(&mut out, fields);
                    out.push('}');
                }
            }
        }
    }
    SmolStr::new(&out)
}

/// Append a `name: value, ...` field sequence to `out`. Used by both
/// record and struct-variant payloads.
fn write_fields(out: &mut String, fields: &[UserField]) {
    let mut first = true;
    for f in fields {
        if !first {
            out.push_str(", ");
        }
        first = false;
        out.push_str(&f.name);
        out.push_str(": ");
        let _ = write!(out, "{}", to_argument_entry(&f.value).value);
    }
}
