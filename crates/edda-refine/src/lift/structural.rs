//! Structural lifts: `if`/`else`, field projection, slice index, method calls,
//! casts, and trailing-expression blocks.

use edda_span::Span;
use edda_syntax::ast::{self, CallArg, Expr, Ident};

use crate::error::LiftError;
use crate::predicate::Predicate;
use crate::sort::Sort;

use super::env::PredicateEnv;
use super::lift_predicate;

pub(super) fn lift_if(
    cond: &Expr,
    then_block: &ast::Block,
    else_branch: Option<&Expr>,
    span: Span,
    env: &dyn PredicateEnv,
) -> Result<Predicate, LiftError> {
    let cond_pred = lift_predicate(cond, env)?;
    let then_pred = lift_predicate(&block_as_expr(then_block)?, env)?;
    let else_expr = else_branch.ok_or(LiftError::NotAdmittedInPredicate {
        form: "if-without-else",
        span,
    })?;
    let else_pred = lift_predicate(else_expr, env)?;
    Ok(Predicate::if_then_else(cond_pred, then_pred, else_pred))
}

// A block in predicate position is only admissible if it has no statements
// and contains a trailing expression — `if cond { e1 } else { e2 }` parses
// `e1` / `e2` as `Block`s with `trailing = Some(_)` and `stmts.len() == 0`.
pub(super) fn block_as_expr(block: &ast::Block) -> Result<Expr, LiftError> {
    if !block.stmts.is_empty() {
        return Err(LiftError::NonTrivialBlock { span: block.span });
    }
    match &block.trailing {
        Some(trailing) => Ok((**trailing).clone()),
        None => Err(LiftError::NotAdmittedInPredicate {
            form: "empty block",
            span: block.span,
        }),
    }
}

pub(super) fn lift_field(
    receiver: &Expr,
    name: &Ident,
    span: Span,
    env: &dyn PredicateEnv,
) -> Result<Predicate, LiftError> {
    let receiver_pred = lift_predicate(receiver, env)?;
    let base_sort = receiver_pred.sort();
    let field_ref = env
        .lookup_field(&base_sort, name)
        .ok_or_else(|| LiftError::UnknownField {
            span,
            field: env.ident_name(name),
        })?;
    Ok(Predicate::field_proj(receiver_pred, field_ref))
}

pub(super) fn lift_index(
    receiver: &Expr,
    index: &Expr,
    env: &dyn PredicateEnv,
) -> Result<Predicate, LiftError> {
    let slice = lift_predicate(receiver, env)?;
    let idx = lift_predicate(index, env)?;
    Ok(Predicate::slice_index(slice, idx))
}

//          recognise the well-known `len()` slice method
pub(super) fn lift_method_call(
    receiver: &Expr,
    name: &Ident,
    args: &[CallArg],
    span: Span,
    env: &dyn PredicateEnv,
) -> Result<Predicate, LiftError> {
    let name_str = env.ident_name(name);
    if name_str.as_str() == "len" && args.is_empty() {
        let slice = lift_predicate(receiver, env)?;
        // Only slice-sorted receivers have a built-in `len()`; a record /
        // integer receiver reaching here (e.g. a user method named `len`)
        // stays outside the predicate fragment rather than producing an
        // ill-sorted SliceLen the Z3 translator would choke on.
        if !matches!(slice.sort(), Sort::Slice(_)) {
            return Err(LiftError::NotAdmittedInPredicate {
                form: "`len()` on a non-slice receiver",
                span,
            });
        }
        return Ok(Predicate::slice_len(slice));
    }
    // Mode keywords inside refinement-position calls are not admitted —
    // predicates must be side-effect-free.
    if args.iter().any(|a| a.mode.is_some()) {
        return Err(LiftError::NotAdmittedInPredicate {
            form: "call-site mode keyword in predicate position",
            span,
        });
    }
    Err(LiftError::NotAdmittedInPredicate {
        form: "user-method call (only the built-in `len()` on slices is admitted)",
        span,
    })
}

pub(super) fn lift_cast(
    expr: &Expr,
    ty: &ast::Type,
    span: Span,
    env: &dyn PredicateEnv,
) -> Result<Predicate, LiftError> {
    let target = env
        .type_sort(ty)
        .ok_or(LiftError::UnsupportedCastTarget { span })?;
    let int_sort = match target {
        Sort::Int(s) => s,
        _ => return Err(LiftError::UnsupportedCastTarget { span }),
    };
    let value = lift_predicate(expr, env)?;
    Ok(Predicate::cast(value, int_sort))
}

pub(super) fn lift_block(
    block: &ast::Block,
    _span: Span,
    env: &dyn PredicateEnv,
) -> Result<Predicate, LiftError> {
    let inner = block_as_expr(block)?;
    lift_predicate(&inner, env)
}
