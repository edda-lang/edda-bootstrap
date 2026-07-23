//! Template clone-and-substitute walker for outbound comptime type
//! parameters (layer 2).
//!
//! Clones a template [`FnDecl`] and rewrites, in place, every `Path`
//! whose head segment names a bound outbound generic — in type
//! position (parameter / return / cast / let-annotation / nested
//! composites), expression position (`size_of(U)`, `field_count(U)`,
//! `U.(d)` construction receivers), and pattern position
//! (`U.variant(...)` arms). The substituted head carries the *bound
//! type's* leaf symbol and span; because the package `Resolutions` map
//! is span-keyed, the rewritten reference resolves to the caller-side
//! type without any resolver mutation. Mirrors the head-rewrite rule
//! of `edda-codegen`'s spec-body substitution walker, restricted to
//! the `(leaf, span)` reference form the mono pass infers.

use ahash::AHashMap;
use edda_intern::Symbol;
use edda_span::Span;
use edda_syntax::ast::{
    Block, Expr, ExprKind, FnBody, FnDecl, Ident, Pat, PatKind, Path, Stmt, StmtKind, Type,
    TypeKind, VariantPatPayload,
};

use super::BoundTy;

/// Bound-type substitution map: each bound generic's name → the
/// [`BoundTy`] the walker splices for it. A `Named` bound rewrites a path
/// head in place; a structural `Unit` / `Tuple` bound replaces the whole
/// type node (`()` / tuple type) or the whole expression node (a
/// folder-recognized tuple-composite sentinel).
type Subs = AHashMap<Symbol, BoundTy>;

/// Clone `template` with every bound outbound-generic reference
/// substituted and the declaration renamed to `mangled_name`.
pub(super) fn substitute_fn_decl(
    template: &FnDecl,
    subs: &Subs,
    mangled_name: Symbol,
) -> FnDecl {
    let mut decl = template.clone();
    decl.name = Ident {
        name: mangled_name,
        span: template.name.span,
    };
    decl.outbound_generics = Vec::new();
    decl.generics.retain(|g| !subs.contains_key(&g.name.name));
    for p in &mut decl.params {
        subst_type(&mut p.ty, subs);
    }
    if let Some(rt) = &mut decl.return_ty {
        subst_type(rt, subs);
    }
    for r in &mut decl.refinements {
        subst_expr(&mut r.pred, subs);
    }
    if let FnBody::Block(b) = &mut decl.body {
        subst_block(b, subs);
    }
    decl
}

/// Rewrite `path`'s head if it names a `Named`-bound generic. Returns
/// `true` when a rewrite happened.
fn subst_path(path: &mut Path, subs: &Subs) -> bool {
    let head = match path.segments.first() {
        Some(h) => h.name,
        None => return false,
    };
    let Some(BoundTy::Named(leaf, span)) = subs.get(&head) else {
        return false;
    };
    path.segments[0] = Ident {
        name: *leaf,
        span: *span,
    };
    path.span = *span;
    true
}

/// A `Named`-bound leaf's single-segment type path.
fn named_path(leaf: Symbol, span: Span) -> Path {
    Path {
        segments: vec![Ident { name: leaf, span }],
        span,
    }
}

/// The whole-type-node splice for a structural bound at a path head:
/// `TypeKind::Unit` for `()`, `TypeKind::Tuple` for a payload composite,
/// `TypeKind::Slice` for a slice bound.
/// `None` for a `Named` bound (rewritten in place) or a non-bound head.
fn structural_type_kind(head: &Symbol, subs: &Subs) -> Option<TypeKind> {
    match subs.get(head)? {
        BoundTy::Named(..) => None,
        BoundTy::Unit => Some(TypeKind::Unit),
        BoundTy::Tuple(elems) => Some(TypeKind::Tuple(
            elems
                .iter()
                .map(|(leaf, span)| Type {
                    span: *span,
                    kind: TypeKind::Path(named_path(*leaf, *span)),
                })
                .collect(),
        )),
        BoundTy::Slice(leaf, span) => Some(TypeKind::Slice(Box::new(Type {
            span: *span,
            kind: TypeKind::Path(named_path(*leaf, *span)),
        }))),
    }
}

