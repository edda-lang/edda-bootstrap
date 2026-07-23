//! `ExprKind` clone-and-rewrite arm for the substitution [`Walker`].
//!
//! Split out from `walk/mod.rs` for file-size reasons. This is the
//! per-`ExprKind`-variant deep-clone dispatch — every expression form
//! recurses into its subexpressions via the walker's shared `expr` /
//! `block` / `ty` / `pat` helpers. Path heads are rewritten upstream in
//! [`super::Walker::expr`]; this arm never re-tests path heads, so the
//! arm order and recursion shape are behaviourally inert.

use edda_syntax::ast::{ExprKind, FStringPart, SpawnArg, SpawnExpr};

use super::Walker;

impl<'a> Walker<'a> {
    pub(super) fn expr_kind(&self, k: &ExprKind) -> ExprKind {
        match k {
            ExprKind::Literal(lit) => ExprKind::Literal(*lit),
            ExprKind::FString(parts) => ExprKind::FString(
                parts
                    .iter()
                    .map(|p| match p {
                        FStringPart::Text(s) => FStringPart::Text(*s),
                        FStringPart::Slot(e) => FStringPart::Slot(Box::new(self.expr(e))),
                    })
                    .collect(),
            ),
            ExprKind::Path(p) => ExprKind::Path(p.clone()),
            ExprKind::Binary { op, lhs, rhs } => ExprKind::Binary {
                op: *op,
                lhs: Box::new(self.expr(lhs)),
                rhs: Box::new(self.expr(rhs)),
            },
            ExprKind::Unary { op, expr } => ExprKind::Unary {
                op: *op,
                expr: Box::new(self.expr(expr)),
            },
            ExprKind::Call { callee, args } => ExprKind::Call {
                callee: Box::new(self.expr(callee)),
                args: args.iter().map(|a| self.call_arg(a)).collect(),
            },
            ExprKind::MethodCall {
                receiver,
                name,
                args,
            } => ExprKind::MethodCall {
                receiver: Box::new(self.expr(receiver)),
                name: *name,
                args: args.iter().map(|a| self.call_arg(a)).collect(),
            },
            ExprKind::Field { receiver, name } => ExprKind::Field {
                receiver: Box::new(self.expr(receiver)),
                name: *name,
            },
            ExprKind::TupleIndex { receiver, index } => ExprKind::TupleIndex {
                receiver: Box::new(self.expr(receiver)),
                index: *index,
            },
            // Comptime-indexed field access (D-22). Substitution preserves
            // it verbatim (recursing into receiver + index) so it survives
            // into the generated artifact, where the pass-2 comptime
            // expansion folds it against the now-concrete receiver type.
            ExprKind::CompField { receiver, index } => ExprKind::CompField {
                receiver: Box::new(self.expr(receiver)),
                index: Box::new(self.expr(index)),
            },
            ExprKind::Index { receiver, index } => ExprKind::Index {
                receiver: Box::new(self.expr(receiver)),
                index: Box::new(self.expr(index)),
            },
            ExprKind::If {
                cond,
                then_block,
                else_branch,
            } => ExprKind::If {
                cond: Box::new(self.expr(cond)),
                then_block: self.block(then_block),
                else_branch: else_branch.as_ref().map(|e| Box::new(self.expr(e))),
            },
            ExprKind::Match { scrutinee, arms } => ExprKind::Match {
                scrutinee: Box::new(self.expr(scrutinee)),
                arms: arms.iter().map(|a| self.match_arm(a)).collect(),
            },
            ExprKind::Block(b) => ExprKind::Block(self.block(b)),
            ExprKind::Cast { expr, ty, mode } => ExprKind::Cast {
                expr: Box::new(self.expr(expr)),
                ty: Box::new(self.ty(ty)),
                mode: *mode,
            },
            ExprKind::Range { lo, hi, kind } => ExprKind::Range {
                lo: lo.as_ref().map(|e| Box::new(self.expr(e))),
                hi: hi.as_ref().map(|e| Box::new(self.expr(e))),
                kind: *kind,
            },
            ExprKind::Tuple(elems) => {
                ExprKind::Tuple(elems.iter().map(|e| self.expr(e)).collect())
            }
            ExprKind::Array(elems) => {
                ExprKind::Array(elems.iter().map(|e| self.expr(e)).collect())
            }
            ExprKind::StructLit { path, fields } => ExprKind::StructLit {
                path: self.rewrite_path_as_type(path),
                fields: fields.iter().map(|f| self.struct_lit_field(f)).collect(),
            },
            ExprKind::Loop {
                body,
                label,
                decreases,
            } => ExprKind::Loop {
                body: self.block(body),
                label: *label,
                decreases: decreases.as_ref().map(|m| Box::new(self.expr(m))),
            },
            ExprKind::For {
                pat,
                iter,
                body,
                label,
            } => ExprKind::For {
                pat: Box::new(self.pat(pat)),
                iter: Box::new(self.expr(iter)),
                body: self.block(body),
                label: *label,
            },
            ExprKind::Try(e) => ExprKind::Try(Box::new(self.expr(e))),
            ExprKind::Await(e) => ExprKind::Await(Box::new(self.expr(e))),
            ExprKind::Raise(e) => ExprKind::Raise(Box::new(self.expr(e))),
            ExprKind::Panic(e) => ExprKind::Panic(Box::new(self.expr(e))),
            ExprKind::Comptime(e) => ExprKind::Comptime(Box::new(self.expr(e))),
            ExprKind::ComptimeBlock(b) => ExprKind::ComptimeBlock(self.block(b)),
            ExprKind::Scope { kind, name, body } => ExprKind::Scope {
                kind: *kind,
                name: name.clone(),
                body: self.block(body),
            },
            ExprKind::Return(opt) => {
                ExprKind::Return(opt.as_ref().map(|e| Box::new(self.expr(e))))
            }
            ExprKind::Break { label, value } => ExprKind::Break {
                label: *label,
                value: value.as_ref().map(|e| Box::new(self.expr(e))),
            },
            ExprKind::Continue { label } => ExprKind::Continue { label: *label },
            ExprKind::EffectRow(row) => ExprKind::EffectRow(row.clone()),
            // Closure literals: substitution-walk passes through
            // unchanged. Substitution into closure bodies lands with the
            // rest of the function-type pipeline.
            ExprKind::Closure(c) => ExprKind::Closure(c.clone()),
            // Handle expressions: substitution-walk recurses into subexpressions.
            // `ty` is `None` for the payload-less pure-effect forms
            // (`handle cancellation -> ...`, `handle divergence -> ...`)
            // — nothing to substitute into. The optional binder is passed
            // through unchanged — substitution does not rebind locals.
            ExprKind::Handle {
                effect,
                ty,
                binder,
                recovery,
                body,
            } => ExprKind::Handle {
                effect: *effect,
                ty: ty.as_deref().map(|t| Box::new(self.ty(t))),
                binder: *binder,
                recovery: Box::new(self.expr(recovery)),
                body: self.block(body),
            },
            // Spawn-block: substitution-walk recurses into every arg's
            // initialiser (and optional type annotation) plus the body.
            // Spec-body admissibility for spawn is open — spawn does not
            // appear inside the locked spec-body grammar today — so this
            // arm exists for completeness. Downstream, `HirExprKind::Spawn`
            // now lowers + type-checks for real; the
            // spawn expression's own value type stays
            // the `Error` sentinel pending the `Task(T)` materialisation
            // wave (mirrors the existing `Range` / `Option` precedent).
            ExprKind::Spawn(s) => ExprKind::Spawn(Box::new(SpawnExpr {
                span: s.span,
                scope_name: s.scope_name,
                args: s
                    .args
                    .iter()
                    .map(|a| SpawnArg {
                        span: a.span,
                        name: a.name,
                        ty: a.ty.as_ref().map(|t| self.ty(t)),
                        init: self.expr(&a.init),
                    })
                    .collect(),
                body: self.block(&s.body),
            })),
            ExprKind::Error => ExprKind::Error,
            ExprKind::Forall { bound, iter, body } => ExprKind::Forall {
                bound: *bound,
                iter: Box::new(self.expr(iter)),
                body: Box::new(self.expr(body)),
            },
            ExprKind::Exists { bound, iter, body } => ExprKind::Exists {
                bound: *bound,
                iter: Box::new(self.expr(iter)),
                body: Box::new(self.expr(body)),
            },
        }
    }
}
