//! AST refinement-expression → [`Predicate`] lifter.
//!
//! Lifts the `pred: ast::Expr` payload of every
//! [`edda_syntax::ast::RefinementClause`] into refine's typed [`Predicate`]
//! IR. The lifter walks the AST top-down and maps every admitted
//! predicate-fragment form (per `docs/syntax/refinements.md` *The
//! predicate fragment*) onto the corresponding [`Predicate`] constructor.
//! Non-admitted forms surface as [`LiftError`] so the typechecker can
//! attribute a precise diagnostic.
//!
//! # Integration seam — annotated for the typechecker wiring
//!
//! Every type-system query (binding sort, field schema, cast target sort)
//! routes through the [`PredicateEnv`] trait. Today refine has no consumer
//! of this trait — tests provide synthetic impls and the [`Z3Backend`]
//! discharge path takes pre-built [`Predicate`] values. The eventual
//! integration is in `edda-types`: the typechecker builds a
//! `TyCx`-backed `PredicateEnv` impl and feeds the AST refinement
//! clauses through this module.
//!
//! Most lifting arms touch `PredicateEnv`; routing every type-system
//! lookup through that single trait keeps the integration points easy
//! to enumerate.
//!
//! # Module layout
//!
//! - [`env`] — [`PredicateEnv`] trait definition.
//! - [`literal`] — leaf-expression lifts (`Int` / `Bool` literals, path
//!   variables, the `match_int_lit` helper used by `*` / `/`).
//! - [`operator`] — binary (`+`, `-`, `*`, `/`, `==`, `<`, `&&`, ...) and
//!   unary (`-`, `!`) lifts.
//! - [`structural`] — `if`/`else`, field projection, slice indexing,
//!   method calls (`len()`), `as`-casts, and trailing-expression blocks.
//!
//! # Scope cuts
//!
//! - **Literals** — only `Int` and `Bool` are lifted today. `Float`, `Str`,
//!   `FString`, `Unit` route to `LiftError::NotAdmittedInPredicate`.
//! - **`%` modulo** — admitted syntactically per `refinements.md` but not in
//!   the required-decidable fragment (`refinement-decidability.md §4`).
//!   Rejected with a clear pointer at restating via `/`.
//! - **`*` / `/` by non-literal** — rejected per the LIA literal-constant rule.
//! - **Tuples** — `ExprKind::Tuple` rejected as `Unsupported` (deferral
//!   on `Sort::Tuple` carries forward).
//! - **`Block`** — only an empty-statement block with a trailing expression
//!   is admitted; the lifter recurses into the trailing expr.
//! - **`@unverified` / `@trust`** — gated on `edda-syntax` adding annotation
//!   parsing. The lifter doesn't inspect annotations today; the discharge
//!   layer continues to receive routing decisions out-of-band via
//!   [`Obligation::with_route`].
//!
//! [`Z3Backend`]: crate::Z3Backend
//! [`Obligation::with_route`]: crate::Obligation::with_route

mod env;
mod literal;
mod operator;
mod quantifier;
mod structural;

#[cfg(test)]
mod tests;

pub use env::PredicateEnv;

use edda_syntax::ast::{self, CallArg, Expr, ExprKind, RefinementClause};
use edda_span::Span;

use crate::error::LiftError;
use crate::predicate::Predicate;

