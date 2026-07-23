//! Item-level attribute validation (B2 slice 2).
//!
//! `@name(args)` attributes on a source [`edda_syntax::ast::Item`] are
//! parsed by `edda-syntax` without semantic interpretation. This module
//! owns the locked registry — the set of attribute names, their
//! admitted argument shapes, and the item-kind families each may
//! attach to — and the validation pass that lifts a parsed attribute
//! list into a typed [`AttrSet`].
//!
//! # Locked registry — the D-18 closed nine
//!
//! The `@`-namespace is a closed whitelist of exactly nine names; any
//! other `@name` is a hard `unknown_attribute` error.
//!
//! | Name              | Arg shape                                           | Admitted on        |
//! |-------------------|-----------------------------------------------------|--------------------|
//! | `@layout`         | `(Ident)` — `natural | declared | sorted | packed`  | TypeDecl           |
//! | `@align`          | `(Int)` — positive power of two ≤ u32::MAX          | TypeDecl           |
//! | `@repr`           | `(Ident)` — `Edda | C | Transparent | Simd | Opaque` | TypeDecl           |
//! | `@abi`            | `(Str)` — catalogue CC, or symbol override (Edda body) | Function       |
//! | `@unverified`     | `(reason = Str)` — non-empty                        | Function           |
//! | `@trust`          | `(reason = Str)` — non-empty                        | any (name-only)    |
//! | `@deprecated`     | `(reason = Str)` — non-empty                        | any (name-only)    |
//! | `@property`       | (arg shape not yet locked — accepted name-only)     | any (name-only)    |
//! | `@target_requires`| `(Ident)` — a capability type name                  | Function           |
//!
//! `@trust` / `@deprecated` / `@property` are name-accepted but carry no
//! [`AttrSet`] payload yet — MIR threading and backend emission for them
//! are deferred past this sterility pass. `@target_requires(T)` is
//! validated against the capability-name catalogue (the locked 18 plus
//! the four experimental browser/WebExtension names) and populates
//! [`AttrSet::target_requires`]; `check_package` consults it, per
//! [`edda_target::TargetTriple::supports_capability`], to skip lowering
//! a gated-absent function's body and to diagnose call sites that still
//! reference it.
//!
//! Stability is **not** an attribute: per §3.7 / D-19 it is the
//! `stable` / `unstable` keyword on `function` and `type` declarations.
//! `@stable` / `@unstable` therefore reject as `unknown_attribute`.
//!
//! Unknown attribute names emit [`DiagnosticClass::UnknownAttribute`]
//! (`error[unknown_attribute]`) with a message naming the closed nine,
//! matching the native compiler verbatim. Wrong argument shapes,
//! out-of-value-set choices, and wrong item-kind admissions emit
//! [`DiagnosticClass::TypecheckError`] with a precise message. Names
//! that arrive parsed-cleanly but fail validation are silently dropped
//! from the resulting [`AttrSet`].
//!
//! `@unverified` is the function-level trust hatch from
//! `corpus/edda-codex/language/03-verification.md` §9: every obligation
//! inside the function is admitted without SMT discharge. The reason
//! string is the audit surface (`edda lint --trust-points`). When a
//! function is also declared `stable function`, the stability pass
//! rejects this combination with `stability_unverified` per §7.
//!
//! Adding a new attribute name (or admitting an existing one on a new
//! item kind) is a spec move that requires updating this registry, the
//! MIR threading layer (B2 slice 3), and any backend emission (slice 4).
//!
//! # Module layout
//!
//! - This file ([`attr`](self)) — the locked attribute-target / value-set
//!   types ([`AttrTarget`], [`AttrAbi`], [`AttrLayout`], [`AttrRepr`],
//!   [`AttrUnverified`], [`AttrSet`]) and the [`validate_attributes`]
//!   dispatcher.
//! - [`apply`] — the per-attribute appliers ([`apply::apply_abi`] et al.)
//!   that validate one attribute's arg shape + admissibility and fold the
//!   result into the [`AttrSet`].
//! - [`helpers`] — the argument-shape probes ([`helpers::expect_single_str`]
//!   et al.) and the diagnostic emitters shared across the appliers.

