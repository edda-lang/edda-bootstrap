//! AST substitution walker — clone-and-rewrite over a [`Spec`]'s body.
//!
//! Recurses into every value-bearing AST node and rewrites Path nodes
//! whose head segment matches a binding in a [`SubstitutionMap`]:
//!
//! - `TypeKind::Path` whose head names a `Type`-kind generic → replace
//!   the path with the binding's qualified-name segments followed by
//!   the original path's tail.
//! - `ExprKind::Path` whose head names a `Type`-kind generic → ditto,
//!   wrapped back into an `ExprKind::Path`.
//! - `ExprKind::Path` whose head names a `Function`-bound generic
//!   → same head-rewrite as a `Type`
//!   arg, so an in-body `f(args)` call becomes a direct call on the
//!   bound function's qualified name.
//! - `ExprKind::Path` single-segment naming a `Comptime`-kind generic
//!   → replace the entire `Expr` with the bound primitive's literal
//!   form. Negative signed integers wrap in `ExprKind::Unary { Neg }`
//!   around a non-negative integer literal so the pretty-printer can
//!   round-trip them.
//! - `StructLit.path`, `Pat::Variant.path`, `Pat::Struct.path` — same
//!   Type-kind head rewrite (these positions are type references and
//!   never name a value).
//!
//! All other variants pass through with a structural deep-clone that
//! preserves spans verbatim. Synthetic identifiers minted from a
//! qualified-name string inherit the originating Path's span — the
//! substituted body is not a faithful source-map back to the spec
//! body, but diagnostic attribution is consistent within one
//! invocation.
//!
//! `EffectMember::Spread` is left untouched. Expanding it requires an
//! `Argument::EffectRow` binding, which [`crate::SubstitutionMap::bind`]
//! rejects.

mod expr_kind;
mod ty_pat;

use edda_intern::Interner;
use edda_span::Span;
use edda_syntax::IntBase;
use edda_syntax::ast::{
    Block, Expr, ExprKind, Ident, Item, Literal, Path, Spec, Stmt, StmtKind, Type, TypeKind, UnOp,
};

use crate::argument::{Argument, PrimitiveValue};
use crate::substitution::map::SubstitutionMap;

/// Substitute the spec's comptime parameters inside `spec.body` and
/// return the rewritten item list.
///
/// The returned items are deep clones of `spec.body` with every Path
/// whose head segment matches a binding in `subst` rewritten per the
/// module-level rules. `interner` is used to mint [`edda_intern::Symbol`]
/// handles for synthetic identifiers produced from `Argument::Type`
/// qualified-name strings.
///
/// This function does no validation — the caller must have constructed
/// `subst` from `spec.generics` via [`SubstitutionMap::bind`].
pub fn substitute_spec_body(
    spec: &Spec,
    subst: &SubstitutionMap,
    interner: &Interner,
) -> Vec<Item> {
    // Augment `subst` with pre→post mangled-name renames for every
    // nested SpecInvocation in the body (e.g. `Option_V → Option_f64`
    // after `V := f64`). The augmented map drives the same Path-rewrite
    // machinery that handles generic-parameter substitution, so body
    // references like `Option_V.some(...)` route to `Option_f64.some(...)`.
    let augmented = subst.clone().with_sibling_renames(&spec.body, interner);
    let walker = Walker { subst: &augmented, interner };
    spec.body.iter().map(|i| walker.item(i)).collect()
}

//   duration; the walker mints `Symbol` handles only via `interner`
pub(super) struct Walker<'a> {
    pub(super) subst: &'a SubstitutionMap,
    pub(super) interner: &'a Interner,
}

impl<'a> Walker<'a> {
    pub(super) fn block(&self, b: &Block) -> Block {
        Block {
            span: b.span,
            stmts: b.stmts.iter().map(|s| self.stmt(s)).collect(),
            trailing: b.trailing.as_ref().map(|e| Box::new(self.expr(e))),
        }
    }

    fn stmt(&self, s: &Stmt) -> Stmt {
        Stmt {
            span: s.span,
            attributes: s.attributes.clone(),
            kind: self.stmt_kind(&s.kind),
        }
    }

    fn stmt_kind(&self, k: &StmtKind) -> StmtKind {
        match k {
            StmtKind::Let {
                mutability,
                pat,
                ty,
                init,
            } => StmtKind::Let {
                mutability: *mutability,
                pat: self.pat(pat),
                ty: ty.as_ref().map(|t| self.ty(t)),
                init: init.as_ref().map(|e| self.expr(e)),
            },
            StmtKind::Assign { target, op, rhs } => StmtKind::Assign {
                target: self.expr(target),
                op: *op,
                rhs: self.expr(rhs),
            },
            StmtKind::Expr(e) => StmtKind::Expr(self.expr(e)),
        }
    }

    pub(super) fn expr(&self, e: &Expr) -> Expr {
        if let ExprKind::Path(path) = &e.kind {
            if let Some(replacement) = self.try_subst_path_expr(path, e.span) {
                return replacement;
            }
        }
        Expr {
            span: e.span,
            kind: self.expr_kind(&e.kind),
        }
    }

    pub(super) fn ty(&self, t: &Type) -> Type {
        if let TypeKind::Path(path) = &t.kind {
            if let Some(replacement) = self.try_subst_path_type(path, t.span) {
                return replacement;
            }
        }
        Type {
            span: t.span,
            kind: self.ty_kind(&t.kind),
        }
    }

