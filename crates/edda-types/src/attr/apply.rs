//! Per-attribute appliers for the D-18 closed-nine registry.
//!
//! Each `apply_*` validates one attribute's argument shape and item-kind
//! admissibility, then folds the well-formed value into the caller's
//! [`AttrSet`]. They are the per-name arms the [`super::validate_attributes`]
//! dispatcher routes to; the shared argument-shape probes and diagnostic
//! emitters live in [`super::helpers`].

use edda_diag::{Diagnostics, LintConfig};
use edda_intern::Interner;
use edda_syntax::ast::{Attribute, AttrArg};

use crate::capability::CapabilityType;

use super::helpers::{
    attr_arg_span, emit, emit_target_mismatch, expect_reason, expect_single_arg,
    expect_single_ident, expect_single_int, expect_single_str,
};
use super::{
    AttrAbi, AttrLayout, AttrRepr, AttrSet, AttrTarget, AttrTargetRequires, AttrUnverified,
};

pub(super) fn apply_target_requires(
    attr: &Attribute,
    target: AttrTarget,
    out: &mut AttrSet,
    interner: &Interner,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    if !matches!(target, AttrTarget::Function | AttrTarget::ExternFunction) {
        emit_target_mismatch(attr, "target_requires", "function declarations", lint_cfg, diags);
        return;
    }
    let Some(arg) = expect_single_arg(attr, "target_requires", lint_cfg, diags) else {
        return;
    };
    let AttrArg::Ident(id) = arg else {
        emit(
            diags,
            lint_cfg,
            attr_arg_span(arg),
            "`@target_requires` expects a bare capability-type identifier argument".to_string(),
        );
        return;
    };
    let name = interner.resolve(id.name);
    let is_known = CapabilityType::from_name(name).is_some()
        || matches!(name, "Dom" | "Window" | "ExtensionContent" | "ExtensionWorker");
    if !is_known {
        emit(
            diags,
            lint_cfg,
            id.span,
            format!(
                "unknown capability `{}` in `@target_requires(...)` — expects a capability type name",
                name
            ),
        );
        return;
    }
    out.target_requires = Some(AttrTargetRequires {
        capability: id.name,
        attr_span: attr.span,
    });
}

pub(super) fn apply_abi(
    attr: &Attribute,
    target: AttrTarget,
    out: &mut AttrSet,
    interner: &Interner,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    if !matches!(target, AttrTarget::Function | AttrTarget::ExternFunction) {
        emit_target_mismatch(attr, "abi", "function declarations", lint_cfg, diags);
        return;
    }
    let Some(sym) = expect_single_str(attr, "abi", lint_cfg, diags) else { return };
    // `@abi` never licenses bodylessness — the parser diagnoses a
    // bodyless `@abi` function and recovers by synthesising the `Extern`
    // body from the attribute payload. An `ExternFunction` target can
    // therefore still reach here (recovery path, or `@abi` stacked on a
    // real `extern "sym"` body-form); record the attribute as consumed so
    // it doesn't trigger `unrecognised_attribute` downstream. No `AttrSet`
    // field is populated — the FFI symbol lives on the FnBody, not on the
    // attribute payload.
    if matches!(target, AttrTarget::ExternFunction) {
        let _ = sym;
        return;
    }
    let text = interner.resolve(sym);
    let abi = match text {
        "c" => AttrAbi::C,
        "system" => AttrAbi::System,
        "sysv64" => AttrAbi::SysV64,
        "win64" => AttrAbi::Win64,
        _ => {
            // On a body-bearing function, a non-catalogue string is a
            // symbol-name override — the function is emitted under this
            // verbatim linker symbol (no module-path mangle) with the
            // platform-default C convention. Duplicate overrides are
            // rejected at the driver's link-set seam, not here.
            if text.is_empty() {
                emit(
                    diags,
                    lint_cfg,
                    attr.span,
                    "`@abi(\"\")` — a symbol-name override must be a non-empty string".to_string(),
                );
                return;
            }
            out.abi_symbol = Some(sym);
            return;
        }
    };
    out.abi = Some(abi);
}