use edda_intern::Interner;
use edda_span::Span;
use edda_syntax::ast::Attribute;

mod apply;
mod helpers;

use apply::{
    apply_abi, apply_align, apply_deprecated, apply_layout, apply_repr, apply_target_requires,
    apply_trust, apply_unverified,
};
use helpers::emit_unknown_attribute;

use edda_diag::{Diagnostics, LintConfig};
use edda_intern::Symbol;

/// Discriminator for which item-kind family the attributes attach to.
///
/// Function items admit `@abi` / `@unverified` / `@target_requires`;
/// type-decl items admit `@align` / `@repr` / `@layout`. The name-only
/// members of the closed nine (`@trust` / `@deprecated` / `@property`)
/// are accepted regardless of target — they carry no [`AttrSet`] payload
/// and so impose no item-kind admission yet. Every attribute name
/// *outside* the closed nine is rejected here as `unknown_attribute`.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum AttrTarget {
    /// `function ...` declaration with an Edda-side body. `@abi("name")`
    /// names a calling convention from the locked catalogue (`"c"`,
    /// `"system"`, `"sysv64"`, `"win64"`), or — for any other non-empty
    /// string — a verbatim linker-symbol override.
    Function,
    /// FFI `function ...` declaration whose body slot is the
    /// `extern "symbol"` body-form — an
    /// `@abi("symbol")` here names the linker-visible symbol (any
    /// non-empty identifier-like string), not a calling convention.
    /// Same attribute family otherwise.
    ExternFunction,
    /// `type ...` declaration.
    TypeDecl,
    /// Any other item kind — admits no attributes.
    Other,
}

/// Locked `@abi("...")` value set.
///
/// The string spellings the user writes in source are matched
/// case-sensitively. The enum variants map onto LLVM calling
/// conventions at the codegen seam in B2 slice 4.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum AttrAbi {
    /// `"c"` — platform default C calling convention.
    C,
    /// `"system"` — platform default (synonym for the ABI tag
    /// `AbiTag::System`).
    System,
    /// `"sysv64"` — System V AMD64 (x86_64 only).
    SysV64,
    /// `"win64"` — Microsoft x64 (x86_64 only).
    Win64,
}

/// Locked `@layout(...)` policy set.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum AttrLayout {
    /// `natural` — fields in declaration order, default padding.
    Natural,
    /// `declared` — fields in declaration order, alignment respected
    /// explicitly.
    Declared,
    /// `sorted` — fields reordered by size to minimise padding.
    Sorted,
    /// `packed` — fields adjacent with no padding.
    Packed,
}

/// Locked `@repr(...)` kind set.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum AttrRepr {
    /// `Edda` — native Edda layout (the default).
    Edda,
    /// `C` — layout matches the platform C ABI.
    C,
    /// `Transparent` — single-field newtype shares its inner type's
    /// representation.
    Transparent,
    /// `Simd` — SIMD vector layout (vector ABI).
    Simd,
    /// `Opaque` — layout is hidden from comptime introspection.
    Opaque,
}

/// `@unverified(reason = "...")` payload — function-level trust hatch.
///
/// Captures the user-supplied reason string and the source position of
/// the annotation itself. Routed through [`AttrSet::unverified`]; the
/// stability pass reads this field to emit `stability_unverified` when
/// the same function is declared `stable function`.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct AttrUnverified {
    /// Interned non-empty `reason` string.
    pub reason: Symbol,
    /// Source range covering the entire `@unverified(...)` clause.
    pub attr_span: Span,
}

/// `@target_requires(T)` payload — whole-function per-target gate.
///
/// `T` is the source spelling of a capability type name. The function
/// does not exist on a build target that does not support `T`.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct AttrTargetRequires {
    /// Interned capability-name spelling, as written in source.
    pub capability: Symbol,
    /// Source range covering the entire `@target_requires(...)` clause.
    pub attr_span: Span,
}

