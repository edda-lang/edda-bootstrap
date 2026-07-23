//! Struct-literal (`Path { field: e, ... }`) evaluation for the
//! comptime HIR evaluator.
//!
//! A comptime block whose body (transitively) constructs a record —
//! e.g. a helper `function palette() -> Theme { return Theme { ... } }`
//! called from a `comptime { ... }` block — reduces the literal to a
//! [`Value::Record`]: each field initialiser is itself comptime-
//! evaluated, in struct-literal source order, and paired with its
//! declared field name.
//!
//! Call-site mode keywords ([`edda_types::HirCallMode`]) on a field
//! value (`take` / `mutable` / `init`) carry ownership semantics that
//! are inert at comptime — the evaluator works over owned [`Value`]
//! clones — so they are ignored here, exactly as call-argument modes
//! are ignored by [`super::eval::eval_call`].

use edda_span::Span;
use edda_types::HirStructLitField;

use crate::eval::expr::{EvalCx, eval_expr};
use crate::value::Value;

/// Evaluate a `Path { field: e, ... }` struct literal to a
/// [`Value::Record`].
pub(super) fn eval_struct_lit(
    fields: &[HirStructLitField],
    _span: Span,
    cx: &mut EvalCx<'_>,
) -> Option<Value> {
    let mut entries = Vec::with_capacity(fields.len());
    for field in fields {
        let value = eval_expr(&field.value, cx)?;
        entries.push((field.name.name, value));
    }
    Some(Value::Record(entries))
}