//            so downstream diagnostics can highlight precise sub-tree positions
//          and delegates type / binding lookups to PredicateEnv
/// Lift an AST expression into a [`Predicate`].
///
/// The entry point for refinement-clause translation. Dispatches by
/// [`ExprKind`]; admitted forms walk recursively, non-admitted forms
/// surface as [`LiftError`]. Type and binding lookups defer to `env` —
/// see the module doc's "Integration seam" section.
pub fn lift_predicate(expr: &Expr, env: &dyn PredicateEnv) -> Result<Predicate, LiftError> {
    match &expr.kind {
        ExprKind::Literal(lit) => literal::lift_literal(lit, expr, env),
        ExprKind::Path(path) => literal::lift_path(path, env),
        ExprKind::Binary { op, lhs, rhs } => operator::lift_binary(*op, lhs, rhs, expr.span, env),
        ExprKind::Unary { op, expr: operand } => {
            operator::lift_unary(*op, operand, expr.span, env)
        }
        ExprKind::If {
            cond,
            then_block,
            else_branch,
        } => structural::lift_if(cond, then_block, else_branch.as_deref(), expr.span, env),
        ExprKind::Field { receiver, name } => {
            structural::lift_field(receiver, name, expr.span, env)
        }
        ExprKind::Index { receiver, index } => structural::lift_index(receiver, index, env),
        ExprKind::MethodCall {
            receiver,
            name,
            args,
        } => structural::lift_method_call(receiver, name, args, expr.span, env),
        // The parser collapses `base.method(args)` into
        // `Call { callee: Path([base, method]), args }` when `base` is
        // an identifier — multi-segment paths absorb the dot.
        // Recognize the Path([base, name]) shape and forward to the
        // MethodCall lifter so `paths.len()` discharges as `slice_len(paths)`.
        ExprKind::Call { callee, args } => lift_method_call_shape(callee, args, expr.span, env),
        ExprKind::Cast { expr: inner, ty, mode: _ } => {
            structural::lift_cast(inner, ty, expr.span, env)
        }
        ExprKind::Block(block) => structural::lift_block(block, expr.span, env),
        ExprKind::Forall { bound, iter, body } => {
            quantifier::lift_forall(bound, iter, body, expr.span, env)
        }
        ExprKind::Exists { bound, iter, body } => {
            quantifier::lift_exists(bound, iter, body, expr.span, env)
        }
        _ => Err(reject_not_admitted(expr)),
    }
}

/// Recognise a `Call` whose callee is a multi-segment
/// `Path([base, .., name])` — the AST shape the parser emits for
/// `base.name(args)` (and `base.field.name(args)`) — and forward to
/// [`structural::lift_method_call`]. Falls back to the catch-all
/// "user-function call" rejection for any other Call shape.
///
/// The receiver synthesised for [`structural::lift_method_call`] is the
/// callee path *minus its trailing method segment*, keyed by the
/// original whole-path span — the resolver registers
/// `Resolved::Binding(head)` against the whole path's span, so that span
/// is the only key that resolves; a sub-span synthesised from `base`'s
/// own ident would not. Passing the method segment through to
/// `lift_path` instead (the previous behaviour) made the lifter
/// re-interpret `len` as a *field* of the receiver's sort, so every
/// parser-folded `xs.len()` — including one inside a caller's own
/// `requires` clause — failed to lift and was silently dropped from the
/// obligation context.
fn lift_method_call_shape(
    callee: &Expr,
    args: &[CallArg],
    site: Span,
    env: &dyn PredicateEnv,
) -> Result<Predicate, LiftError> {
    let ExprKind::Path(path) = &callee.kind else {
        return Err(LiftError::NotAdmittedInPredicate {
            form: "user-function call",
            span: site,
        });
    };
    if path.segments.len() < 2 {
        return Err(LiftError::NotAdmittedInPredicate {
            form: "user-function call",
            span: site,
        });
    }
    let method_ident = &path.segments[path.segments.len() - 1];
    let receiver = Expr {
        span: callee.span,
        kind: ExprKind::Path(ast::Path {
            span: path.span,
            segments: path.segments[..path.segments.len() - 1].to_vec(),
        }),
    };
    structural::lift_method_call(&receiver, method_ident, args, site, env)
}

/// Lift a [`RefinementClause`] (a `where` / `requires` / `ensures` clause)
/// into a [`Predicate`]. Thin wrapper around [`lift_predicate`] kept as a
/// distinct entry point so the typechecker integration can hang annotation
/// inspection off this slot without changing the bare-expression API.
pub fn lift_clause(
    clause: &RefinementClause,
    env: &dyn PredicateEnv,
) -> Result<Predicate, LiftError> {
    lift_predicate(&clause.pred, env)
}

