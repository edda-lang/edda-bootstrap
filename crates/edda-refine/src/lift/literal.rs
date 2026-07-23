//! Leaf-expression lifts: integer/bool literals and path references.

use edda_span::Span;
use edda_syntax::ast::{Expr, ExprKind, Literal, Path};

use crate::error::LiftError;
use crate::predicate::{IntLit, IntLitValue, Predicate, Variable};
use crate::sort::{IntSort, Sort};

use super::env::PredicateEnv;

pub(super) fn lift_literal(
    lit: &Literal,
    expr: &Expr,
    env: &dyn PredicateEnv,
) -> Result<Predicate, LiftError> {
    match lit {
        Literal::Int { value, .. } => {
            // The typechecker has inferred the literal's target sort; we
            // ask the env for it rather than re-inferring on our side.
            let sort = env.expr_sort(expr).ok_or(LiftError::UnresolvedPath {
                span: expr.span,
            })?;
            let int_sort = match sort {
                Sort::Int(s) => s,
                other => {
                    return Err(LiftError::SortMismatch {
                        span: expr.span,
                        expected: format!("Int (got {other:?})"),
                    });
                }
            };
            let lit_value = int_lit_value_from_u128(*value, int_sort, expr.span)?;
            Ok(Predicate::IntLit(IntLit {
                value: lit_value,
                sort: int_sort,
            }))
        }
        Literal::Bool(b) => Ok(Predicate::BoolLit(*b)),
        Literal::Float(_) => Err(LiftError::Unsupported {
            what: "float literals in predicate position — not yet supported \
                   (`refinement-decidability.md` §5)"
                .to_string(),
            span: expr.span,
        }),
        Literal::Str(_) => Err(LiftError::NotAdmittedInPredicate {
            form: "string literal",
            span: expr.span,
        }),
        Literal::Unit => Err(LiftError::Unsupported {
            what: "unit literal in predicate position".to_string(),
            span: expr.span,
        }),
    }
}

// Convert a u128 literal value into the IR's signed/unsigned representation,
// choosing the variant per the paired IntSort.
pub(super) fn int_lit_value_from_u128(
    value: u128,
    sort: IntSort,
    span: Span,
) -> Result<IntLitValue, LiftError> {
    if sort.signed {
        // The parser stores integers as u128. Negation is applied at the
        // `ast::ExprKind::Unary { op: Neg, .. }` layer, so signed literals
        // here are non-negative. We still allow the full positive i128
        // range by casting through; values that exceed i128::MAX surface
        // as out-of-range.
        let signed = i128::try_from(value).map_err(|_| LiftError::IntLitOutOfRange {
            span,
            value: value.to_string(),
        })?;
        Ok(IntLitValue::Signed(signed))
    } else {
        Ok(IntLitValue::Unsigned(value))
    }
}

//            further module-qualification —
//            the resolver records `Resolved::Binding(head)` keyed by the
//            WHOLE path's span for any Param/Local head in expression
//            position, so a multi-segment value-position Path is always
//            `head.field1.field2...`, mirroring `edda-types`'
//            `lower_path_as_value` decomposition on the typecheck side
//          trailing segments chain through env.lookup_field —
//          the parser folds a bare-identifier `.field` chain into `Path`,
//          not `ExprKind::Field`, so this is the only place that sees it
pub(super) fn lift_path(path: &Path, env: &dyn PredicateEnv) -> Result<Predicate, LiftError> {
    let (name, sort) = env
        .lookup_path(path.span)
        .ok_or(LiftError::UnresolvedPath { span: path.span })?;
    let mut pred = Predicate::Var(Variable::new(name, sort));
    for segment in &path.segments[1..] {
        let base_sort = pred.sort();
        let field_ref = env
            .lookup_field(&base_sort, segment)
            .ok_or_else(|| LiftError::UnknownField {
                span: path.span,
                field: env.ident_name(segment),
            })?;
        pred = Predicate::field_proj(pred, field_ref);
    }
    Ok(pred)
}

// Recognise an integer literal in an expression position, returning the
// fully-typed IntLit. Returns Ok(None) for non-literal expressions so the
// caller can decide whether the non-literal case is admitted.
//
pub(super) fn match_int_lit(
    expr: &Expr,
    env: &dyn PredicateEnv,
) -> Result<Option<IntLit>, LiftError> {
    let value = match &expr.kind {
        ExprKind::Literal(Literal::Int { value, .. }) => *value,
        _ => return Ok(None),
    };
    let sort = env.expr_sort(expr).ok_or(LiftError::UnresolvedPath {
        span: expr.span,
    })?;
    let int_sort = match sort {
        Sort::Int(s) => s,
        other => {
            return Err(LiftError::SortMismatch {
                span: expr.span,
                expected: format!("Int (got {other:?})"),
            });
        }
    };
    let lit_value = int_lit_value_from_u128(value, int_sort, expr.span)?;
    Ok(Some(IntLit {
        value: lit_value,
        sort: int_sort,
    }))
}