pub(super) fn apply_align(
    attr: &Attribute,
    target: AttrTarget,
    out: &mut AttrSet,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    if !matches!(target, AttrTarget::TypeDecl) {
        emit_target_mismatch(attr, "align", "type declarations", lint_cfg, diags);
        return;
    }
    let Some((value, value_span)) = expect_single_int(attr, "align", lint_cfg, diags) else {
        return;
    };
    if value == 0 || (value & (value - 1)) != 0 {
        emit(
            diags,
            lint_cfg,
            value_span,
            format!("`@align({})` must be a positive power of two", value),
        );
        return;
    }
    if value > u32::MAX as u128 {
        emit(
            diags,
            lint_cfg,
            value_span,
            format!("`@align({})` exceeds u32::MAX", value),
        );
        return;
    }
    out.align = Some(value as u32);
}

pub(super) fn apply_repr(
    attr: &Attribute,
    target: AttrTarget,
    out: &mut AttrSet,
    interner: &Interner,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    if !matches!(target, AttrTarget::TypeDecl) {
        emit_target_mismatch(attr, "repr", "type declarations", lint_cfg, diags);
        return;
    }
    let Some(name) = expect_single_ident(attr, "repr", interner, lint_cfg, diags) else {
        return;
    };
    let repr = match name {
        "Edda" => AttrRepr::Edda,
        "C" => AttrRepr::C,
        "Transparent" => AttrRepr::Transparent,
        "Simd" => AttrRepr::Simd,
        "Opaque" => AttrRepr::Opaque,
        _ => {
            emit(
                diags,
                lint_cfg,
                attr.span,
                format!(
                    "unknown repr `{}` — admitted values are `Edda`, `C`, `Transparent`, `Simd`, `Opaque`",
                    name
                ),
            );
            return;
        }
    };
    out.repr = Some(repr);
}

pub(super) fn apply_unverified(
    attr: &Attribute,
    target: AttrTarget,
    out: &mut AttrSet,
    interner: &Interner,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    if !matches!(target, AttrTarget::Function | AttrTarget::ExternFunction) {
        emit_target_mismatch(
            attr,
            "unverified",
            "function declarations",
            lint_cfg,
            diags,
        );
        return;
    }
    let Some(reason) = expect_reason(attr, "unverified", interner, lint_cfg, diags) else {
        return;
    };
    out.unverified = Some(AttrUnverified {
        reason,
        attr_span: attr.span,
    });
}

/// Validate `@trust(reason = "...")`. The reason is required and is the
/// audit surface for `edda lint --trust-points`.
pub(super) fn apply_trust(
    attr: &Attribute,
    interner: &Interner,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    let _ = expect_reason(attr, "trust", interner, lint_cfg, diags);
}

/// Validate `@deprecated(reason = "...")` — marks an item deprecated
/// (`migration.md`).
pub(super) fn apply_deprecated(
    attr: &Attribute,
    interner: &Interner,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    let _ = expect_reason(attr, "deprecated", interner, lint_cfg, diags);
}

pub(super) fn apply_layout(
    attr: &Attribute,
    target: AttrTarget,
    out: &mut AttrSet,
    interner: &Interner,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    if !matches!(target, AttrTarget::TypeDecl) {
        emit_target_mismatch(attr, "layout", "type declarations", lint_cfg, diags);
        return;
    }
    let Some(name) = expect_single_ident(attr, "layout", interner, lint_cfg, diags) else {
        return;
    };
    let layout = match name {
        "natural" => AttrLayout::Natural,
        "declared" => AttrLayout::Declared,
        "sorted" => AttrLayout::Sorted,
        "packed" => AttrLayout::Packed,
        _ => {
            emit(
                diags,
                lint_cfg,
                attr.span,
                format!(
                    "unknown layout `{}` — admitted values are `natural`, `declared`, `sorted`, `packed`",
                    name
                ),
            );
            return;
        }
    };
    out.layout = Some(layout);
}
