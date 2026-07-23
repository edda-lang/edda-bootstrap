//! Structural recursion over an expression's kind for the [`Expander`].
//!
//! Holds [`Expander::expr_kind`] — the per-variant cloning walker that
//! transforms every child expression / block under the current [`Env`] —
//! plus the small per-collection helpers (`opt_bx`, `exprs`, `call_args`,
//! `match_arms`, `struct_fields`) it dispatches into. The two special
//! cases (loop-variable path, `CompField`) are handled by
//! [`Expander::expr`] in [`super::expander`] before this is reached.

use edda_diag::Diagnostics;
use edda_syntax::ast::{Expr, ExprKind, FStringPart};

use super::expander::{Env, Expander};

impl<'a> Expander<'a> {
    /// Structural recursion over an expression's kind, transforming every
    /// child expression / block under `env`. Mirrors the cloning walker
    /// in `edda-codegen`'s substitution pass; the two special cases
    /// (loop-var path, `CompField`) are handled by [`Expander::expr`]
    /// before this is reached.
    pub(super) fn expr_kind(&self, k: &ExprKind, env: &Env<'_>, diags: &mut Diagnostics) -> ExprKind {
        // Direct `self.expr(...)` calls (no closures) so each child borrows
        // `diags` sequentially — closures capturing `&mut diags` would
        // conflict (E0524). The two special cases (loop-var path,
        // `CompField`) are handled in [`Expander::expr`] before this is reached.
        macro_rules! bx {
            ($e:expr) => {
                Box::new(self.expr($e, env, diags))
            };
        }
        match k {
            ExprKind::Literal(l) => ExprKind::Literal(*l),
            ExprKind::FString(parts) => ExprKind::FString(
                parts
                    .iter()
                    .map(|part| match part {
                        FStringPart::Text(s) => FStringPart::Text(*s),
                        FStringPart::Slot(e) => {
                            FStringPart::Slot(Box::new(self.expr(e, env, diags)))
                        }
                    })
                    .collect(),
            ),
            ExprKind::Path(p) => ExprKind::Path(p.clone()),
            ExprKind::Binary { op, lhs, rhs } => {
                let lhs = bx!(lhs);
                let rhs = bx!(rhs);
                ExprKind::Binary { op: *op, lhs, rhs }
            }
            ExprKind::Unary { op, expr } => ExprKind::Unary { op: *op, expr: bx!(expr) },
            ExprKind::Call { callee, args } => {
                let callee = bx!(callee);
                let args = self.call_args(args, env, diags);
                ExprKind::Call { callee, args }
            }
            ExprKind::MethodCall { receiver, name, args } => {
                let receiver = bx!(receiver);
                let args = self.call_args(args, env, diags);
                ExprKind::MethodCall { receiver, name: *name, args }
            }
            ExprKind::Field { receiver, name } => {
                ExprKind::Field { receiver: bx!(receiver), name: *name }
            }
            ExprKind::TupleIndex { receiver, index } => {
                ExprKind::TupleIndex { receiver: bx!(receiver), index: *index }
            }
            ExprKind::CompField { receiver, index } => {
                let receiver = bx!(receiver);
                let index = bx!(index);
                ExprKind::CompField { receiver, index }
            }
            ExprKind::Index { receiver, index } => {
                let receiver = bx!(receiver);
                let index = bx!(index);
                ExprKind::Index { receiver, index }
            }
            ExprKind::If { cond, then_block, else_branch } => {
                let cond = bx!(cond);
                let then_block = self.block(then_block, env, diags);
                let else_branch = self.opt_bx(else_branch, env, diags);
                ExprKind::If { cond, then_block, else_branch }
            }
            ExprKind::Match { scrutinee, arms } => {
                let scrutinee = bx!(scrutinee);
                let arms = self.match_arms(arms, env, diags);
                ExprKind::Match { scrutinee, arms }
            }
            ExprKind::Block(b) => ExprKind::Block(self.block(b, env, diags)),
            ExprKind::Cast { expr, ty, mode } => {
                ExprKind::Cast { expr: bx!(expr), ty: ty.clone(), mode: *mode }
            }
            ExprKind::Range { lo, hi, kind } => {
                let lo = self.opt_bx(lo, env, diags);
                let hi = self.opt_bx(hi, env, diags);
                ExprKind::Range { lo, hi, kind: *kind }
            }
            ExprKind::Tuple(es) => ExprKind::Tuple(self.exprs(es, env, diags)),
            ExprKind::Array(es) => ExprKind::Array(self.exprs(es, env, diags)),
            ExprKind::StructLit { path, fields } => {
                let fields = self.struct_fields(fields, env, diags);
                ExprKind::StructLit { path: path.clone(), fields }
            }
            ExprKind::Loop { body, label, decreases } => {
                let body = self.block(body, env, diags);
                let decreases = self.opt_bx(decreases, env, diags);
                ExprKind::Loop { body, label: *label, decreases }
            }
            ExprKind::For { pat, iter, body, label } => {
                let iter = bx!(iter);
                let body = self.block(body, env, diags);
                ExprKind::For { pat: pat.clone(), iter, body, label: *label }
            }
            ExprKind::Try(e) => ExprKind::Try(bx!(e)),
            ExprKind::Await(e) => ExprKind::Await(bx!(e)),
            ExprKind::Raise(e) => ExprKind::Raise(bx!(e)),
            ExprKind::Panic(e) => ExprKind::Panic(bx!(e)),
            ExprKind::Comptime(e) => ExprKind::Comptime(bx!(e)),
            ExprKind::ComptimeBlock(b) => ExprKind::ComptimeBlock(self.block(b, env, diags)),
            ExprKind::Scope { kind, name, body } => {
                let body = self.block(body, env, diags);
                ExprKind::Scope { kind: *kind, name: name.clone(), body }
            }
            ExprKind::Return(opt) => ExprKind::Return(self.opt_bx(opt, env, diags)),
            ExprKind::Break { label, value } => {
                ExprKind::Break { label: *label, value: self.opt_bx(value, env, diags) }
            }
            ExprKind::Continue { label } => ExprKind::Continue { label: *label },
            ExprKind::EffectRow(r) => ExprKind::EffectRow(r.clone()),
            ExprKind::Closure(c) => ExprKind::Closure(c.clone()),
            ExprKind::Handle { effect, ty, binder, recovery, body } => {
                let recovery = bx!(recovery);
                let body = self.block(body, env, diags);
                ExprKind::Handle {
                    effect: *effect,
                    ty: ty.clone(),
                    binder: *binder,
                    recovery,
                    body,
                }
            }
            ExprKind::Spawn(s) => ExprKind::Spawn(s.clone()),
            ExprKind::Error => ExprKind::Error,
            ExprKind::Forall { bound, iter, body } => {
                let iter = bx!(iter);
                let body = bx!(body);
                ExprKind::Forall { bound: *bound, iter, body }
            }
            ExprKind::Exists { bound, iter, body } => {
                let iter = bx!(iter);
                let body = bx!(body);
                ExprKind::Exists { bound: *bound, iter, body }
            }
        }
    }

