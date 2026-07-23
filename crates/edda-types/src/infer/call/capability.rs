//! Call-site capability-row substitution (`effect-tracking.md §2`).
//!
//! `translate_entry` rewrites a callee row entry into the caller-side
//! entry that should contribute to the caller's accumulator, and the
//! `capability_source*` helpers walk a HIR expression to find the
//! originating capability binding.

use ahash::AHashMap;
use edda_intern::Symbol;

use crate::effect::EffectEntry;
use crate::hir::{HirCallArg, HirExpr, HirExprKind};
use crate::sig::Param;

/// Maximum recursion depth when walking a capability-source
/// derivation chain. The bound is intentionally loose — real
/// derivations are 1–2 levels (`world.network`); deeper chains
/// indicate either malformed HIR or a future surface we haven't
/// locked yet.
pub(super) const CAPABILITY_DERIVATION_DEPTH: usize = 32;

/// Rewrite a callee row entry into the caller-side entry that should
/// contribute to the caller's accumulator.
///
/// - `Pure(*)` passes through unchanged — qualified-name match
///   handles propagation.
/// - `Capability(callee_param)` is rewritten to the caller-side
///   capability source: find the positional parameter named
///   `callee_param` in `params`, look at the corresponding argument,
///   walk its expression for a single-segment path or a chain of
///   field projections, and use that root symbol as the source. If
///   the root symbol is an alias in `aliases` (a let-bound derivation
///   such as `let mono = clock.monotonic()`), the alias target is
///   used instead. If no source is recoverable, the entry passes
///   through unchanged — the function-exit row-containment check will
///   surface the issue.
/// Rewrite a callee row entry for the call site.
///
/// Consolidates the path-form and method-form translations: when
/// `receiver` is `Some`, the receiver fills the `params[0]` slot and
/// `args` covers `params[1..]`; when `receiver` is `None`, `args` and
/// `params` line up positionally.
pub(crate) fn translate_entry(
    entry: EffectEntry,
    params: &[Param],
    receiver: Option<&HirExpr>,
    args: &[HirCallArg],
    aliases: &AHashMap<Symbol, Symbol>,
) -> EffectEntry {
    let EffectEntry::Capability(callee_sym) = entry else {
        return entry; // Pure(_) passes through.
    };
    let Some(idx) = params.iter().position(|p| p.name == callee_sym) else {
        // Callee row references an unknown parameter — surface via the
        // exit-check fallback rather than fabricating a substitution.
        return entry;
    };
    let raw = match receiver {
        Some(receiver) if idx == 0 => {
            // Method form: the receiver supplies the `params[0]` source.
            // `capability_source_of_call` handles the
            // `clock.monotonic()` shape where the receiver is itself a
            // method call.
            capability_source(receiver).or_else(|| capability_source_of_call(receiver))
        }
        Some(_) => args.get(idx - 1).and_then(|a| capability_source(&a.expr)),
        None => args.get(idx).and_then(|a| capability_source(&a.expr)),
    };
    match raw {
        Some(raw_sym) => {
            // Walk the alias map: `mono → clock` so `Capability(clock)` propagates.
            let resolved_sym = aliases.get(&raw_sym).copied().unwrap_or(raw_sym);
            EffectEntry::Capability(resolved_sym)
        }
        None => entry,
    }
}

/// Extract the capability source of a narrowing call expression.
///
/// Fires only for call-shaped expressions (`fs.read_only()`,
/// `fsmod.scoped_to(fsmod.read_only(fs), p)`), delegating to
/// [`capability_source`], which traces the call to the capability it
/// narrows. Returns `None` for any non-call shape so a plain rebind
/// (`let x = fs`) does not record a spurious alias.
pub(crate) fn capability_source_of_call(expr: &HirExpr) -> Option<Symbol> {
    match &expr.kind {
        // Delegate to `capability_source`, which traces a narrowing call
        // to the capability it derives from — the receiver in method form
        // (`clock.monotonic()`) or the leading positional argument in
        // free-function form (`fsmod.read_only(fs)`). Restricted to
        // call-shaped expressions so non-call inits (a plain rebind
        // `let x = fs`) do not record a spurious alias.
        HirExprKind::MethodCall { .. } | HirExprKind::Call { .. } => capability_source(expr),
        _ => None,
    }
}

/// Walk a HIR expression to find the originating capability source.
///
/// Admits: a single-segment path naming a binding (`fs`); a chain of
/// field projections rooted at a single-segment path
/// (`world.network.local_addr`); and a narrowing call, which is traced
/// to the capability it derives from — the receiver in method form
/// (`fs.read_only()`) or the leading positional argument in
/// free-function form (`fsmod.scoped_to(fsmod.read_only(fs), p)`). The
/// capability source is the root of the chain — the leftmost binding
/// name. Any other shape (arbitrary computed expression, multi-segment
/// module path, etc.) returns `None`.
pub(crate) fn capability_source(expr: &HirExpr) -> Option<Symbol> {
    capability_source_depth(expr, CAPABILITY_DERIVATION_DEPTH)
}

