//! `Path`-shaped type-expression resolution for [`super::lower_type`].
//!
//! Resolves a `Path` type expression to its [`TyId`]: the single-segment
//! primitive / capability shortcut first, then nominal lookup through the
//! resolved package's [`edda_resolve::Resolutions`]. Every interner touch
//! is gated against the parser-recovery sentinel `Symbol::DUMMY`.

use edda_diag::{Diagnostics, LintConfig};
use edda_resolve::{BindingKind, Resolved};
use edda_span::Span;
use edda_syntax::ast;

use crate::capability::CapabilityType;
use crate::lower::LowerCx;
use crate::lower::emit_typecheck_error;
use crate::prim::Primitive;
use crate::ty::TyId;

/// Lower a `Path`-shaped type expression.
///
/// Resolution order:
/// 1. Single-segment path whose ident matches a [`Primitive`] name —
///    lower directly to that primitive. Primitives are never bindings,
///    so the resolver does not record them.
/// 2. Otherwise, if a [`crate::lower::LowerCx::package`] is available,
///    consult [`edda_resolve::Resolutions`] keyed by `path.span`:
///    - [`Resolved::Binding(id)`](edda_resolve::Resolved::Binding)
///      with `kind == TypeDecl` → lower to [`crate::TyKind::Nominal(id)`].
///    - Any other binding kind in type position → diagnostic.
///    - [`Resolved::Module`](edda_resolve::Resolved::Module) in type
///      position → diagnostic.
///    - [`Resolved::Error`](edda_resolve::Resolved::Error) → cascade
///      silently (the resolver already emitted a diagnostic).
/// 3. Without a `package`, emit the "user-defined types not
///    yet supported" deferral diagnostic and return the Error sentinel.
pub(super) fn lower_path(
    path: &ast::Path,
    span: Span,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> TyId {
    // Single-segment primitive / capability shortcut — admits `i32`, `Clock`, etc.
    // without consulting the resolver (these are not scope-resident bindings).
    //
    // Parser-recovery sentinel: a single-segment path whose head is
    // `Symbol::DUMMY` is produced by error-recovery in `edda-syntax`.
    // The parse_error has already been pushed onto the diagnostic take,
    // so we cascade silently to the Error sentinel without reaching the
    // interner — `Interner::resolve(DUMMY)` panics by contract.
    if path.segments.len() == 1 {
        let Some(name) = cx.interner.try_resolve(path.segments[0].name) else {
            return cx.ty_interner.error();
        };
        if let Some(p) = Primitive::from_name(name) {
            return cx.ty_interner.prim(p);
        }
        if let Some(c) = CapabilityType::from_name(name) {
            return cx.ty_interner.capability(c);
        }
    }

    // Non-primitive paths require a resolved package. Without one,
    // defer with a diagnostic.
    let Some(package) = cx.package else {
        let message = if path.segments.len() == 1 {
            // DUMMY here is already returned above; this branch is
            // reachable only when the head is a real symbol.
            match cx.interner.try_resolve(path.segments[0].name) {
                Some(name) => format!(
                    "type `{name}` is not a built-in primitive; \
                     user-defined types require a resolved package"
                ),
                None => return cx.ty_interner.error(),
            }
        } else {
            "qualified type paths (user-defined types) are not yet \
             supported without a resolved package"
                .to_string()
        };
        emit_typecheck_error(diags, lint_cfg, span, message);
        return cx.ty_interner.error();
    };

    match package.resolutions().lookup_path(path.span) {
        Some(Resolved::Binding(id)) => {
            let entry = package.binding(id);
            // SpecInvocation bindings are CA1 pass-1 placeholders for the
            // module that codegen will materialise; admit them as nominal
            // so type-position references resolve. Downstream layout
            // lookups (`TyCx::type_decl`) will return None until the
            // codegen→typecheck feedback wave lands.
            if matches!(entry.kind, BindingKind::TypeDecl | BindingKind::SpecInvocation) {
                cx.ty_interner.nominal(id)
            } else {
                // Parser-recovery sentinel: when the resolver's item
                // table registered an entry under `Symbol::DUMMY` (a
                // recovered-but-incomplete top-level decl, common in a
                // stdlib module that failed to fully build), the binding
                // resolves but has no displayable name. Cascade silently
                // to the Error sentinel — the upstream parse_error is
                // already on the diagnostic take, and re-emitting a
                // typecheck_error against a synthetic name (or panicking
                // in `Interner::resolve`) would only add noise.
                let Some(name) = cx.interner.try_resolve(entry.name) else {
                    return cx.ty_interner.error();
                };
                emit_typecheck_error(
                    diags,
                    lint_cfg,
                    span,
                    format!("`{name}` is not a type"),
                );
                cx.ty_interner.error()
            }
        }
        Some(Resolved::Module(_)) => {
            emit_typecheck_error(
                diags,
                lint_cfg,
                span,
                "module name used in type position; expected a type",
            );
            cx.ty_interner.error()
        }
        Some(Resolved::Error) => {
            // Resolver already diagnosed; cascade silently.
            cx.ty_interner.error()
        }
        None => {
            // No recorded resolution — typically a primitive whose
            // path predates the resolver walk, or a hand-built
            // AST under test. Emit the deferral message.
            let message = if path.segments.len() == 1 {
                match cx.interner.try_resolve(path.segments[0].name) {
                    Some(name) => format!(
                        "type `{name}` is not a built-in primitive and has \
                         no resolution recorded"
                    ),
                    None => return cx.ty_interner.error(),
                }
            } else {
                "qualified type path has no resolution recorded".to_string()
            };
            emit_typecheck_error(diags, lint_cfg, span, message);
            cx.ty_interner.error()
        }
    }
}
