//! The cloning walker that unrolls `comptime for`, folds `CompField`,
//! and elides `comptime if` dead branches.
//!
//! [`Expander`] walks one function body, threading an [`Env`] that names
//! the enclosing `comptime for` loop variable + its current literal index
//! + the construction target's field names. It substitutes loop-variable
//! uses for their literal index, folds constant-indexed `CompField`s into
//! named `Field` / variant-path accesses, splices the live arm of any
//! decidable `comptime if` (see [`super::comptime_if`]), folds a bare
//! `field_count(T)` call to its literal member count wherever it appears
//! — not only as a loop bound — and
//! otherwise recurses structurally. [`UnrollPlan`] is the resolved
//! `[lo, hi)` iteration plan for one recognised `field_count(T)`-bounded
//! loop.

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, Severity};
use edda_intern::{Interner, Symbol};
use edda_span::Span;
use edda_syntax::IntBase;
use edda_syntax::ast::{
    Block, Expr, ExprKind, Ident, Literal, Pat, PatKind, Path, RangeKind, Stmt, StmtKind,
};

use super::ShapeIndex;

/// Loop-variable binding environment threaded through one unrolled
/// iteration: the loop var's `Symbol`, its current literal index, the
/// construction target's member names, and whether the loop's
/// `field_count(T)` target walks positionally (`positional` selects the
/// value-receiver `CompField` fold — a `TupleIndex` for a sum's
/// variant-payload read or a tuple composite's element, a named-field
/// access for a product; D-22 READ surface, extended to tuple composites).
pub(super) enum Env<'a> {
    None,
    Loop { var: Symbol, index: u128, fields: &'a [Ident], positional: bool },
}

pub(super) struct Expander<'a> {
    pub(super) shapes: &'a ShapeIndex,
    pub(super) interner: &'a Interner,
    /// Active build target — folds `comptime if target_has("…")` conditions.
    pub(super) target: &'a edda_target::TargetCfg,
}

impl<'a> Expander<'a> {
    pub(super) fn block(&self, b: &Block, env: &Env<'_>, diags: &mut Diagnostics) -> Block {
        let mut stmts = Vec::with_capacity(b.stmts.len());
        for s in &b.stmts {
            self.stmt_into(s, env, &mut stmts, diags);
        }
        Block {
            span: b.span,
            stmts,
            trailing: b.trailing.as_ref().map(|e| Box::new(self.expr(e, env, diags))),
        }
    }

    /// Lower one statement into `out`. A `comptime for` over
    /// `field_count(T)` expands into one block statement per field;
    /// every other statement is transformed in place.
    fn stmt_into(
        &self,
        s: &Stmt,
        env: &Env<'_>,
        out: &mut Vec<Stmt>,
        diags: &mut Diagnostics,
    ) {
        if let StmtKind::Expr(e) = &s.kind
            && let ExprKind::Comptime(inner) = &e.kind
            && let ExprKind::For { pat, iter, body, .. } = &inner.kind
            && let Some(plan) = self.plan_unroll(pat, iter, diags)
        {
            for index in plan.indices() {
                let iter_env = Env::Loop {
                    var: plan.var,
                    index,
                    fields: &plan.fields,
                    positional: plan.positional,
                };
                // Flatten each iteration's statements directly into the
                // enclosing scope (no `{ }` wrapper) so field assignments
                // to an outer `uninit out` drive its `Uninit → PartialInit
                // → Valid` typestate transition in the function's own
                // scope — matching the locked staged-init discipline
                // (02-modes §typestate). Construction bodies assign fields
                // and declare no per-iteration locals, so flattening is
                // sound; a trailing expression (rare for a unit-typed loop
                // body) is preserved as an expression statement.
                let expanded = self.block(body, &iter_env, diags);
                out.extend(expanded.stmts);
                if let Some(trailing) = expanded.trailing {
                    out.push(Stmt {
                        span: trailing.span,
                        attributes: Vec::new(),
                        kind: StmtKind::Expr(*trailing),
                    });
                }
            }
            return;
        }
        // `comptime if` in statement position: splice the live arm's
        // statements directly into the enclosing scope (same flattening
        // rationale as the `comptime for` unroll above — staged-init
        // field assignments must transition the outer binding's
        // typestate in the function's own scope). An undecidable
        // condition is a hard error: a
        // `comptime if` must be compile-time decidable, and silently
        // degrading it to a runtime `if` leaked the dead arm's effect
        // row into the function's declared effects.
        if let StmtKind::Expr(e) = &s.kind
            && let ExprKind::Comptime(inner) = &e.kind
            && let ExprKind::If { cond, then_block, else_branch } = &inner.kind
        {
            match self.fold_comptime_if(e.span, cond, then_block, else_branch.as_deref(), env, diags)
            {
                Some(folded) => {
                    if let ExprKind::Block(b) = folded.kind {
                        out.extend(b.stmts);
                        if let Some(trailing) = b.trailing {
                            out.push(Stmt {
                                span: trailing.span,
                                attributes: Vec::new(),
                                kind: StmtKind::Expr(*trailing),
                            });
                        }
                    } else {
                        out.push(Stmt {
                            span: folded.span,
                            attributes: s.attributes.clone(),
                            kind: StmtKind::Expr(folded),
                        });
                    }
                    return;
                }
                // Keep the node (fall through to the structural rebuild
                // below) so a well-formed tree reaches AST→HIR; the
                // emitted error already fails the build.
                None => self.reject_comptime_if_undecidable(inner.span, diags),
            }
        }
        out.push(self.stmt(s, env, diags));
    }