// Map every non-admitted [`ExprKind`] to its [`LiftError`] variant. Pulled
// out of the master dispatch because the catch-all arms outnumber the
// admitted arms ~2:1, and bundling them keeps the dispatcher's intent
// (admitted-form routing) visible.
//
//            must mirror new ExprKind variants added to edda-syntax
fn reject_not_admitted(expr: &Expr) -> LiftError {
    let span = expr.span;
    match &expr.kind {
        ExprKind::FString(_) => LiftError::NotAdmittedInPredicate {
            form: "interpolated string",
            span,
        },
        ExprKind::Tuple(_) => LiftError::Unsupported {
            what: "tuple-construction in predicate position".to_string(),
            span,
        },
        ExprKind::Array(_) => LiftError::NotAdmittedInPredicate {
            form: "array literal",
            span,
        },
        ExprKind::Closure(_) => LiftError::NotAdmittedInPredicate {
            form: "closure literal",
            span,
        },
        ExprKind::Spawn(_) => LiftError::NotAdmittedInPredicate {
            form: "spawn-block",
            span,
        },
        ExprKind::Call { .. } => LiftError::NotAdmittedInPredicate {
            form: "user-function call",
            span,
        },
        ExprKind::Match { .. } => LiftError::NotAdmittedInPredicate {
            form: "match",
            span,
        },
        ExprKind::Range { .. } => LiftError::NotAdmittedInPredicate {
            form: "range",
            span,
        },
        ExprKind::StructLit { .. } => LiftError::NotAdmittedInPredicate {
            form: "struct literal",
            span,
        },
        ExprKind::Loop { .. } => LiftError::NotAdmittedInPredicate {
            form: "loop",
            span,
        },
        ExprKind::For { .. } => LiftError::NotAdmittedInPredicate {
            form: "for-loop",
            span,
        },
        ExprKind::Try(_) => LiftError::NotAdmittedInPredicate {
            form: "`?` propagation",
            span,
        },
        ExprKind::Await(_) => LiftError::NotAdmittedInPredicate {
            form: ".await",
            span,
        },
        ExprKind::Raise(_) => LiftError::NotAdmittedInPredicate {
            form: "raise",
            span,
        },
        ExprKind::Panic(_) => LiftError::NotAdmittedInPredicate {
            form: "panic",
            span,
        },
        ExprKind::Comptime(_) | ExprKind::ComptimeBlock(_) => {
            LiftError::NotAdmittedInPredicate {
                form: "comptime",
                span,
            }
        }
        ExprKind::Scope { .. } => LiftError::NotAdmittedInPredicate {
            form: "scope",
            span,
        },
        ExprKind::Return(_) | ExprKind::Break { .. } | ExprKind::Continue { .. } => {
            LiftError::NotAdmittedInPredicate {
                form: "control-flow exit",
                span,
            }
        }
        ExprKind::Error => LiftError::NotAdmittedInPredicate {
            form: "parser-error sentinel",
            span,
        },
        ExprKind::EffectRow(_) => LiftError::NotAdmittedInPredicate {
            form: "effect-row literal",
            span,
        },
        ExprKind::Handle { .. } => LiftError::NotAdmittedInPredicate {
            form: "handle expression",
            span,
        },
        // The admitted arms are dispatched in lift_predicate; this helper
        // is only reached for non-admitted forms.
        ExprKind::TupleIndex { .. } => LiftError::Unsupported {
            what: "tuple-index access in predicate position".to_string(),
            span,
        },
        ExprKind::CompField { .. } => LiftError::NotAdmittedInPredicate {
            form: "comptime-indexed field access",
            span,
        },
        ExprKind::Literal(_)
        | ExprKind::Path(_)
        | ExprKind::Binary { .. }
        | ExprKind::Unary { .. }
        | ExprKind::If { .. }
        | ExprKind::Field { .. }
        | ExprKind::Index { .. }
        | ExprKind::MethodCall { .. }
        | ExprKind::Cast { .. }
        | ExprKind::Block(_)
        | ExprKind::Forall { .. }
        | ExprKind::Exists { .. } => unreachable!(
            "reject_not_admitted called on an admitted ExprKind — dispatch \
             bug in lift_predicate"
        ),
    }
}
