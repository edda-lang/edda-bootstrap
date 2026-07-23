//! Test fixtures shared across `body/*` tests.
//!
//! Compiled only under `#[cfg(test)]`; not part of the released
//! `edda-codegen` API surface.

use edda_intern::Interner;
use edda_span::Span;
use edda_syntax::ast::{Expr, ExprKind, FnTypeParam, Ident, ParamMode, Pat, PatKind, Path, Type, TypeKind};
use smol_str::SmolStr;
use std::cell::RefCell;
use std::collections::HashMap;

use super::resolver::QualifiedNameResolver;

/// Make a dummy span. Used everywhere a span is required but not
/// consulted (the encoder ignores spans entirely — only AST kinds and
/// payloads enter the hash).
pub(super) fn dummy_span() -> Span {
    Span::DUMMY
}

/// Intern `name` and wrap it in an `Ident` carrying a dummy span.
pub(super) fn ident(interner: &Interner, name: &str) -> Ident {
    Ident {
        name: interner.intern(name),
        span: dummy_span(),
    }
}

/// Build a [`Path`] from dot-separated segments.
pub(super) fn path(interner: &Interner, segments: &[&str]) -> Path {
    Path {
        segments: segments.iter().map(|s| ident(interner, s)).collect(),
        span: dummy_span(),
    }
}

/// Wrap a [`TypeKind`] in a [`Type`] with a dummy span.
pub(super) fn ty(kind: TypeKind) -> Type {
    Type {
        span: dummy_span(),
        kind,
    }
}

/// Build a bare-type function-type parameter (no name, default mode).
pub(super) fn fn_param(t: Type) -> FnTypeParam {
    FnTypeParam {
        span: dummy_span(),
        name: None,
        mode: ParamMode::Default,
        ty: t,
    }
}

/// Wrap an [`ExprKind`] in an [`Expr`] with a dummy span.
pub(super) fn expr(kind: ExprKind) -> Expr {
    Expr {
        span: dummy_span(),
        kind,
    }
}

/// Wrap a [`PatKind`] in a [`Pat`] with a dummy span.
pub(super) fn pat(kind: PatKind) -> Pat {
    Pat {
        span: dummy_span(),
        kind,
    }
}

//   override is registered via [`PassThroughResolver::set_override`]
/// Resolver that returns the dot-joined source segments for each
/// `Path` by default. Adequate for encoder tests that want a
/// deterministic mapping without spinning up a real `Resolutions` map.
pub(super) struct PassThroughResolver<'a> {
    interner: &'a Interner,
    overrides: RefCell<HashMap<String, SmolStr>>,
}

impl<'a> PassThroughResolver<'a> {
    pub(super) fn new(interner: &'a Interner) -> Self {
        PassThroughResolver {
            interner,
            overrides: RefCell::new(HashMap::new()),
        }
    }
}

impl<'a> QualifiedNameResolver for PassThroughResolver<'a> {
    fn resolve_path(&self, path: &Path) -> SmolStr {
        let joined: String = path
            .segments
            .iter()
            .map(|seg| self.interner.resolve(seg.name).to_owned())
            .collect::<Vec<_>>()
            .join(".");
        self.overrides
            .borrow()
            .get(&joined)
            .cloned()
            .unwrap_or_else(|| SmolStr::new(joined))
    }
}