    fn stmt(&self, s: &Stmt, env: &Env<'_>, diags: &mut Diagnostics) -> Stmt {
        let kind = match &s.kind {
            StmtKind::Let { mutability, pat, ty, init } => StmtKind::Let {
                mutability: *mutability,
                pat: pat.clone(),
                ty: ty.clone(),
                init: init.as_ref().map(|e| self.expr(e, env, diags)),
            },
            StmtKind::Assign { target, op, rhs } => StmtKind::Assign {
                target: self.expr(target, env, diags),
                op: *op,
                rhs: self.expr(rhs, env, diags),
            },
            StmtKind::Expr(e) => StmtKind::Expr(self.expr(e, env, diags)),
        };
        Stmt { span: s.span, attributes: s.attributes.clone(), kind }
    }

    /// Transform one expression under `env`: substitute the loop
    /// variable for its literal index, fold `CompField` whose index is a
    /// constant into a named `Field`, fold a bare `field_count(T)` call
    /// to its literal member count wherever it appears — not only as a
    /// `comptime for` bound — and
    /// recurse structurally otherwise.
    pub(super) fn expr(&self, e: &Expr, env: &Env<'_>, diags: &mut Diagnostics) -> Expr {
        // Loop-variable use → literal index.
        if let (ExprKind::Path(p), Env::Loop { var, index, .. }) = (&e.kind, env)
            && p.segments.len() == 1
            && p.segments[0].name == *var
        {
            return Expr { span: e.span, kind: ExprKind::Literal(int_lit(*index)) };
        }
        // `comptime if` in expression position → the live arm as a block
        // expression. An undecidable condition is a hard error;
        // the node is kept (structural
        // recursion below) so a well-formed tree reaches AST→HIR.
        if let ExprKind::Comptime(inner) = &e.kind
            && let ExprKind::If { cond, then_block, else_branch } = &inner.kind
        {
            if let Some(folded) =
                self.fold_comptime_if(e.span, cond, then_block, else_branch.as_deref(), env, diags)
            {
                return folded;
            }
            self.reject_comptime_if_undecidable(inner.span, diags);
        }
        // Comptime-indexed field access → named field once the index is
        // a constant in range.
        if let ExprKind::CompField { receiver, index } = &e.kind {
            let receiver = self.expr(receiver, env, diags);
            let index = self.expr(index, env, diags);
            return self.fold_comp_field(e.span, receiver, index, env, diags);
        }
        // Bare `field_count(T)` call in general expression position:
        // fold to the literal member
        // count, reusing the same ground truth a `comptime for` bound
        // already folds against ([`Expander::field_count_target`]). Left
        // unfolded when `T` isn't resolvable via [`ShapeIndex`] (still
        // abstract, or not a composite) so an unfoldable call surfaces
        // the same pre-existing diagnostic as today.
        if let Some((fields, _)) = self.field_count_target(e) {
            return Expr {
                span: e.span,
                kind: ExprKind::Literal(int_lit(fields.len() as u128)),
            };
        }
        Expr { span: e.span, kind: self.expr_kind(&e.kind, env, diags) }
    }

