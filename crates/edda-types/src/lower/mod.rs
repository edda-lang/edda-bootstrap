//! AST → typed-form lowering passes.
//!
//! Every public lowering entry point follows the same shape:
//!
//! ```ignore
//! pub fn lower_<thing>(
//!     thing: &ast::<Thing>,
//!     cx: &LowerCx<'_>,
//!     diags: &mut Diagnostics,
//!     lint_cfg: &LintConfig,
//! ) -> <Lowered>;
//! ```
//!
//! A small read-only [`LowerCx`] bundles the interners (mirroring
//! `edda_resolve::ResolveCx`); the mutable diagnostics take and the
//! lint configuration travel as separate parameters so the driver can
//! rebuild diagnostics per pass without rebuilding the context.
//!
//! Submodules:
//!
//! - [`ty`] — `lower_type`: `ast::Type` → [`TyId`](crate::TyId).
//! - [`row`] — `lower_effect_row`: `ast::EffectRow` → [`EffectRow`](crate::EffectRow).
//! - [`sig`] — `lower_fn_sig`: `ast::FnDecl` header → [`FnSig`](crate::FnSig).
//! - [`hir`] — `lower_expr` / `lower_stmt` / `lower_pat` /
//!   `lower_block`: structural `ast::*` → HIR mapping.

mod hir;
mod package;
mod row;
mod sig;
mod ty;

pub(crate) use hir::lower_expr;
pub(crate) use package::lower_type_decl;
pub(crate) use sig::lower_fn_sig;
pub(crate) use ty::lower_type;

pub(crate) use hir::lower_block;
pub(crate) use package::build_ty_cx;
pub(crate) use row::lower_effect_row;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_intern::Interner;
use edda_resolve::ResolvedPackage;
use edda_span::Span;

use crate::ty::TyInterner;

/// Read-only context every lowering pass consumes.
///
/// Mirrors `edda_resolve::ResolveCx`: bundles only the `&`-references
/// the passes read. The mutable take ([`Diagnostics`]) and the
/// configuration ([`LintConfig`]) travel as separate parameters so the
/// driver can rebuild the diagnostics per pass without rebuilding the
/// read-only context.
pub(crate) struct LowerCx<'a> {
    /// String interner — used to recover identifier text from `Symbol`
    /// handles so passes can compare against the locked primitive /
    /// pure-effect-kind catalogue.
    pub interner: &'a Interner,
    /// Type interner — holds the canonical `TyKind`s and issues the
    /// `TyId`s the lowering passes return.
    pub ty_interner: &'a TyInterner,
    /// Optional resolved-package context. `Some` once `edda-resolve`
    /// has run; needed by [`lower_type`] to resolve multi-segment Path
    /// type expressions (e.g. `std.option.Option`) into nominal
    /// [`crate::TyKind::Nominal`] handles via [`ResolvedPackage::resolutions`].
    /// `None` is permitted for unit-test lowering scenarios that
    /// pre-date resolution; in that mode multi-segment paths emit the
    /// pre-existing "qualified-path expressions not yet inferred"
    /// diagnostic and lower to the Error sentinel.
    pub package: Option<&'a ResolvedPackage>,
}

/// Build and push a `TypecheckError` diagnostic, honouring the
/// effective severity from [`LintConfig`]. A `None` severity (the class
/// was suppressed via the manifest's `lints` block) silently drops the
/// diagnostic per `edda-diag`'s emission contract.
pub(super) fn emit_typecheck_error(
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
    span: Span,
    message: impl Into<String>,
) {
    let sev = lint_cfg.effective(DiagnosticClass::TypecheckError);
    diags.push(Diagnostic::new(
        DiagnosticClass::TypecheckError,
        sev,
        span,
        message,
    ));
}

