//! Per-argument mangling helpers for the `storage.md` §5 short name.
//!
//! Split out from `mangle/mod.rs` for file-size reasons. These helpers
//! render one [`Argument`] into the running short-mangled-name buffer:
//! type leaves (strip-lowercase rule), effect rows (canonical order),
//! primitives, and user-defined values. The byte forms are locked in
//! lock-step with the resolver-side mangler — do not alter them.

use std::fmt::Write;

use crate::argument::{
    Argument, EffectRowArgument, EffectRowEntry, PrimitiveValue, UserValue,
};

use super::leaf_of;

/// Mangle a type's qualified name into its `mangled-type` component
/// per `storage.md` §5. The historical `leaf_of` form (last segment
/// only) collides when two nested-spec args share a leaf
/// (`Vec(Vec_String.Vec)` vs `Vec(Vec_usize.Vec)`); this form keeps the
/// spec-mangled portion so the two stay distinct, while still
/// collapsing module-qualified args (`kind_mod.Token`,
/// `repro.tokens.kind.Token`) to their leaf-only canonical form
/// (`Token`) so non-colliding multi-segment invocations stay aligned
/// with the resolver-side mangler in `edda-resolve::items::arg_leaf_name`.
pub(super) fn type_leaf_mangle(qualified: &str) -> String {
    let mut segments: Vec<&str> = qualified.split('.').collect();
    if segments.is_empty() {
        return String::new();
    }
    // Strip leading lowercase-Edda-module segments (e.g. `std.collections.vec`).
    while segments.len() > 1 {
        let head = segments[0];
        match head.chars().next() {
            Some(c) if c.is_ascii_lowercase() => {
                segments.remove(0);
            }
            _ => break,
        }
    }
    segments.join("_")
}

pub(super) fn mangle_arg_into(arg: &Argument, out: &mut String) {
    match arg {
        Argument::Type(qname) => out.push_str(&type_leaf_mangle(qname)),
        Argument::EffectRow(row) => mangle_effect_row(row, out),
        Argument::Primitive(p) => mangle_primitive(p, out),
        Argument::UserDefined(v) => out.push_str(&type_leaf_mangle(user_type_qname(v))),
        // A function arg mangles to its leaf via the same
        // strip-lowercase rule as a type, so it agrees byte-for-byte
        // with the resolver-side `arg_leaf_name` for the same arg path.
        Argument::Function(qname) => out.push_str(&type_leaf_mangle(qname)),
    }
}

//   does not re-sort; [`EffectRowArgument::new`] guarantees this
fn mangle_effect_row(row: &EffectRowArgument, out: &mut String) {
    out.push_str("with");
    for entry in row.entries() {
        out.push('_');
        match entry {
            EffectRowEntry::Err(ty) => {
                out.push_str("err_");
                out.push_str(leaf_of(ty));
            }
            EffectRowEntry::Panic => out.push_str("panic"),
            EffectRowEntry::Yield(ty) => {
                out.push_str("yield_");
                out.push_str(leaf_of(ty));
            }
        }
    }
}

//   [`ArtifactName::new`] admits); positive forms are bare decimal
//   distinct unsafe-string values within a project share a mangled
//   form, and the artifact hash prefix is the disambiguator
pub(super) fn mangle_primitive(p: &PrimitiveValue, out: &mut String) {
    match p {
        PrimitiveValue::U8(v) => write!(out, "{v}").unwrap(),
        PrimitiveValue::U16(v) => write!(out, "{v}").unwrap(),
        PrimitiveValue::U32(v) => write!(out, "{v}").unwrap(),
        PrimitiveValue::U64(v) => write!(out, "{v}").unwrap(),
        PrimitiveValue::USize(v) => write!(out, "{v}").unwrap(),
        PrimitiveValue::I8(v) => write!(out, "{v}").unwrap(),
        PrimitiveValue::I16(v) => write!(out, "{v}").unwrap(),
        PrimitiveValue::I32(v) => write!(out, "{v}").unwrap(),
        PrimitiveValue::I64(v) => write!(out, "{v}").unwrap(),
        PrimitiveValue::ISize(v) => write!(out, "{v}").unwrap(),
        PrimitiveValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        PrimitiveValue::String(s) => {
            if is_safe_identifier(s.as_str()) {
                out.push_str(s.as_str());
            } else {
                out.push_str("string");
            }
        }
    }
}

fn user_type_qname(v: &UserValue) -> &str {
    match v {
        UserValue::Record { type_qualified, .. } => type_qualified.as_str(),
        UserValue::Variant { type_qualified, .. } => type_qualified.as_str(),
    }
}

//   character is ASCII alphanumeric or `_` — exactly the set safe to
//   embed verbatim in a filename
fn is_safe_identifier(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}