/// Typed attribute payload for one item, produced by
/// [`validate_attributes`].
#[derive(Copy, Clone, Eq, PartialEq, Debug, Default)]
pub struct AttrSet {
    /// `@abi("...")` — locked C-ABI synonyms.
    pub abi: Option<AttrAbi>,
    /// `@abi("symbol")` with a non-catalogue string on a body-bearing
    /// function — the verbatim linker symbol the function exports.
    /// Implies the platform-default C
    /// convention; mutually exclusive with `abi`.
    pub abi_symbol: Option<Symbol>,
    /// `@align(N)` — alignment in bytes (positive power of two).
    pub align: Option<u32>,
    /// `@repr(Kind)` — representation policy.
    pub repr: Option<AttrRepr>,
    /// `@layout(Policy)` — layout policy.
    pub layout: Option<AttrLayout>,
    /// `@unverified(reason = "...")` — function-level trust hatch.
    pub unverified: Option<AttrUnverified>,
    /// `@target_requires(T)` — whole-function per-target gate.
    pub target_requires: Option<AttrTargetRequires>,
}

impl AttrSet {
    /// `true` when no attribute field has been populated. Equivalent to
    /// `self == &AttrSet::default()`. Used by the typecheck driver to
    /// decide whether to record this set in the per-package
    /// `BindingId → AttrSet` map.
    pub fn is_empty(&self) -> bool {
        self.abi.is_none()
            && self.abi_symbol.is_none()
            && self.align.is_none()
            && self.repr.is_none()
            && self.layout.is_none()
            && self.unverified.is_none()
            && self.target_requires.is_none()
    }
}

/// Validate the attribute list attached to an item and return its
/// typed [`AttrSet`].
///
/// Emits one [`DiagnosticClass::TypecheckError`] per failure (unknown
/// name, wrong arg shape, wrong item kind, out-of-value-set choice).
/// Successful entries are folded into the returned [`AttrSet`];
/// failures leave the relevant field `None` and the value-set survives
/// the call so downstream passes can still inspect every well-formed
/// attribute.
pub fn validate_attributes(
    attrs: &[Attribute],
    target: AttrTarget,
    interner: &Interner,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) -> AttrSet {
    let mut out = AttrSet::default();
    for attr in attrs {
        // try_resolve, not resolve: parser recovery can leave a
        // `Symbol::DUMMY` attribute name, and `resolve` panics on the
        // sentinel. The malformed
        // attribute was already diagnosed at parse time, so skip it.
        let Some(name) = interner.try_resolve(attr.name.name) else {
            continue;
        };
        match name {
            "abi" => apply_abi(attr, target, &mut out, interner, lint_cfg, diags),
            "align" => apply_align(attr, target, &mut out, lint_cfg, diags),
            "repr" => apply_repr(attr, target, &mut out, interner, lint_cfg, diags),
            "layout" => apply_layout(attr, target, &mut out, interner, lint_cfg, diags),
            "unverified" => {
                apply_unverified(attr, target, &mut out, interner, lint_cfg, diags)
            }
            "trust" => apply_trust(attr, interner, lint_cfg, diags),
            "deprecated" => apply_deprecated(attr, interner, lint_cfg, diags),
            "target_requires" => {
                apply_target_requires(attr, target, &mut out, interner, lint_cfg, diags)
            }
            // `@property` is in the closed-nine but carries no [`AttrSet`]
            // payload yet: its arg shape is accepted name-only here.
            "property" => {}
            // Everything else is rejected against the D-18 closed-nine —
            // including `@stable` / `@unstable` (D-19: stability is the
            // `stable` / `unstable` keyword, not an attribute) and bogus
            // annotations (`@invariant`, `@pattern`, `@note`, `@internal`, ...).
            _ => emit_unknown_attribute(diags, lint_cfg, attr.name.span),
        }
    }
    out
}
