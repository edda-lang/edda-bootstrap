//! Per-target capability availability check.
//!
//! Runs after [`check_package`](super::check_package) returns a
//! [`TypedPackage`](super::TypedPackage). Walks every recorded function
//! signature's parameter types and return type; any
//! [`TyKind::Capability(c)`](crate::TyKind::Capability) whose
//! `c.name()` is not in
//! [`edda_target::TargetTriple::supports_capability`]'s admit-list for
//! the current target emits a
//! [`DiagnosticClass::CapabilityNotAvailableOnTarget`] diagnostic.
//!
//! Implements Model 1 from `corpus/edda-codex/language/effects.md`
//! § per-target capabilities: a function whose signature names a cap
//! the target does not provide is refused at typecheck time. The codex
//! lock and the cap-availability table live in
//! `edda_target::TargetTriple::supports_capability`; this module is the
//! per-package walker that feeds locked cap names into that predicate.
//!
//! The walk is recursive through `Slice` / `Tuple` / `FnPtr` so a
//! `[Subprocess]` slice parameter, or a `function(Subprocess) -> ()`
//! callback parameter, still surfaces the diagnostic.

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_span::Span;
use edda_target::TargetTriple;

use crate::sig::FnSig;
use crate::ty::{TyId, TyInterner, TyKind};

use super::TypedPackage;

/// Drive the per-target capability-availability check across a
/// type-checked package.
///
/// For every function signature recorded in
/// [`TypedPackage::ty_cx`](super::TypedPackage::ty_cx), walks the
/// parameter types and the return type. Each
/// [`TyKind::Capability(c)`](crate::TyKind::Capability) is looked up
/// via [`TargetTriple::supports_capability`]; on `false` a
/// [`DiagnosticClass::CapabilityNotAvailableOnTarget`] diagnostic is
/// emitted at the parameter's declaration span.
///
/// The walker descends into `Slice`, `Tuple`, and `FnPtr` payloads so
/// nested cap occurrences are caught.
pub fn check_capability_availability(
    package: &TypedPackage,
    ty_interner: &TyInterner,
    target: &TargetTriple,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    for (_, sig) in package.ty_cx.iter_sigs() {
        check_signature(sig, ty_interner, target, lint_cfg, diags);
    }
}

fn check_signature(
    sig: &FnSig,
    ty_interner: &TyInterner,
    target: &TargetTriple,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    for param in sig.params.iter() {
        walk_ty(param.ty, param.span, ty_interner, target, lint_cfg, diags);
    }
    // Return-type span falls back to the signature's first-parameter
    // span when present, or `Span::DUMMY` otherwise. Capability returns
    // are unusual in practice — there are no constructors that yield
    // a bare capability value — but the check is structurally complete.
    let return_span = sig
        .params
        .first()
        .map(|p| p.span)
        .unwrap_or(Span::DUMMY);
    walk_ty(sig.return_ty, return_span, ty_interner, target, lint_cfg, diags);
}

fn walk_ty(
    ty: TyId,
    span: Span,
    ty_interner: &TyInterner,
    target: &TargetTriple,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    match ty_interner.kind(ty) {
        TyKind::Capability(cap) => {
            if !target.supports_capability(cap.name()) {
                emit_unavailable(*cap, span, target, lint_cfg, diags);
            }
        }
        TyKind::Slice(elem) => {
            walk_ty(*elem, span, ty_interner, target, lint_cfg, diags);
        }
        TyKind::Tuple(elements) => {
            for elem in elements.iter() {
                walk_ty(*elem, span, ty_interner, target, lint_cfg, diags);
            }
        }
        TyKind::FnPtr(fn_sig) => {
            for fp_param in fn_sig.params.iter() {
                walk_ty(fp_param.ty, span, ty_interner, target, lint_cfg, diags);
            }
            walk_ty(fn_sig.return_ty, span, ty_interner, target, lint_cfg, diags);
        }
        TyKind::Primitive(_) | TyKind::Nominal(_) | TyKind::Error => {}
    }
}

fn emit_unavailable(
    cap: crate::capability::CapabilityType,
    span: Span,
    target: &TargetTriple,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    let sev = lint_cfg.effective(DiagnosticClass::CapabilityNotAvailableOnTarget);
    let message = format!(
        "'{}' capability is not available on target `{}`",
        cap.name(),
        target
    );
    diags.push(Diagnostic::new(
        DiagnosticClass::CapabilityNotAvailableOnTarget,
        sev,
        span,
        message,
    ));
}
