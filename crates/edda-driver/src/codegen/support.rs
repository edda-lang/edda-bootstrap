//! Codegen-stage support helpers — path joining, worklist dedup keys,
//! cache-tier classification, and diagnostic projection.

use edda_cache::Tier;
use edda_codegen::{Argument, ArgumentTuple, CodegenError};
use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_intern::Interner;
use edda_span::Span;
use edda_syntax::ast::Path as AstPath;

/// Dot-join the source segments of a Path. Used as the resolver's
/// last-resort fallback when the resolution map has no entry.
pub(super) fn join_segments(path: &AstPath, interner: &Interner) -> String {
    let mut out = String::new();
    for (i, seg) in path.segments.iter().enumerate() {
        if i > 0 {
            out.push('.');
        }
        out.push_str(interner.resolve(seg.name));
    }
    out
}

/// Compose a deduplication key for the codegen worklist. Cheaper than
/// the full BLAKE3 canonical-form hash because `instantiate_spec` has
/// not yet been called when the key is consulted; downstream the
/// hash-level dedup still applies via `CodegenSession::stage`.
pub(super) fn dedup_key(spec_qualified: &str, args: &ArgumentTuple) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(spec_qualified.len() + 16);
    out.push_str(spec_qualified);
    out.push('(');
    for (i, arg) in args.args().iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        match arg {
            Argument::Type(name) => out.push_str(name.as_str()),
            // Prefix keeps a function arg textually distinct from a
            // same-named type arg in the worklist dedup key (though their
            // qnames are namespace-disjoint in practice).
            Argument::Function(name) => {
                out.push_str("fn:");
                out.push_str(name.as_str());
            }
            Argument::Primitive(pv) => {
                write!(out, "{pv:?}").expect("formatting into String is infallible")
            }
            Argument::EffectRow(_) => out.push_str("<effect-row>"),
            Argument::UserDefined(_) => out.push_str("<user-defined>"),
        }
    }
    out.push(')');
    out
}

/// Classify the cache tier for an invocation rooted in `source_path`.
///
/// Per `storage.md` §1: stdlib-originated invocations land in
/// cache-tier (shared across projects); project-local invocations
/// land in repo-tier (live alongside the source).
pub(super) fn classify_tier(source_path: &std::path::Path, stdlib_root: Option<&std::path::Path>) -> Tier {
    match stdlib_root {
        Some(root) if source_path.starts_with(root) => Tier::Cache,
        _ => Tier::Repo,
    }
}

//   §8 class set has no `codegen_error` variant so codegen failures
//   fall back to this class per `edda-codegen::error`'s policy
pub(super) fn emit_typecheck(
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

/// Project a [`CodegenError`] to a [`Diagnostic`] and push it.
pub(super) fn push_codegen_error(
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
    err: &CodegenError,
) {
    let diag = err.to_diagnostic();
    let sev = lint_cfg.effective(diag.class);
    diags.push(Diagnostic::new(diag.class, sev, diag.primary.span, diag.message));
}