    fn opt_bx(
        &self,
        o: &Option<Box<Expr>>,
        env: &Env<'_>,
        diags: &mut Diagnostics,
    ) -> Option<Box<Expr>> {
        match o {
            Some(e) => Some(Box::new(self.expr(e, env, diags))),
            None => None,
        }
    }

    fn exprs(&self, es: &[Expr], env: &Env<'_>, diags: &mut Diagnostics) -> Vec<Expr> {
        let mut out = Vec::with_capacity(es.len());
        for e in es {
            out.push(self.expr(e, env, diags));
        }
        out
    }

    fn call_args(
        &self,
        args: &[edda_syntax::ast::CallArg],
        env: &Env<'_>,
        diags: &mut Diagnostics,
    ) -> Vec<edda_syntax::ast::CallArg> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            out.push(edda_syntax::ast::CallArg {
                span: a.span,
                mode: a.mode,
                name: a.name.clone(),
                expr: self.expr(&a.expr, env, diags),
            });
        }
        out
    }

    fn match_arms(
        &self,
        arms: &[edda_syntax::ast::MatchArm],
        env: &Env<'_>,
        diags: &mut Diagnostics,
    ) -> Vec<edda_syntax::ast::MatchArm> {
        let mut out = Vec::with_capacity(arms.len());
        for a in arms {
            let guard = match &a.guard {
                Some(g) => Some(self.expr(g, env, diags)),
                None => None,
            };
            let body = self.expr(&a.body, env, diags);
            out.push(edda_syntax::ast::MatchArm {
                span: a.span,
                pat: a.pat.clone(),
                guard,
                body,
            });
        }
        out
    }

    fn struct_fields(
        &self,
        fields: &[edda_syntax::ast::StructLitField],
        env: &Env<'_>,
        diags: &mut Diagnostics,
    ) -> Vec<edda_syntax::ast::StructLitField> {
        let mut out = Vec::with_capacity(fields.len());
        for f in fields {
            out.push(edda_syntax::ast::StructLitField {
                span: f.span,
                name: f.name,
                mode: f.mode,
                value: self.expr(&f.value, env, diags),
            });
        }
        out
    }
}