/// The whole-expression-node splice for a structural bound at a path
/// head: an empty `Tuple` expression for `()`, a `Tuple` of the element
/// type paths for a payload composite, a one-element `Array` `[E]` for a
/// slice bound — the folder-recognized
/// sentinels. `None` for a `Named` bound (rewritten in place) or a
/// non-bound head.
fn structural_sentinel_kind(head: &Symbol, subs: &Subs) -> Option<ExprKind> {
    match subs.get(head)? {
        BoundTy::Named(..) => None,
        BoundTy::Unit => Some(ExprKind::Tuple(Vec::new())),
        BoundTy::Tuple(elems) => Some(ExprKind::Tuple(
            elems
                .iter()
                .map(|(leaf, span)| Expr {
                    span: *span,
                    kind: ExprKind::Path(named_path(*leaf, *span)),
                })
                .collect(),
        )),
        BoundTy::Slice(leaf, span) => Some(ExprKind::Array(vec![Expr {
            span: *span,
            kind: ExprKind::Path(named_path(*leaf, *span)),
        }])),
    }
}

/// Rewrite a type expression in place.
fn subst_type(ty: &mut Type, subs: &Subs) {
    // A structural bound (`Unit` / `Tuple`) at a bound-generic head
    // replaces the whole type node; compute the replacement first so the
    // immutable borrow of `ty.kind` ends before the assignment.
    let replacement = match &ty.kind {
        TypeKind::Path(p) => p
            .segments
            .first()
            .and_then(|s| structural_type_kind(&s.name, subs)),
        _ => None,
    };
    if let Some(kind) = replacement {
        ty.kind = kind;
        return;
    }
    match &mut ty.kind {
        TypeKind::Path(p) => {
            if subst_path(p, subs) && p.segments.len() == 1 {
                ty.span = p.span;
            }
        }
        TypeKind::Tuple(elems) => {
            for e in elems {
                subst_type(e, subs);
            }
        }
        TypeKind::Slice(inner) => subst_type(inner, subs),
        TypeKind::Function { params, ret, .. } => {
            for p in params {
                subst_type(&mut p.ty, subs);
            }
            subst_type(ret, subs);
        }
        TypeKind::Comptime(inner) => subst_type(inner, subs),
        TypeKind::Refined { base, pred } => {
            subst_type(base, subs);
            subst_expr(pred, subs);
        }
        TypeKind::Unit | TypeKind::Meta | TypeKind::Error => {}
    }
}

/// Rewrite a block in place.
fn subst_block(b: &mut Block, subs: &Subs) {
    for s in &mut b.stmts {
        subst_stmt(s, subs);
    }
    if let Some(t) = &mut b.trailing {
        subst_expr(t, subs);
    }
}

/// Rewrite a statement in place.
fn subst_stmt(s: &mut Stmt, subs: &Subs) {
    match &mut s.kind {
        StmtKind::Let { pat, ty, init, .. } => {
            subst_pat(pat, subs);
            if let Some(t) = ty {
                subst_type(t, subs);
            }
            if let Some(e) = init {
                subst_expr(e, subs);
            }
        }
        StmtKind::Assign { target, rhs, .. } => {
            subst_expr(target, subs);
            subst_expr(rhs, subs);
        }
        StmtKind::Expr(e) => subst_expr(e, subs),
    }
}

/// Rewrite a pattern in place (variant / struct type paths).
fn subst_pat(p: &mut Pat, subs: &Subs) {
    match &mut p.kind {
        PatKind::Variant { path, payload } => {
            subst_path(path, subs);
            match payload {
                VariantPatPayload::None => {}
                VariantPatPayload::Tuple(pats) => {
                    for inner in pats {
                        subst_pat(inner, subs);
                    }
                }
                VariantPatPayload::Struct(fields) => {
                    for f in fields {
                        subst_pat(&mut f.pat, subs);
                    }
                }
            }
        }
        PatKind::Struct { path, fields, .. } => {
            subst_path(path, subs);
            for f in fields {
                subst_pat(&mut f.pat, subs);
            }
        }
        PatKind::Tuple(pats) => {
            for inner in pats {
                subst_pat(inner, subs);
            }
        }
        PatKind::Guard { pat, cond } => {
            subst_pat(pat, subs);
            subst_expr(cond, subs);
        }
        PatKind::AtBinding { inner, .. } => subst_pat(inner, subs),
        PatKind::Slice {
            prefix,
            suffix,
            ..
        } => {
            for inner in prefix {
                subst_pat(inner, subs);
            }
            for inner in suffix {
                subst_pat(inner, subs);
            }
        }
        // Range bounds are literals; nothing to substitute.
        PatKind::Range { .. }
        | PatKind::Wildcard
        | PatKind::Binding(_)
        | PatKind::Literal(_)
        | PatKind::Error => {}
    }
}

