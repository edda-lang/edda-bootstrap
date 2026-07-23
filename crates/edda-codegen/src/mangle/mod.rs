//! `storage.md` §5 mangling grammar — produces the human-readable
//! `<short-mangled-name>` half of an [`ArtifactName`].
//!
//! Per §5:
//! ```text
//! artifact-name      ::= short-mangled-name "__" hash-prefix
//! short-mangled-name ::= spec-leaf-name ( "_" mangled-arg )*
//! spec-leaf-name     ::= spec's last path component
//! mangled-arg        ::= mangled-type | mangled-effect-row | mangled-primitive | mangled-value
//! mangled-type       ::= type-leaf-name                     // "i32", "Option_i32" (recursive)
//! mangled-effect-row ::= "with" ( "_" mangled-entry )*      // canonical-row-ordered
//! mangled-entry      ::= "err_" type-leaf-name | "yield_" type-leaf-name | "panic"
//! mangled-primitive  ::= digits | "true" | "false" | quoted-utf8-bytes
//! mangled-value      ::= type-leaf-name                     // name only; hash disambiguates
//! ```
//!
//! Mangled forms are intentionally *not unique* — two distinct
//! argument tuples may produce the same `short-mangled-name`. The
//! 12-hex `hash-prefix` (computed by [`crate::compute_hash`]) is the
//! disambiguator and is appended via [`artifact_name`].

mod args;

use edda_cache::{ArtifactHash, ArtifactName, hash_bytes};
use smol_str::SmolStr;

use crate::argument::{Argument, ArgumentTuple};

use self::args::{mangle_arg_into, mangle_primitive};

const MODULE_DISAMBIG_VERSION: u8 = 0x01;

//   characters `ArtifactName::new` admits (no '/', '\\', whitespace,
//   or NUL); the source qualified name's leaf must already satisfy
//   this — the resolver upstream guarantees ASCII identifier shape
/// Mangle a spec invocation into its `<short-mangled-name>`.
///
/// `spec_qualified` is the spec's fully qualified name (dotted form);
/// only the last path component appears in the mangled output. Each
/// argument contributes one `_<mangled-arg>` suffix, in declaration
/// order.
pub fn mangle_short_name(spec_qualified: &str, args: &ArgumentTuple) -> SmolStr {
    let mut out = String::with_capacity(spec_qualified.len() + 4 * args.len());
    out.push_str(leaf_of(spec_qualified));
    for arg in args.args() {
        out.push('_');
        mangle_arg_into(arg, &mut out);
    }
    SmolStr::new(&out)
}

//   mangled short name — which would itself be a contract violation
//   upstream, not a normal failure mode
/// Compose the full [`ArtifactName`] (`<short>__<hash-prefix>`) for a
/// spec invocation.
pub fn artifact_name(
    spec_qualified: &str,
    args: &ArgumentTuple,
    hash: &ArtifactHash,
) -> Option<ArtifactName> {
    let short = mangle_short_name(spec_qualified, args);
    ArtifactName::new(short, hash.prefix())
}

//   (a single-segment qualified name's leaf IS itself)
fn leaf_of(qualified: &str) -> &str {
    qualified.rsplit('.').next().unwrap_or(qualified)
}

/// 8-hex disambiguator suffix for a spec invocation's generated module
/// leaf segment. Used by [`crate::compose_module_path`] (and the
/// resolver-side reciprocal) to distinguish two invocations that would
/// otherwise mangle to the same leaf — e.g. `eq(<module1>.BlockId)` vs
/// `eq(<module2>.BlockId)`.
pub fn module_disambig_hex(spec_qualified: &str, args: &ArgumentTuple) -> Option<String> {
    let mut s = String::with_capacity(spec_qualified.len() + 32 * args.len());
    s.push(MODULE_DISAMBIG_VERSION as char);
    // Only the spec's leaf segment goes into the hash input — the resolver-
    // side reciprocal works from `si.path.segments.last()` and would need full
    // path-resolution to recover the parent prefix, which it doesn't have at
    // `build_spec_inv_targets` time. Including only the leaf keeps the two
    // sides symmetric while still letting two `eq(<m1>.BlockId)` /
    // `eq(<m2>.BlockId)` invocations diverge through their arg qnames.
    s.push_str(leaf_of(spec_qualified));
    for arg in args.args() {
        s.push('\0');
        match arg {
            // A function arg folds in its full qualified name under the
            // same `T:` form as a type arg. The resolver-side
            // reciprocal `arg_type_qname_for_hash` resolves a function
            // path to the identical qname, so the two sides agree; a
            // function qname can never collide with a type qname (no two
            // top-level decls share a fully-qualified name), so reusing
            // the `T:` tag is sound and needs no version bump.
            Argument::Type(qname) | Argument::Function(qname) => {
                s.push_str("T:");
                s.push_str(qname.as_str());
            }
            Argument::Primitive(p) => {
                s.push_str("P:");
                mangle_primitive(p, &mut s);
            }
            // EffectRow / UserDefined would require additional resolver-side
            // canonicalisation that has not landed yet; the caller falls back
            // to leaf-only matching when we can't produce a hash here.
            Argument::EffectRow(_) | Argument::UserDefined(_) => return None,
        }
    }
    let hash = hash_bytes(s.as_bytes());
    let full = hash.to_string();
    Some(full[..8].to_string())
}

#[cfg(test)]
mod tests;
