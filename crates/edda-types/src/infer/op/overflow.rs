//! Checked-arithmetic `err: Overflow` row attachment.
//!
//! Checked operators (`+? -? *? %?`) and the checked cast (`as T
//! checked`) originate the overflow effect. These helpers resolve the
//! `Overflow` nominal from an open handler or the function's declared
//! row and push the corresponding `err: Overflow` entry onto the row
//! accumulator.

use edda_span::Span;
use edda_syntax::ast::BinOp;

use crate::effect::{EffectEntry, PureEffect};
use crate::ty::{TyId, TyKind};

use super::super::InferCx;
use super::binary::op_keyword;

/// Look up `Overflow` in any active `handle err: Overflow as _ -> ...`
/// or in the function's declared row, then push the corresponding
/// `err: Overflow` entry onto the row accumulator. Innermost handler
/// wins so the pushed `TyId` matches what `synth_handle` will
/// discharge with `row.discharge_since`. Emits a precise diagnostic
/// if neither path carries it.
pub(in crate::infer::op) fn attach_overflow_row(ic: &mut InferCx<'_>, op: BinOp, span: Span) {
    if let Some(overflow_ty) = find_overflow_ty(ic) {
        ic.row
            .push(EffectEntry::Pure(PureEffect::Err(overflow_ty)));
        return;
    }
    ic.emit_typecheck_error(
        span,
        format!(
            "`{}` originates `err: Overflow` but neither the function's declared row nor an open handler carries it; \
             add `import std.overflow` and either `with {{err: Overflow, ...}}` to the function signature \
             or wrap the operator in `handle err: overflow.Overflow as _ -> ...`",
            op_keyword(op)
        ),
    );
}

/// Same shape as [`attach_overflow_row`] but tagged for the
/// checked-cast (`expr as T checked`) site: the diagnostic identifies
/// the operator as the cast form rather than a [`BinOp`] symbol.
pub(in crate::infer) fn attach_overflow_row_for_cast(ic: &mut InferCx<'_>, span: Span) {
    if let Some(overflow_ty) = find_overflow_ty(ic) {
        ic.row
            .push(EffectEntry::Pure(PureEffect::Err(overflow_ty)));
        return;
    }
    ic.emit_typecheck_error(
        span,
        "`as T checked` originates `err: Overflow` but neither the function's declared row nor an open handler carries it; \
         add `import std.overflow` and either `with {err: Overflow, ...}` to the function signature \
         or wrap the cast in `handle err: overflow.Overflow as _ -> ...`"
            .to_string(),
    );
}

/// Scan active `handle err: T as _ -> ...` scopes (innermost first)
/// and then the function's declared row for an `err: T` whose nominal
/// name resolves to `Overflow`. Returns the [`TyId`] of the first
/// match.
fn find_overflow_ty(ic: &InferCx<'_>) -> Option<TyId> {
    let package = ic.lower.package?;
    let matches_overflow = |t: TyId| -> bool {
        let TyKind::Nominal(binding_id) = ic.lower.ty_interner.kind(t) else {
            return false;
        };
        let binding_entry = package.binding(*binding_id);
        ic.lower.interner.resolve(binding_entry.name) == "Overflow"
    };
    for entry in ic.discharged_effects.iter().rev() {
        if let EffectEntry::Pure(PureEffect::Err(t)) = entry
            && matches_overflow(*t)
        {
            return Some(*t);
        }
    }
    for entry in ic.declared_row.entries() {
        if let EffectEntry::Pure(PureEffect::Err(t)) = entry
            && matches_overflow(*t)
        {
            return Some(*t);
        }
    }
    None
}