/// Rewrite an expression in place.
fn subst_expr(e: &mut Expr, subs: &Subs) {
    // A structural bound (`Unit` / `Tuple`) at a bound-generic head in an
    // expression position (only comptime-builtin arguments survive here)
    // is spliced as a whole-node tuple-composite sentinel; compute it
    // first so the immutable borrow of `e.kind` ends before the assignment.
    let sentinel = match &e.kind {
        ExprKind::Path(p) => p
            .segments
            .first()
            .and_then(|s| structural_sentinel_kind(&s.name, subs)),
        _ => None,
    };
    if let Some(kind) = sentinel {
        e.kind = kind;
        return;
    }
    match &mut e.kind {
        ExprKind::FString(parts) => {
            for part in parts {
                if let edda_syntax::ast::FStringPart::Slot(slot) = part {
                    subst_expr(slot, subs);
                }
            }
        }
        ExprKind::Path(p) => {
            if subst_path(p, subs) && p.segments.len() == 1 {
                e.span = p.span;
            }
        }
        ExprKind::Literal(_) | ExprKind::Continue { .. } | ExprKind::Error => {}
        ExprKind::Binary { lhs, rhs, .. } => {
            subst_expr(lhs, subs);
            subst_expr(rhs, subs);
        }
        ExprKind::Unary { expr, .. } => subst_expr(expr, subs),
        ExprKind::Call { callee, args } => {
            subst_expr(callee, subs);
            for a in args {
                subst_expr(&mut a.expr, subs);
            }
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            subst_expr(receiver, subs);
            for a in args {
                subst_expr(&mut a.expr, subs);
            }
        }
        ExprKind::Field { receiver, .. } => subst_expr(receiver, subs),
        ExprKind::TupleIndex { receiver, .. } => subst_expr(receiver, subs),
        ExprKind::CompField { receiver, index } => {
            subst_expr(receiver, subs);
            subst_expr(index, subs);
        }
        ExprKind::Index { receiver, index } => {
            subst_expr(receiver, subs);
            subst_expr(index, subs);
        }
        ExprKind::If {
            cond,
            then_block,
            else_branch,
        } => {
            subst_expr(cond, subs);
            subst_block(then_block, subs);
            if let Some(eb) = else_branch {
                subst_expr(eb, subs);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            subst_expr(scrutinee, subs);
            for arm in arms {
                subst_pat(&mut arm.pat, subs);
                if let Some(g) = &mut arm.guard {
                    subst_expr(g, subs);
                }
                subst_expr(&mut arm.body, subs);
            }
        }
        ExprKind::Block(b) => subst_block(b, subs),
        ExprKind::Cast { expr, ty, .. } => {
            subst_expr(expr, subs);
            subst_type(ty, subs);
        }
        ExprKind::Range { lo, hi, .. } => {
            if let Some(l) = lo {
                subst_expr(l, subs);
            }
            if let Some(h) = hi {
                subst_expr(h, subs);
            }
        }
        ExprKind::Tuple(es) | ExprKind::Array(es) => {
            for inner in es {
                subst_expr(inner, subs);
            }
        }
        ExprKind::StructLit { path, fields } => {
            subst_path(path, subs);
            for f in fields {
                subst_expr(&mut f.value, subs);
            }
        }
        ExprKind::Loop {
            body, decreases, ..
        } => {
            subst_block(body, subs);
            if let Some(d) = decreases {
                subst_expr(d, subs);
            }
        }
        ExprKind::For {
            pat, iter, body, ..
        } => {
            subst_pat(pat, subs);
            subst_expr(iter, subs);
            subst_block(body, subs);
        }
        ExprKind::Try(inner)
        | ExprKind::Await(inner)
        | ExprKind::Raise(inner)
        | ExprKind::Panic(inner)
        | ExprKind::Comptime(inner) => subst_expr(inner, subs),
        ExprKind::ComptimeBlock(b) => subst_block(b, subs),
        ExprKind::Scope { body, .. } => subst_block(body, subs),
        ExprKind::Return(opt) => {
            if let Some(inner) = opt {
                subst_expr(inner, subs);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(inner) = value {
                subst_expr(inner, subs);
            }
        }
        ExprKind::EffectRow(_) => {}
        ExprKind::Closure(c) => {
            for p in &mut c.params {
                subst_type(&mut p.ty, subs);
            }
            subst_type(&mut c.ret, subs);
            subst_block(&mut c.body, subs);
        }
        ExprKind::Handle {
            ty, recovery, body, ..
        } => {
            if let Some(ty) = ty {
                subst_type(ty, subs);
            }
            subst_expr(recovery, subs);
            subst_block(body, subs);
        }
        ExprKind::Spawn(s) => {
            for a in &mut s.args {
                subst_expr(&mut a.init, subs);
            }
            subst_block(&mut s.body, subs);
        }
        ExprKind::Forall { iter, body, .. } | ExprKind::Exists { iter, body, .. } => {
            subst_expr(iter, subs);
            subst_expr(body, subs);
        }
    }
}