    /// Fold `receiver.(k)` once the index `k` is a constant (D-22 §4.5):
    ///
    /// - **Variant construction** — `receiver` names a sum type `T`:
    ///   rewrite `T.(k)` to the variant path `T.<variant_name_at(T, k)>`
    ///   (the surrounding `Call`, if any, then supplies the payload —
    ///   `T.(k)(p)` → `T.<variant>(p)`).
    /// - **Positional read/write** — `receiver` is a value and the
    ///   enclosing `comptime for` walks a sum's or tuple composite's
    ///   `field_count(T)`: rewrite `v.(k)` / `out.(k)` to the positional
    ///   form `v.k` ([`ExprKind::TupleIndex`]), which inference + MIR
    ///   lowering resolve as the variant-`k` payload projection (sum,
    ///   the D-22 READ surface —
    ///   mirrors the native's `variant_payload` place projection) or the
    ///   element-`k` projection (tuple composite).
    /// - **Record field access** — `receiver` is a value (the enclosing
    ///   `comptime for` construction target is a product): rewrite
    ///   `out.(k)` to `out.<field_name_at(T, k)>` using the loop's
    ///   field names.
    ///
    /// On any failure (non-constant index, out-of-range, no field set in
    /// scope) emit a diagnostic and leave the `CompField` in place — it
    /// becomes the `HirExprKind::Error` sentinel at AST→HIR lowering.
    fn fold_comp_field(
        &self,
        span: Span,
        receiver: Expr,
        index: Expr,
        env: &Env<'_>,
        diags: &mut Diagnostics,
    ) -> Expr {
        let keep = |recv: Expr, idx: Expr| Expr {
            span,
            kind: ExprKind::CompField { receiver: Box::new(recv), index: Box::new(idx) },
        };
        let ExprKind::Literal(Literal::Int { value, .. }) = &index.kind else {
            self.reject(span, "comptime construction index did not reduce to a constant", diags);
            return keep(receiver, index);
        };
        let Ok(k) = usize::try_from(*value) else {
            self.reject(span, "comptime construction index out of range", diags);
            return keep(receiver, index);
        };
        // Variant construction: the receiver names a sum type. `T.(k)`
        // becomes the qualified variant path `T.<variant_name>`.
        if let ExprKind::Path(p) = &receiver.kind
            && let Some(leaf) = p.segments.last()
            && let Some(shape) = self.shapes.get(&leaf.name)
            && shape.is_sum
        {
            let Some(vname) = shape.members.get(k) else {
                self.reject(span, "comptime variant index out of range for the type's variant count", diags);
                return keep(receiver, index);
            };
            // Reuse the receiver path's span: name resolution already
            // recorded `T` (the head type) at that span, and the
            // variant-constructor call rule (`infer/call`) looks the
            // callee path up by span → head-type binding, then reads the
            // *last* segment as the variant. Synthesising a fresh span
            // would have no recorded resolution. Append the variant
            // segment so `T.(k)` becomes `T.<variant>`.
            let path_span = p.span;
            let mut segments = p.segments.clone();
            segments.push(*vname);
            return Expr {
                span: path_span,
                kind: ExprKind::Path(Path { segments, span: path_span }),
            };
        }
        // Value receiver: fold against the enclosing loop's member set.
        let Env::Loop { fields, positional, .. } = env else {
            self.reject(span, "comptime-indexed field access `.(i)` outside a `comptime for` (and receiver is not a sum type)", diags);
            return keep(receiver, index);
        };
        let Some(name) = fields.get(k) else {
            self.reject(span, "comptime field index out of range for the type's field count", diags);
            return keep(receiver, index);
        };
        // Positional loop (sum or tuple composite): `v.(k)` / `out.(k)` on
        // a value is the positional element-`k` access — variant payloads
        // and tuple elements have no field name.
        if *positional {
            let Ok(idx) = u32::try_from(k) else {
                self.reject(span, "comptime variant index exceeds the positional-access range", diags);
                return keep(receiver, index);
            };
            return Expr {
                span,
                kind: ExprKind::TupleIndex { receiver: Box::new(receiver), index: idx },
            };
        }
        Expr {
            span,
            kind: ExprKind::Field { receiver: Box::new(receiver), name: *name },
        }
    }

    /// Emit a comptime-expansion hard error at `span`. Used for an
    /// unfoldable `CompField` (which would otherwise reach AST→HIR as
    /// the `Error` sentinel with no explanation) and for an undecidable
    /// `comptime if` condition — both are diagnosed here, where the
    /// comptime context is still known.
    fn reject(&self, span: Span, msg: &str, diags: &mut Diagnostics) {
        diags.push(Diagnostic::new(
            DiagnosticClass::TypecheckError,
            Severity::Error,
            span,
            msg.to_string(),
        ));
    }

    /// Reject an undecidable `comptime if` condition.
    /// A `comptime if` must be
    /// compile-time decidable; before this guard an undecidable
    /// condition silently degraded to a runtime `if`, so the dead arm
    /// was typechecked and its effect row leaked into the function's
    /// declared effects.
    fn reject_comptime_if_undecidable(&self, span: Span, diags: &mut Diagnostics) {
        self.reject(
            span,
            "comptime if condition is not comptime-decidable: it must reduce to a \
             constant bool from the type predicates (is_signed / is_integer / …), \
             type equality (== / !=), or boolean composition (! / && / ||) — a \
             comptime-pure call this pass cannot fold is not admitted here",
            diags,
        );
    }

