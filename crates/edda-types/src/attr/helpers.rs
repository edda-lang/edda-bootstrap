//! Argument-shape probes and diagnostic emitters shared by the appliers.
//!
//! The `expect_*` helpers validate one attribute's argument shape (single
//! string / integer / bare identifier, or the shared `reason = "..."`
//! named argument) and return the extracted value or push a precise
//! diagnostic. The `emit*` helpers centralise the `TypecheckError` /
//! `UnknownAttribute` diagnostic construction the appliers and the
//! [`super::validate_attributes`] dispatcher share.

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_intern::{Interner, Symbol};
use edda_span::Span;
use edda_syntax::ast::{AttrArg, AttrLit, Attribute};

// === Argument-shape helpers ===========================================

pub(super) fn expect_single_str(
    attr: &Attribute,
    name: &str,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) -> Option<Symbol> {
    let arg = expect_single_arg(attr, name, lint_cfg, diags)?;
    match arg {
        AttrArg::Lit {
            lit: AttrLit::Str(sym),
            ..
        } => Some(*sym),
        _ => {
            emit(
                diags,
                lint_cfg,
                attr_arg_span(arg),
                format!("`@{}` expects a string literal argument", name),
            );
            None
        }
    }
}

pub(super) fn expect_single_int(
    attr: &Attribute,
    name: &str,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) -> Option<(u128, Span)> {
    let arg = expect_single_arg(attr, name, lint_cfg, diags)?;
    match arg {
        AttrArg::Lit {
            lit: AttrLit::Int { value, .. },
            span,
        } => Some((*value, *span)),
        _ => {
            emit(
                diags,
                lint_cfg,
                attr_arg_span(arg),
                format!("`@{}` expects an integer literal argument", name),
            );
            None
        }
    }
}

pub(super) fn expect_single_ident<'a>(
    attr: &'a Attribute,
    name: &str,
    interner: &'a Interner,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) -> Option<&'a str> {
    let arg = expect_single_arg(attr, name, lint_cfg, diags)?;
    match arg {
        AttrArg::Ident(id) => Some(interner.resolve(id.name)),
        _ => {
            emit(
                diags,
                lint_cfg,
                attr_arg_span(arg),
                format!("`@{}` expects a bare identifier argument", name),
            );
            None
        }
    }
}

pub(super) fn expect_single_arg<'a>(
    attr: &'a Attribute,
    name: &str,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) -> Option<&'a AttrArg> {
    match attr.args.as_slice() {
        [arg] => Some(arg),
        args => {
            emit(
                diags,
                lint_cfg,
                attr.span,
                format!(
                    "`@{}` expects exactly one argument, got {}",
                    name,
                    args.len()
                ),
            );
            None
        }
    }
}

pub(super) fn attr_arg_span(arg: &AttrArg) -> Span {
    match arg {
        AttrArg::Ident(id) => id.span,
        AttrArg::Lit { span, .. } => *span,
        AttrArg::Named { span, .. } => *span,
    }
}

pub(super) fn emit_target_mismatch(
    attr: &Attribute,
    name: &str,
    admitted_on: &str,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) {
    emit(
        diags,
        lint_cfg,
        attr.span,
        format!("`@{}` is admitted only on {}", name, admitted_on),
    );
}

pub(super) fn emit(diags: &mut Diagnostics, lint_cfg: &LintConfig, span: Span, msg: String) {
    let sev = lint_cfg.effective(DiagnosticClass::TypecheckError);
    diags.push(Diagnostic::new(
        DiagnosticClass::TypecheckError,
        sev,
        span,
        msg,
    ));
}

/// Emit the dedicated `unknown_attribute` diagnostic (D-18). The
/// `@`-namespace is a closed whitelist of nine; everything else — bogus
/// annotations and the retired `@stable` / `@unstable` attribute forms
/// (D-19) — is rejected here.
pub(super) fn emit_unknown_attribute(diags: &mut Diagnostics, lint_cfg: &LintConfig, span: Span) {
    let sev = lint_cfg.effective(DiagnosticClass::UnknownAttribute);
    diags.push(Diagnostic::new(
        DiagnosticClass::UnknownAttribute,
        sev,
        span,
        "unknown attribute — the `@`-namespace is a closed whitelist (`@layout @align @repr @abi @unverified @trust @deprecated @property @target_requires`); invariants go in `where`/`requires`/`ensures`, patterns in a `spec`, stability in the `stable`/`unstable` keywords".to_string(),
    ));
}

/// Validate the shared `reason = "..."` named-argument shape and return
/// the interned non-empty reason symbol on success; emits a precise
/// diagnostic and returns `None` for every other shape.
pub(super) fn expect_reason(
    attr: &Attribute,
    name: &str,
    interner: &Interner,
    lint_cfg: &LintConfig,
    diags: &mut Diagnostics,
) -> Option<Symbol> {
    let arg = expect_single_arg(attr, name, lint_cfg, diags)?;
    let (reason_sym, reason_span) = match arg {
        AttrArg::Named { key, value, .. } => {
            let key_name = interner.resolve(key.name);
            if key_name != "reason" {
                emit(
                    diags,
                    lint_cfg,
                    key.span,
                    format!(
                        "`@{}` expects a single named argument `reason = \"...\"`, got `{}`",
                        name, key_name
                    ),
                );
                return None;
            }
            match value.as_ref() {
                AttrArg::Lit {
                    lit: AttrLit::Str(sym),
                    span,
                } => (*sym, *span),
                other => {
                    emit(
                        diags,
                        lint_cfg,
                        attr_arg_span(other),
                        format!("`@{}` expects `reason = \"<string literal>\"`", name),
                    );
                    return None;
                }
            }
        }
        _ => {
            emit(
                diags,
                lint_cfg,
                attr_arg_span(arg),
                format!("`@{}` expects a single named argument `reason = \"...\"`", name),
            );
            return None;
        }
    };
    if interner.resolve(reason_sym).is_empty() {
        emit(
            diags,
            lint_cfg,
            reason_span,
            format!(
                "`@{}` requires a non-empty `reason` string — the reason is the audit surface for `edda lint --trust-points`",
                name
            ),
        );
        return None;
    }
    Some(reason_sym)
}
