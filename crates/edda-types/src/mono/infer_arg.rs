//! Structural declared-type inference for outbound-generic call
//! arguments (layer 2).
//!
//! Decides `U := <declared type of the argument>` without running type
//! inference: the admitted argument shapes are a name with a known
//! declared type (caller parameter or annotated `let`/`var` binding), a
//! field-projection chain rooted at one (stepped through the
//! [`ShapeIndex`]'s per-field declared-type leafs — the same ground
//! truth the `comptime if` evaluator folds against), and — for the
//! D-22 sum fan-out — a positional
//! `TupleIndex` read of a sum-typed receiver's variant payload composite
//! or a tuple-typed receiver's element. Every decision returns the
//! [`BoundTy`] reference form of an actual source type, so the
//! substituted references resolve through the existing span-keyed
//! `Resolutions` map.

use ahash::AHashMap;
use edda_intern::Symbol;
use edda_syntax::ast::{Expr, ExprKind, Param, Pat, PatKind, Stmt, StmtKind, Type, TypeKind};

use super::{member_bound_ty, BoundTy};
use crate::comptime_expand::ShapeIndex;

/// Flat declared-type environment for one function body.
///
/// Shadowing nuance: later `let`s overwrite earlier entries in walk
/// order. The walk is pre-order, so a shadowed name inside a nested
/// block can leak its type to later sibling uses — acceptable for the
/// conservative v1 surface (a wrong leaf makes specialization resolve
/// to a different concrete type, which then fails arg checking with a
/// precise type-mismatch diagnostic rather than miscompiling).
pub(super) struct TypeLeafEnv {
    entries: AHashMap<Symbol, BoundTy>,
}

impl TypeLeafEnv {
    /// Build the environment from a function's parameters; `let`
    /// annotations are added lazily by the rewrite walk.
    pub(super) fn from_params(params: &[Param]) -> Self {
        let mut entries = AHashMap::default();
        for p in params {
            if let Some(bt) = type_to_bound(&p.ty) {
                entries.insert(p.name.name, bt);
            }
        }
        Self { entries }
    }

    /// Record an annotated `let`/`var` statement's binding type.
    pub(super) fn observe_stmt(&mut self, s: &Stmt) {
        if let StmtKind::Let { pat, ty: Some(t), .. } = &s.kind
            && let edda_syntax::ast::PatKind::Binding(name) = &pat.kind
            && let Some(bt) = type_to_bound(t)
        {
            self.entries.insert(name.name, bt);
        }
    }

    /// Record a `for <pat> in <iter>` element binding when the iterand's
    /// declared type is a slice `[E]`.
    /// The element binding `elem` then carries `E` (a plain named leaf),
    /// so a call `hash_value(elem)` inside the body infers `U := E`. Only
    /// a simple binding pattern over a slice-typed iterand is recorded;
    /// destructuring patterns and non-slice iterands are left unbound,
    /// which makes inference through them fail conservatively — the same
    /// discipline `observe_stmt` follows for `let` annotations.
    pub(super) fn observe_for(&mut self, pat: &Pat, iter: &Expr, shapes: &ShapeIndex) {
        let PatKind::Binding(name) = &pat.kind else {
            return;
        };
        if let Some(BoundTy::Slice(leaf, span)) = declared_leaf_of_expr(iter, self, shapes) {
            self.entries.insert(name.name, BoundTy::Named(leaf, span));
        }
    }

    fn lookup(&self, name: Symbol) -> Option<BoundTy> {
        self.entries.get(&name).cloned()
    }
}

/// The [`BoundTy`] of a declared type annotation admitted into the env:
/// a plain named path, the unit type `()`, a flat tuple of plain paths
/// (the tuple-U specializations' payload-composite parameter
/// annotations), or a slice `[E]` of a plain path (the slice-U
/// specializations' parameter annotation).
/// A non-path tuple/slice element or any richer structural form is not
/// expressible and yields `None`.
fn type_to_bound(ty: &Type) -> Option<BoundTy> {
    match &ty.kind {
        TypeKind::Path(tp) => tp.segments.last().map(|s| BoundTy::Named(s.name, tp.span)),
        TypeKind::Unit => Some(BoundTy::Unit),
        TypeKind::Tuple(elems) => {
            let mut refs = Vec::with_capacity(elems.len());
            for el in elems {
                let TypeKind::Path(pp) = &el.kind else {
                    return None;
                };
                let seg = pp.segments.last()?;
                refs.push((seg.name, pp.span));
            }
            Some(BoundTy::Tuple(refs))
        }
        TypeKind::Slice(inner) => {
            let TypeKind::Path(pp) = &inner.kind else {
                return None;
            };
            let seg = pp.segments.last()?;
            Some(BoundTy::Slice(seg.name, pp.span))
        }
        _ => None,
    }
}

