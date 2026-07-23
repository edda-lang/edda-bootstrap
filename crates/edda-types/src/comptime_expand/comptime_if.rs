//! `comptime if` dead-branch elision for the [`Expander`].
//!
//! Per `corpus/edda-codex/language/04-specs-comptime.md`, the dead branch
//! of a `comptime if` is elided **before** typecheck — the unsupported
//! arm may reference absent capabilities or type-incompatible forms, and
//! it must contribute nothing to the function's effect row. This module
//! holds the condition evaluator and the live-arm splice.
//!
//! The evaluator is deliberately partial: it decides the per-field
//! category-dispatch fragment the serde/properties walk needs (the seven
//! `is_*` predicates and `==`/`!=` over type values, where a type value
//! is a named type or `field_type_at(T, k)` with a constant `k`), plus
//! `!`/`&&`/`||` composition and bool literals. A condition outside that
//! fragment returns `None`, and the caller raises a hard error rather
//! than degrading the `comptime if` to a runtime `if`:
//! a `comptime if` must be compile-time
//! decidable, so an undecidable condition is a defect — the prior
//! silent fall-through let the dead arm's effect row leak into the
//! function's declared effects.

use edda_diag::Diagnostics;
use edda_intern::Symbol;
use edda_span::Span;
use edda_syntax::ast::{BinOp, Block, Expr, ExprKind, Literal, UnOp};
use edda_target::TargetHas;

use super::expander::{Env, Expander};
use super::MemberTy;
use crate::Primitive;

/// Recursion cap for [`Expander::ty_val_is_copy`]'s member walk.
const IS_COPY_MAX_DEPTH: usize = 64;

/// A comptime-evaluated type value: a locked primitive (a unit variant's
/// payload composite `()` rides `Prim(Primitive::Unit)`), a user type
/// known to the [`super::ShapeIndex`] by its leaf-name `Symbol`, or a
/// sum-variant payload composite (the D-22 payload-tuple convention)
/// with per-element values — `None` for
/// an element this pass cannot classify.
#[derive(Clone)]
enum TyVal {
    Prim(Primitive),
    User(Symbol),
    Tuple(Vec<Option<TyVal>>),
    /// A slice `[E]` — a spliced slice-composite sentinel
    /// (`ExprKind::Array`) or a `Slice` member type. `is_container`
    /// folds `true` over it (a slice is walked element-by-element, not
    /// field-by-field); the six numeric/primitive `is_*` predicates and
    /// `is_sum` fold `false`.
    Slice,
}

/// Structural type-value equality for `==` / `!=` folding.
fn ty_val_eq(l: &TyVal, r: &TyVal) -> Option<bool> {
    match (l, r) {
        (TyVal::Prim(a), TyVal::Prim(b)) => Some(a == b),
        (TyVal::User(a), TyVal::User(b)) => Some(a == b),
        (TyVal::Tuple(a), TyVal::Tuple(b)) => {
            if a.len() != b.len() {
                return Some(false);
            }
            let mut undecided = false;
            for (x, y) in a.iter().zip(b) {
                match (x, y) {
                    (Some(xv), Some(yv)) => match ty_val_eq(xv, yv) {
                        Some(false) => return Some(false),
                        Some(true) => {}
                        None => undecided = true,
                    },
                    _ => undecided = true,
                }
            }
            if undecided { None } else { Some(true) }
        }
        _ => Some(false),
    }
}

impl<'a> Expander<'a> {
    /// Fold one `comptime if` (the `If` directly under an
    /// `ExprKind::Comptime`): evaluate the condition and return the live
    /// arm as a block expression, recursing through `else if` chains.
    /// Returns `None` when any condition on the selected path is
    /// comptime-undecidable — the caller then raises a hard error,
    /// since a `comptime if` must be
    /// compile-time decidable.
    pub(super) fn fold_comptime_if(
        &self,
        span: Span,
        cond: &Expr,
        then_block: &Block,
        else_branch: Option<&Expr>,
        env: &Env<'_>,
        diags: &mut Diagnostics,
    ) -> Option<Expr> {
        if self.eval_cond(cond, env)? {
            return Some(Expr {
                span,
                kind: ExprKind::Block(self.block(then_block, env, diags)),
            });
        }
        let Some(else_expr) = else_branch else {
            // `comptime if` with no `else` and a false condition: the
            // whole form vanishes — splice an empty (unit) block.
            return Some(Expr {
                span,
                kind: ExprKind::Block(Block { span, stmts: Vec::new(), trailing: None }),
            });
        };
        match &else_expr.kind {
            ExprKind::Block(b) => Some(Expr {
                span: else_expr.span,
                kind: ExprKind::Block(self.block(b, env, diags)),
            }),
            // `else if` chain: the whole chain sits under one `comptime`,
            // so the next condition must also fold.
            ExprKind::If { cond, then_block, else_branch } => self.fold_comptime_if(
                else_expr.span,
                cond,
                then_block,
                else_branch.as_deref(),
                env,
                diags,
            ),
            _ => Some(self.expr(else_expr, env, diags)),
        }
    }

