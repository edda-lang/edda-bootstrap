//! The `duplicate_spec_invocation` lint: identical `spec PATH(ARGS)` in scope.

use std::collections::HashMap;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_intern::Symbol;
use edda_syntax::ast::{Expr, ExprKind, ItemKind};

use crate::ResolvedPackage;
use crate::resolve::ResolveCx;

/// Emit `duplicate_spec_invocation` for every `spec PATH(ARGS)` that
/// appears more than once with lexically identical arguments within the
/// same scope. Scopes are: each file's top-level items, plus each
/// `spec NAME(...) { body }`'s body — generic-parameter bindings differ
/// across parent specs, so a `spec Option(T)` inside `spec StringMap(T)`
/// is NOT a duplicate of a `spec Option(T)` inside `spec IntMap(T)`.
pub fn emit_duplicate_spec_invocation_lints(
    package: &ResolvedPackage,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) {
    let severity = lint_cfg.effective(DiagnosticClass::DuplicateSpecInvocation);
    for module in &package.modules {
        let entry = package.graph.module(module.id);
        check_spec_invocation_scope(&entry.ast.items, cx, diags, severity);
    }
}

fn check_spec_invocation_scope(
    items: &[edda_syntax::ast::Item],
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    severity: edda_diag::Severity,
) {
    let mut seen: HashMap<String, edda_span::Span> = HashMap::new();
    for item in items {
        match &item.kind {
            ItemKind::SpecInvocation(si) => {
                let Some(key) = spec_invocation_key(si, cx.interner) else {
                    continue; // arg shape not admitted in the canonical key — skip
                };
                if let Some(&earlier_span) = seen.get(&key) {
                    let path_text = format_spec_path(&si.path, cx.interner);
                    let diag = Diagnostic::new(
                        DiagnosticClass::DuplicateSpecInvocation,
                        severity,
                        si.span,
                        format!("duplicate `spec {path_text}` invocation with identical arguments"),
                    )
                    .with_label(earlier_span, "first invocation here")
                    .with_note(
                        "both invocations content-address to the same module; the second has no effect — remove it",
                    );
                    diags.push(diag);
                } else {
                    seen.insert(key, si.span);
                }
            }
            ItemKind::Spec(spec) => {
                // Nested spec body is a separate scope (its generic
                // parameters bind freshly).
                check_spec_invocation_scope(&spec.body, cx, diags, severity);
            }
            _ => {}
        }
    }
}

fn spec_invocation_key(
    si: &edda_syntax::ast::SpecInvocation,
    interner: &edda_intern::Interner,
) -> Option<String> {
    if si.path.segments.iter().any(|s| s.name == Symbol::DUMMY) {
        return None;
    }
    let path = format_spec_path(&si.path, interner);
    let mut key = String::with_capacity(path.len() + 16);
    key.push_str(&path);
    for arg in &si.args {
        key.push('|');
        key.push_str(&expr_canonical_key(arg, interner)?);
    }
    Some(key)
}

fn format_spec_path(
    path: &edda_syntax::ast::Path,
    interner: &edda_intern::Interner,
) -> String {
    let mut out = String::new();
    for (i, seg) in path.segments.iter().enumerate() {
        if i > 0 {
            out.push('.');
        }
        out.push_str(interner.try_resolve(seg.name).unwrap_or("<missing>"));
    }
    out
}

fn expr_canonical_key(
    expr: &Expr,
    interner: &edda_intern::Interner,
) -> Option<String> {
    use edda_syntax::ast::Literal;
    match &expr.kind {
        ExprKind::Path(p) => {
            let mut s = String::from("p:");
            for (i, seg) in p.segments.iter().enumerate() {
                if i > 0 {
                    s.push('.');
                }
                s.push_str(interner.try_resolve(seg.name)?);
            }
            Some(s)
        }
        ExprKind::Literal(lit) => match lit {
            Literal::Int { value, .. } => Some(format!("i:{value}")),
            Literal::Bool(b) => Some(format!("b:{b}")),
            Literal::Str(sym) => Some(format!("s:{}", interner.try_resolve(*sym)?)),
            Literal::Float(sym) => Some(format!("f:{}", interner.try_resolve(*sym)?)),
            Literal::Unit => Some(String::from("u")),
        },
        // Other Expr shapes (calls, tuples, etc.) aren't supported as
        // canonical keys — skip the lint for invocations with such args.
        _ => None,
    }
}
