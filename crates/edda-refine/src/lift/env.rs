//! [`PredicateEnv`] trait — the typechecker integration seam.
//!
//! Type-context queries the lifter needs from the caller. Today refine has
//! no production consumer — tests construct synthetic impls; the eventual
//! typechecker integration provides a `TyCx`-backed impl. Every method on
//! this trait corresponds to a type-system answer the lifter cannot infer
//! on its own; the typechecker has already inferred them by the time the
//! lifter runs, and the lifter just routes those answers into
//! [`Predicate`](crate::Predicate) constructors.

use smol_str::SmolStr;

use edda_span::Span;
use edda_syntax::ast::{self, Expr, Ident};

use crate::sort::{FieldRef, Sort};

//            doc for the `@pattern predicate-env-lookup` rationale
//          lifter about source-level types and bindings
/// Type-context queries the lifter needs from the caller. Today refine has
/// no production consumer — tests construct synthetic impls; the eventual
/// typechecker integration provides a `TyCx`-backed impl.
///
/// Every method is required because the lifter cannot infer types on its
/// own; the typechecker has already inferred them by the time the lifter
/// runs, and the lifter just routes those answers into [`Predicate`]
/// constructors.
///
/// [`Predicate`]: crate::Predicate
pub trait PredicateEnv {
    /// Resolve a [`Path`-expression](edda_syntax::ast::ExprKind::Path)'s
    /// source span to a `(name, sort)` pair. The name is the source-level
    /// identifier (or qualified path) the typechecker associates with the
    /// path's resolved binding; the sort is the binding's inferred type
    /// projected into refine's [`Sort`] system. Returns `None` when the
    /// path resolves to something the predicate fragment doesn't admit
    /// (function, type-decl, module).
    fn lookup_path(&self, span: Span) -> Option<(SmolStr, Sort)>;

    /// Compute the sort of an expression. Used at every spot the lifter
    /// needs to discriminate sub-expression behaviour by sort (e.g. picking
    /// the right [`Predicate`](crate::Predicate) arm for a `==` on bool vs.
    /// record vs. int). Returns `None` for expressions whose type inference
    /// failed.
    fn expr_sort(&self, expr: &Expr) -> Option<Sort>;

    /// Resolve a field name against a base record sort into a [`FieldRef`].
    /// The lifter calls this for every
    /// [`ExprKind::Field`](edda_syntax::ast::ExprKind::Field) node.
    fn lookup_field(&self, base_sort: &Sort, field: &Ident) -> Option<FieldRef>;

    /// Translate an [`ast::Type`] expression into a refine [`Sort`]. Used
    /// for cast targets. Returns `None` when the type isn't supported.
    fn type_sort(&self, ty: &ast::Type) -> Option<Sort>;

    /// Resolve an [`Ident`]'s interned symbol to its source text. Used to
    /// match the well-known `len()` method on slices and to render field /
    /// path names into [`SmolStr`] for
    /// [`Variable`](crate::Variable) / [`FieldRef`].
    fn ident_name(&self, ident: &Ident) -> SmolStr;

    /// Register a quantifier-bound variable for the duration of a body lift.
    /// After this call, `lookup_path` for spans that resolve to the named
    /// local binding must return the supplied `(name, sort)` pair.
    ///
    /// Default implementation is a no-op. Implementations that participate
    /// in bounded-quantifier lifting (`forall` / `exists`) override this
    /// alongside `pop_quantifier_bound` and surface the bound through
    /// `lookup_path` against the named local. Implementations that don't
    /// support quantifiers may leave the default — the lifter will then
    /// fail on the bound's body uses with `UnresolvedPath`.
    fn push_quantifier_bound(&self, _ident: &Ident, _sort: Sort) {}

    /// Pop a previously-pushed quantifier bound. Default: no-op.
    fn pop_quantifier_bound(&self, _ident: &Ident) {}
}