    /// Evaluate a comptime-`if` condition to a constant bool. `None`
    /// means "not decidable by this pass"; the caller turns an
    /// undecidable `comptime if` condition into a hard error.
    fn eval_cond(&self, e: &Expr, env: &Env<'_>) -> Option<bool> {
        match &e.kind {
            ExprKind::Comptime(inner) => self.eval_cond(inner, env),
            ExprKind::Literal(Literal::Bool(b)) => Some(*b),
            ExprKind::Unary { op: UnOp::Not, expr } => Some(!self.eval_cond(expr, env)?),
            ExprKind::Binary { op: BinOp::And, lhs, rhs } => {
                match (self.eval_cond(lhs, env), self.eval_cond(rhs, env)) {
                    (Some(false), _) | (_, Some(false)) => Some(false),
                    (Some(true), Some(true)) => Some(true),
                    _ => None,
                }
            }
            ExprKind::Binary { op: BinOp::Or, lhs, rhs } => {
                match (self.eval_cond(lhs, env), self.eval_cond(rhs, env)) {
                    (Some(true), _) | (_, Some(true)) => Some(true),
                    (Some(false), Some(false)) => Some(false),
                    _ => None,
                }
            }
            ExprKind::Binary { op: op @ (BinOp::Eq | BinOp::Ne), lhs, rhs } => {
                let l = self.eval_type(lhs, env)?;
                let r = self.eval_type(rhs, env)?;
                let eq = ty_val_eq(&l, &r)?;
                Some(if matches!(op, BinOp::Eq) { eq } else { !eq })
            }
            ExprKind::Call { callee, args } => {
                let ExprKind::Path(p) = &callee.kind else {
                    return None;
                };
                let name = self.interner.resolve(p.segments.last()?.name);
                // `target_has("feature")` — a string-arg comptime-pure call
                // answered by the active target. `Unknown` (feature outside
                // the locked catalogue) stays undecidable, so the caller
                // raises the hard error rather than silently taking a branch.
                if name == "target_has" {
                    let ExprKind::Literal(Literal::Str(sym)) = &args.first()?.expr.kind else {
                        return None;
                    };
                    let feature = self.interner.resolve(*sym);
                    return match self.target.target_has(feature) {
                        TargetHas::Yes => Some(true),
                        TargetHas::No => Some(false),
                        TargetHas::Unknown => None,
                    };
                }
                // `target.supports(Cap)` — capability-availability gate.
                // Two-segment callee
                // `target.supports`; the argument is a bare capability
                // type-name. A non-capability argument leaves the
                // condition undecidable (`None`), so the caller raises
                // the hard error rather than silently taking a branch.
                if name == "supports"
                    && p.segments.len() == 2
                    && self.interner.resolve(p.segments[0].name) == "target"
                {
                    let ExprKind::Path(cap_path) = &args.first()?.expr.kind else {
                        return None;
                    };
                    if cap_path.segments.len() != 1 {
                        return None;
                    }
                    let cap_name = self.interner.resolve(cap_path.segments[0].name);
                    let cap = crate::capability::CapabilityType::from_name(cap_name)?;
                    return Some(self.target.triple().supports_capability(cap.name()));
                }
                // `is_sum(T)` needs the [`super::ShapeIndex`] (a user
                // type's product-vs-sum shape is not visible on the
                // [`TyVal`] alone), so it dispatches here instead of
                // through [`eval_is_predicate`] (D-22 READ surface).
                if name == "is_sum" {
                    let arg = self.eval_type(&args.first()?.expr, env)?;
                    return match arg {
                        TyVal::Prim(_) | TyVal::Tuple(_) | TyVal::Slice => Some(false),
                        TyVal::User(sym) => Some(self.shapes.get(&sym)?.is_sum),
                    };
                }
                // `is_copy(T)` needs the same [`super::ShapeIndex`] as
                // `is_sum` (a user type's field/variant types and
                // linearity are not visible on the [`TyVal`] alone), so
                // it dispatches here too rather than through
                // [`eval_is_predicate`].
                // Always decidable (`Some`, never `None`) — an
                // undecidable `comptime if` condition is a hard error,
                // and the only sound default for a fragment this
                // pass cannot positively verify copy-safe is `false`
                // (steering the caller to its always-correct slow path),
                // never a build failure.
                if name == "is_copy" {
                    let arg = self.eval_type(&args.first()?.expr, env)?;
                    return Some(self.ty_val_is_copy(&arg, 0));
                }
                // `is_container(T)` is the complement signal to `is_sum`:
                // a spec-instantiation-minted type (`Box_*`, `Vec_*`,
                // `Option_*`, map instances) surfaces here as a `User`
                // leaf whose mangled name is ABSENT from the
                // [`super::ShapeIndex`] — source `type` declarations are
                // the pass's only entries — so `shapes.get(&sym).is_none()`
                // is the decidable spec-instance predicate. Primitives,
                // tuples, and ShapeIndex-known nominals are not
                // containers. Always decidable (`Some`, never `None`),
                // unlike the shape-dependent `is_sum` whose `shapes`
                // lookup goes undecidable on the very same leaf — that
                // undecidability is exactly what this predicate lets
                // `std` guard around, steering the derived field-walk
                // away from container fields whose introspection does
                // not fold.
                if name == "is_container" {
                    let arg = self.eval_type(&args.first()?.expr, env)?;
                    return match arg {
                        TyVal::Prim(_) | TyVal::Tuple(_) => Some(false),
                        // A slice `[E]` is a built-in container: walked
                        // element-by-element, not field-by-field, so it
                        // rides the same derived-walk container arm a
                        // spec-instance (`Vec_*`/`Box_*`) does.
                        TyVal::Slice => Some(true),
                        TyVal::User(sym) => Some(self.shapes.get(&sym).is_none()),
                    };
                }
                let arg = self.eval_type(&args.first()?.expr, env)?;
                eval_is_predicate(name, arg)
            }
            _ => None,
        }
    }