    /// Recognise `comptime for <i> in <lo>..<field_count(T)>` and resolve
    /// the unroll plan: loop variable, half-open/closed range, low bound,
    /// and `T`'s field names. Returns `None` for any loop whose iterator
    /// is not a `field_count(T)`-bounded range (those are left for a
    /// later, more general expansion).
    fn plan_unroll(
        &self,
        pat: &Pat,
        iter: &Expr,
        diags: &mut Diagnostics,
    ) -> Option<UnrollPlan> {
        let PatKind::Binding(var_ident) = &pat.kind else {
            return None;
        };
        let ExprKind::Range { lo, hi, kind } = &iter.kind else {
            return None;
        };
        let lo_val = match lo.as_deref() {
            None => 0,
            Some(e) => const_usize(e)?,
        };
        let hi_expr = hi.as_deref()?;
        let (fields, positional) = self.field_count_target(hi_expr)?;
        let count = fields.len() as u128;
        let hi_val = match kind {
            RangeKind::HalfOpen => count,
            RangeKind::Closed => count.saturating_add(1),
        };
        if lo_val != 0 {
            self.reject(
                iter.span,
                "comptime for over field_count(T) must start at 0",
                diags,
            );
            return None;
        }
        Some(UnrollPlan { var: var_ident.name, lo: lo_val, hi: hi_val, fields, positional })
    }

    /// If `e` is a `field_count(T)` call, return the walk's member names
    /// plus whether it is positional (see [`Expander::fold_comp_field`]):
    ///
    /// - a named type in the [`ShapeIndex`] → its member names (fields for
    ///   a product, variants for a sum); positional for a sum, named for a
    ///   product,
    /// - a tuple-composite sentinel `(A, B, …)` / `()` (a structural bound
    ///   spliced for `field_count(U)`) →
    ///   the element type-path idents as placeholder members (unused by
    ///   the positional fold), always positional.
    ///
    /// `field_count(T)` is the member/element count in every case. Used
    /// both to resolve a `comptime for`'s loop bound ([`Expander::plan_unroll`])
    /// and to fold a bare
    /// `field_count(T)` call anywhere else in the body ([`Expander::expr`]).
    fn field_count_target(&self, e: &Expr) -> Option<(Vec<Ident>, bool)> {
        let ExprKind::Call { callee, args } = &e.kind else {
            return None;
        };
        let ExprKind::Path(callee_path) = &callee.kind else {
            return None;
        };
        let last = callee_path.segments.last()?;
        if self.interner.resolve(last.name) != "field_count" {
            return None;
        }
        let arg = args.first()?;
        match &arg.expr.kind {
            ExprKind::Path(arg_path) => {
                let ty_leaf = arg_path.segments.last()?;
                self.shapes.get(&ty_leaf.name).map(|s| (s.members.clone(), s.is_sum))
            }
            // Tuple-composite sentinel: positional walk of `elems.len()`
            // members. Reuse each element's type-path leaf as a
            // placeholder member ident (the positional fold ignores names).
            ExprKind::Tuple(elems) => {
                let mut members = Vec::with_capacity(elems.len());
                for el in elems {
                    let ExprKind::Path(pp) = &el.kind else {
                        return None;
                    };
                    members.push(*pp.segments.last()?);
                }
                Some((members, true))
            }
            _ => None,
        }
    }
}

/// A resolved unroll: the loop variable, `[lo, hi)` index range, the
/// construction target's member names, and whether the target walks
/// positionally (a sum's variant payloads or a tuple composite's
/// elements) rather than by field name (a product).
struct UnrollPlan {
    var: Symbol,
    lo: u128,
    hi: u128,
    fields: Vec<Ident>,
    positional: bool,
}

impl UnrollPlan {
    fn indices(&self) -> impl Iterator<Item = u128> {
        self.lo..self.hi
    }
}

fn int_lit(value: u128) -> Literal {
    Literal::Int { value, base: IntBase::Dec }
}

/// Evaluate a trivially-constant `usize` expression (a non-negative
/// decimal integer literal). Returns `None` for anything else — the
/// pass only needs `0` as a low bound.
fn const_usize(e: &Expr) -> Option<u128> {
    match &e.kind {
        ExprKind::Literal(Literal::Int { value, .. }) => Some(*value),
        _ => None,
    }
}