    /// If `path`'s head names a binding bound to an `Argument::Type`,
    /// return a rewritten `Type::Path` whose segments come from the
    /// binding's qualified name followed by `path`'s tail. Otherwise
    /// `None`. Admits both `Type`-kind generics and `comptime T: Type`
    /// generics — the canonical-form encoder distinguishes them via the
    /// argument tag, not the generic-parameter kind.
    fn try_subst_path_type(&self, path: &Path, span: Span) -> Option<Type> {
        let head = path.segments.first()?;
        let binding = self.subst.lookup(head.name)?;
        let Argument::Type(qname) = &binding.value else {
            return None;
        };
        Some(Type {
            span,
            kind: TypeKind::Path(self.rewrite_path_with_qname(path, qname)),
        })
    }

    /// If `path`'s head names a binding, return the substituted `Expr`:
    /// a `Type` argument → `ExprKind::Path` of the qualified type name;
    /// a single-segment `Primitive` argument → literal form. The walker
    /// switches on the argument tag rather than the generic-parameter
    /// kind so `comptime T: Type` parameters (bound to `Argument::Type`)
    /// are admitted alongside bare `Type`-kind generics.
    fn try_subst_path_expr(&self, path: &Path, span: Span) -> Option<Expr> {
        let head = path.segments.first()?;
        let binding = self.subst.lookup(head.name)?;
        match &binding.value {
            // A Type arg and a Function arg both rewrite the path
            // head to the bound qualified name. For a function this turns
            // an in-body `f(key)` call into a direct call on the function's
            // qualified name (`probe.hstr(key)`), which the pass-2
            // resolve/typecheck/MIR pipeline lowers like any other call —
            // no MIR-side comptime-binding plumbing is needed.
            Argument::Type(qname) | Argument::Function(qname) => Some(Expr {
                span,
                kind: ExprKind::Path(self.rewrite_path_with_qname(path, qname)),
            }),
            Argument::Primitive(prim) => {
                if path.segments.len() != 1 {
                    return None;
                }
                Some(prim_to_expr(prim, span, self.interner))
            }
            _ => None,
        }
    }

    /// Rewrite a path whose head segment is bound to an `Argument::Type`;
    /// in any other case (unbound head, non-Type binding, missing head)
    /// return the path unchanged. Used at positions whose path must
    /// resolve to a type (struct literal, sum-variant pattern, struct
    /// pattern). Admits both `Type`-kind generics and `comptime T: Type`
    /// generics — see `try_subst_path_type` for the rationale.
    fn rewrite_path_as_type(&self, path: &Path) -> Path {
        let Some(head) = path.segments.first() else {
            return path.clone();
        };
        let Some(binding) = self.subst.lookup(head.name) else {
            return path.clone();
        };
        let Argument::Type(qname) = &binding.value else {
            return path.clone();
        };
        self.rewrite_path_with_qname(path, qname)
    }

    /// Build a path whose segments are `qname.split('.')` followed by
    /// `path.segments[1..]`. Empty segments produced by leading or
    /// trailing dots are dropped; this is permissive — the caller is
    /// responsible for the qualified-name's well-formedness.
    fn rewrite_path_with_qname(&self, path: &Path, qname: &str) -> Path {
        let mut segments: Vec<Ident> = qname
            .split('.')
            .filter(|s| !s.is_empty())
            .map(|s| Ident {
                name: self.interner.intern(s),
                span: path.span,
            })
            .collect();
        segments.extend(path.segments.iter().skip(1).copied());
        Path {
            segments,
            span: path.span,
        }
    }
}

//   and the unsigned-integer primitives; signed-integer primitives below
//   zero are wrapped in an `ExprKind::Unary { Neg }` around a non-
//   negative `Literal::Int`
/// Render a [`PrimitiveValue`] as a syntactically equivalent [`Expr`].
fn prim_to_expr(prim: &PrimitiveValue, span: Span, interner: &Interner) -> Expr {
    match prim {
        PrimitiveValue::Bool(b) => lit_expr(Literal::Bool(*b), span),
        PrimitiveValue::String(s) => lit_expr(Literal::Str(interner.intern(s)), span),
        PrimitiveValue::U8(n) => lit_expr(unsigned_int(*n as u128), span),
        PrimitiveValue::U16(n) => lit_expr(unsigned_int(*n as u128), span),
        PrimitiveValue::U32(n) => lit_expr(unsigned_int(*n as u128), span),
        PrimitiveValue::U64(n) => lit_expr(unsigned_int(*n as u128), span),
        PrimitiveValue::USize(n) => lit_expr(unsigned_int(*n as u128), span),
        PrimitiveValue::I8(n) => signed_int_expr(*n as i128, span),
        PrimitiveValue::I16(n) => signed_int_expr(*n as i128, span),
        PrimitiveValue::I32(n) => signed_int_expr(*n as i128, span),
        PrimitiveValue::I64(n) => signed_int_expr(*n as i128, span),
        PrimitiveValue::ISize(n) => signed_int_expr(*n as i128, span),
    }
}

fn lit_expr(lit: Literal, span: Span) -> Expr {
    Expr {
        span,
        kind: ExprKind::Literal(lit),
    }
}

fn unsigned_int(value: u128) -> Literal {
    Literal::Int {
        value,
        base: IntBase::Dec,
    }
}

//   value overflows the literal's `u128` payload
/// Build an `Expr` for a signed integer. Negative values become a
/// unary `Neg` wrapping a non-negative `Literal::Int` so the pretty-
/// printer can round-trip them without a signed-literal grammar.
fn signed_int_expr(value: i128, span: Span) -> Expr {
    if value >= 0 {
        return lit_expr(unsigned_int(value as u128), span);
    }
    let inner = lit_expr(unsigned_int(value.unsigned_abs()), span);
    Expr {
        span,
        kind: ExprKind::Unary {
            op: UnOp::Neg,
            expr: Box::new(inner),
        },
    }
}