fn capability_source_depth(expr: &HirExpr, depth: usize) -> Option<Symbol> {
    if depth == 0 {
        return None;
    }
    match &expr.kind {
        HirExprKind::Path(p) if p.segments.len() == 1 => Some(p.segments[0].name),
        HirExprKind::Field { receiver, .. } => {
            capability_source_depth(receiver, depth - 1)
        }
        // A capability produced by a narrowing call derives from the
        // capability it narrows: the receiver in method form
        // (`fs.read_only()`), or the leading positional argument in
        // free-function form (`fsmod.scoped_to(fsmod.read_only(fs), p)`).
        // Every locked narrowing method takes its source capability in
        // that slot, so tracing it lets an inline narrowing chain resolve
        // to its ambient root parameter.
        HirExprKind::MethodCall { receiver, .. } => {
            capability_source_depth(receiver, depth - 1)
        }
        HirExprKind::Call { callee, args } => match &callee.kind {
            HirExprKind::Field { receiver, .. } => {
                capability_source_depth(receiver, depth - 1)
            }
            _ => capability_source_depth(&args.first()?.expr, depth - 1),
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edda_intern::Interner;
    use edda_syntax::ast::Ident;
    use crate::hir::{HirCallArg, HirPath};
    use crate::ty::TyInterner;
    use edda_span::Span;

    fn ident(interner: &Interner, name: &str) -> Ident {
        Ident { name: interner.intern(name), span: Span::DUMMY }
    }

    fn hir_path(interner: &Interner, name: &str, ty: &TyInterner) -> HirExpr {
        HirExpr {
            span: Span::DUMMY,
            ty: ty.error(),
            kind: HirExprKind::Path(HirPath {
                span: Span::DUMMY,
                segments: Box::from([ident(interner, name)]),
            }),
        }
    }

    fn arg(expr: HirExpr) -> HirCallArg {
        HirCallArg { span: Span::DUMMY, mode: None, name: None, expr }
    }

    fn lit_int(ty: &TyInterner) -> HirExpr {
        HirExpr {
            span: Span::DUMMY,
            ty: ty.error(),
            kind: HirExprKind::Literal(edda_syntax::ast::Literal::Int {
                value: 0,
                base: edda_syntax::IntBase::Dec,
            }),
        }
    }

    fn call(callee: HirExpr, args: Vec<HirCallArg>, ty: &TyInterner) -> HirExpr {
        HirExpr {
            span: Span::DUMMY,
            ty: ty.error(),
            kind: HirExprKind::Call {
                callee: Box::new(callee),
                args: args.into_boxed_slice(),
            },
        }
    }

    fn method_call(receiver: HirExpr, interner: &Interner, name: &str, args: Vec<HirCallArg>, ty: &TyInterner) -> HirExpr {
        HirExpr {
            span: Span::DUMMY,
            ty: ty.error(),
            kind: HirExprKind::MethodCall {
                receiver: Box::new(receiver),
                name: ident(interner, name),
                args: args.into_boxed_slice(),
            },
        }
    }

    #[test]
    fn capability_source_traces_inline_free_function_narrowing_chain() {
        // `scoped_to(read_only(fs), 0)` — capability arg is the inline
        // narrowing chain rooted at `fs`.
        let interner = Interner::new();
        let ty = TyInterner::new();
        let fs_sym = interner.intern("fs");
        let read_only = call(
            hir_path(&interner, "read_only", &ty),
            vec![arg(hir_path(&interner, "fs", &ty))],
            &ty,
        );
        let scoped = call(
            hir_path(&interner, "scoped_to", &ty),
            vec![arg(read_only), arg(lit_int(&ty))],
            &ty,
        );
        assert_eq!(capability_source(&scoped), Some(fs_sym));
    }

    #[test]
    fn capability_source_traces_method_narrowing_call() {
        // `fs.read_only()` — method form, source is the receiver `fs`.
        let interner = Interner::new();
        let ty = TyInterner::new();
        let fs_sym = interner.intern("fs");
        let ro = method_call(hir_path(&interner, "fs", &ty), &interner, "read_only", vec![], &ty);
        assert_eq!(capability_source(&ro), Some(fs_sym));
        // `fs.read_only().scoped_to(0)` — chained method narrowing.
        let sc = method_call(ro, &interner, "scoped_to", vec![arg(lit_int(&ty))], &ty);
        assert_eq!(capability_source(&sc), Some(fs_sym));
    }
}
