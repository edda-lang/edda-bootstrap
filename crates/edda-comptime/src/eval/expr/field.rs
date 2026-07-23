//! Record field projection (`receiver.field`) evaluation for the
//! comptime HIR evaluator.
//!
//! Companion to [`super::structlit`] (construction): once
//! a `comptime` body reduces a struct literal to a [`Value::Record`],
//! a subsequent field read (`theme.bg`) projects the named entry back
//! out. The receiver is comptime-evaluated first; the result must be a
//! [`Value::Record`], whose entries are `(Symbol, Value)` pairs in
//! declared field order — the named entry's value is returned by clone
//! (the evaluator works over owned [`Value`]s).
//!
//! Both failure arms below (non-record receiver, absent field) are
//! typechecker-precluded — the typechecker rejects a field access on a
//! non-product type and an unknown field name before the HIR reaches
//! comptime — so they exist only as defensive `push_panic` diagnostics
//! rather than a "not yet supported" fall-through.

use edda_span::Span;
use edda_syntax::ast::Ident;
use edda_types::HirExpr;

use crate::eval::expr::diag::push_panic;
use crate::eval::expr::{EvalCx, eval_expr};
use crate::value::Value;

/// Evaluate `receiver.name` by projecting the named entry out of the
/// [`Value::Record`] the receiver reduces to.
pub(super) fn eval_field(
    receiver: &HirExpr,
    name: &Ident,
    span: Span,
    cx: &mut EvalCx<'_>,
) -> Option<Value> {
    let value = eval_expr(receiver, cx)?;
    match value {
        Value::Record(entries) => {
            match entries.into_iter().find(|(field, _)| *field == name.name) {
                Some((_, field_value)) => Some(field_value),
                None => {
                    push_panic(
                        cx.diags,
                        span,
                        format!(
                            "comptime field access `{}` names no field of the record value",
                            cx.interner.resolve(name.name)
                        ),
                    );
                    None
                }
            }
        }
        other => {
            push_panic(
                cx.diags,
                span,
                format!(
                    "comptime field access `.{}` on a non-record {} value",
                    cx.interner.resolve(name.name),
                    other.kind().name()
                ),
            );
            None
        }
    }
}