/// Infer the declared [`BoundTy`] of one call argument.
///
/// Admitted shapes:
/// - `name` — a parameter or annotated local,
/// - `name.f1.f2…` (parsed as a multi-segment `Path`) and explicit
///   `Field` chains rooted at one (the record field-walk shape),
/// - `recv.i` (`TupleIndex`, the shape the D-22 sum-loop `CompField`
///   fold produces) — variant-`i` payload composite of a sum receiver,
///   or element `i` of a tuple receiver.
pub(super) fn declared_leaf_of_expr(
    expr: &Expr,
    env: &TypeLeafEnv,
    shapes: &ShapeIndex,
) -> Option<BoundTy> {
    match &expr.kind {
        ExprKind::Path(p) => {
            let head = p.segments.first()?;
            let mut cur = env.lookup(head.name)?;
            for seg in &p.segments[1..] {
                cur = field_step(&cur, seg.name, shapes)?;
            }
            Some(cur)
        }
        ExprKind::Field { receiver, name } => {
            let recv = declared_leaf_of_expr(receiver, env, shapes)?;
            field_step(&recv, name.name, shapes)
        }
        // `v[i]` element access over a slice-typed receiver `[E]` reads
        // the element type `E` — the second half of slice inference
        // (the field-projection half that
        // binds `U := [E]`). Only a `Slice` receiver has
        // an element leaf; any other receiver bound is not element-typed.
        ExprKind::Index { receiver, .. } => {
            let recv = declared_leaf_of_expr(receiver, env, shapes)?;
            match recv {
                BoundTy::Slice(elem_leaf, elem_span) => Some(BoundTy::Named(elem_leaf, elem_span)),
                BoundTy::Named(..) | BoundTy::Tuple(..) | BoundTy::Unit => None,
            }
        }
        // `x.(i)` folds (post-expansion) to a positional `TupleIndex`
        // against a sum-typed receiver (variant-`i` payload composite,
        // the D-22 READ surface) or a tuple-typed receiver (element `i`,
        // the walk inside a tuple-U specialization).
        ExprKind::TupleIndex { receiver, index } => {
            let recv = declared_leaf_of_expr(receiver, env, shapes)?;
            let i = *index as usize;
            match recv {
                BoundTy::Named(leaf, _) => {
                    let shape = shapes.get(&leaf)?;
                    // Positional projection is a variant-payload read; a
                    // product's fields are named, not positionally indexed.
                    if !shape.is_sum {
                        return None;
                    }
                    member_bound_ty(shape.member_tys.get(i)?)
                }
                BoundTy::Tuple(refs) => {
                    let (elem_leaf, elem_span) = refs.get(i)?;
                    Some(BoundTy::Named(*elem_leaf, *elem_span))
                }
                // A slice bound has no positional comptime fields — a
                // `recv.i` read over `[E]` is not decidable here.
                BoundTy::Unit | BoundTy::Slice(..) => None,
            }
        }
        _ => None,
    }
}

/// One named-field step: the declared [`BoundTy`] of field `field` on
/// the product type named by the `Named` receiver `recv`. `None` for a
/// sum receiver (its members are not name-projected), a non-`Named`
/// receiver, an unknown field, or a field type not expressible as a
/// bound type.
fn field_step(recv: &BoundTy, field: Symbol, shapes: &ShapeIndex) -> Option<BoundTy> {
    let BoundTy::Named(recv_leaf, _) = recv else {
        return None;
    };
    let shape = shapes.get(recv_leaf)?;
    if shape.is_sum {
        return None;
    }
    let idx = shape.members.iter().position(|m| m.name == field)?;
    member_bound_ty(shape.member_tys.get(idx)?)
}