    /// Evaluate a type-valued comptime expression: a path naming a
    /// primitive or a [`super::ShapeIndex`] type, or `field_type_at(T, k)`
    /// with a constant (or loop-variable) index.
    fn eval_type(&self, e: &Expr, env: &Env<'_>) -> Option<TyVal> {
        match &e.kind {
            ExprKind::Comptime(inner) => self.eval_type(inner, env),
            ExprKind::Path(p) => self.classify_leaf(p.segments.last()?.name),
            // Tuple-composite sentinel: the
            // substitution walker splices a structural bound generic as a
            // tuple expression in comptime-builtin argument positions. An
            // empty tuple is the unit composite `()` — it rides
            // `Prim(Unit)` so `is_primitive(())` folds true; a non-empty
            // tuple is the payload composite value.
            ExprKind::Tuple(elems) => {
                if elems.is_empty() {
                    return Some(TyVal::Prim(Primitive::Unit));
                }
                Some(TyVal::Tuple(
                    elems.iter().map(|el| self.eval_type(el, env)).collect(),
                ))
            }
            // Slice-composite sentinel:
            // the substitution walker splices a slice bound generic
            // `U := [E]` as a one-element array expression `[E]` in
            // comptime-builtin argument positions — distinct from the
            // tuple sentinel so `is_container(U)` folds `true` while the
            // numeric/primitive predicates fold `false`.
            ExprKind::Array(_) => Some(TyVal::Slice),
            ExprKind::Call { callee, args } => {
                let ExprKind::Path(cp) = &callee.kind else {
                    return None;
                };
                if self.interner.resolve(cp.segments.last()?.name) != "field_type_at" {
                    return None;
                }
                let recv = &args.first()?.expr;
                let k = self.eval_const_index(&args.get(1)?.expr, env)?;
                match &recv.kind {
                    // `field_type_at(T, k)` over a named type: the
                    // ShapeIndex member type (product field / sum payload
                    // composite).
                    ExprKind::Path(ty_path) => {
                        let shape = self.shapes.get(&ty_path.segments.last()?.name)?;
                        match shape.member_tys.get(k)? {
                            MemberTy::Named(leaf, _) => self.classify_leaf(*leaf),
                            MemberTy::Unit => Some(TyVal::Prim(Primitive::Unit)),
                            MemberTy::Tuple(elems) => Some(TyVal::Tuple(
                                elems
                                    .iter()
                                    .map(|e| e.and_then(|(leaf, _)| self.classify_leaf(leaf)))
                                    .collect(),
                            )),
                            MemberTy::Slice(_) => Some(TyVal::Slice),
                            MemberTy::Opaque => None,
                        }
                    }
                    // `field_type_at((A, B, …), k)` over a tuple-composite
                    // sentinel: element `k` — the walk inside a tuple-U
                    // specialization.
                    ExprKind::Tuple(elems) => self.eval_type(elems.get(k)?, env),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Classify a type leaf name: locked primitive spelling → `Prim`,
    /// anything else → `User`. A leaf
    /// outside the [`super::ShapeIndex`] — a spec-instance-generated
    /// type's mangled name (`Option_AvailFact_bc1b6d6b`), an alias this
    /// pass cannot see — is still a non-primitive named type, which is
    /// all the six `is_*` predicates need (`User` answers `false`
    /// uniformly). The shape-dependent queries stay conservative:
    /// `is_sum` short-circuits to undecidable on an unknown `User`
    /// symbol via its `shapes` lookup, and `==`/`!=` compare leaf
    /// identity — the same granularity `ShapeIndex` itself keys on.
    fn classify_leaf(&self, leaf: Symbol) -> Option<TyVal> {
        if let Some(p) = Primitive::from_name(self.interner.resolve(leaf)) {
            return Some(TyVal::Prim(p));
        }
        Some(TyVal::User(leaf))
    }

    /// `is_copy` folded over a [`TyVal`]:
    /// `true` for any primitive, a tuple of exclusively copy elements,
    /// or a [`ShapeIndex`]-known user type with no `linearity` of its
    /// own whose every member is itself copy. `false` — never a
    /// build-failing `None` — for anything this pass cannot positively
    /// verify (an unknown `User` leaf, a non-plain-path member type,
    /// depth exhaustion).
    fn ty_val_is_copy(&self, val: &TyVal, depth: usize) -> bool {
        if depth > IS_COPY_MAX_DEPTH {
            return false;
        }
        match val {
            TyVal::Prim(_) => true,
            TyVal::Tuple(elems) => elems.iter().all(|e| match e {
                Some(v) => self.ty_val_is_copy(v, depth + 1),
                None => false,
            }),
            TyVal::User(sym) => self.user_is_copy(*sym, depth),
            // A slice `[E]` is a fat-pointer view, not a bitwise-copyable
            // owned value — answer `false` so the caller keeps its
            // element-wise slow path.
            TyVal::Slice => false,
        }
    }

    /// `is_copy` for one [`ShapeIndex`]-known user type: `false` when
    /// the leaf is unknown to this pass or the declaration itself
    /// carries `linear`/`affine`, else every member's [`MemberTy`] must
    /// fold copy.
    fn user_is_copy(&self, sym: Symbol, depth: usize) -> bool {
        let Some(shape) = self.shapes.get(&sym) else {
            return false;
        };
        if shape.linearity.is_some() {
            return false;
        }
        shape
            .member_tys
            .iter()
            .all(|mt| self.member_ty_is_copy(mt, depth + 1))
    }

    /// `is_copy` for one product field's or sum variant payload
    /// element's [`MemberTy`]. `Opaque` (a structural field type this
    /// pass cannot resolve to a leaf name — tuple, slice, function, …)
    /// answers `false` conservatively rather than recursing further.
    fn member_ty_is_copy(&self, mt: &MemberTy, depth: usize) -> bool {
        match mt {
            MemberTy::Unit => true,
            MemberTy::Named(leaf, _) => self.classify_leaf(*leaf).is_some_and(|v| self.ty_val_is_copy(&v, depth)),
            MemberTy::Tuple(elems) => elems.iter().all(|e| match e {
                Some((leaf, _)) => self
                    .classify_leaf(*leaf)
                    .is_some_and(|v| self.ty_val_is_copy(&v, depth)),
                None => false,
            }),
            // A slice field `[E]` is a fat-pointer view — never a
            // bitwise-copyable value.
            MemberTy::Slice(_) => false,
            MemberTy::Opaque => false,
        }
    }

    /// Constant `usize` index: an integer literal or the enclosing
    /// `comptime for` loop variable at its current iteration index.
    fn eval_const_index(&self, e: &Expr, env: &Env<'_>) -> Option<usize> {
        match (&e.kind, env) {
            (ExprKind::Literal(Literal::Int { value, .. }), _) => usize::try_from(*value).ok(),
            (ExprKind::Path(p), Env::Loop { var, index, .. })
                if p.segments.len() == 1 && p.segments[0].name == *var =>
            {
                usize::try_from(*index).ok()
            }
            _ => None,
        }
    }
}

/// Apply one of the six locked `is_*` comptime predicates to a type
/// value. `None` for any other callee name.
fn eval_is_predicate(name: &str, arg: TyVal) -> Option<bool> {
    let prim = match arg {
        TyVal::Prim(p) => Some(p),
        TyVal::User(_) | TyVal::Tuple(_) | TyVal::Slice => None,
    };
    Some(match name {
        "is_signed" => prim.is_some_and(Primitive::is_signed_integer),
        "is_unsigned" => prim.is_some_and(Primitive::is_unsigned_integer),
        "is_integer" => prim.is_some_and(Primitive::is_integer),
        "is_floating" => prim.is_some_and(Primitive::is_float),
        "is_numeric" => prim.is_some_and(Primitive::is_numeric),
        "is_primitive" => prim.is_some(),
        _ => return None,
    })
}
