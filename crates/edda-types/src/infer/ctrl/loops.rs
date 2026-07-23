//! Loop inference — `loop [decreases <expr>] { ... }`.
//!
//! `synth_loop` synthesises `never`, enforces the §4 body re-entry
//! check, and injects `Pure(Divergence)` for a non-`decreases` loop.
//! The B17 `probe_integer_expected` helper narrows literal siblings in
//! a `decreases` measure.

use crate::effect::{EffectEntry, PureEffect};
use crate::hir::HirBlock;
use crate::prim::Primitive;
use crate::ty::TyId;

use super::super::{InferCx, TyEnv, check_expr, synth_block, synth_expr};

/// Synthesise `loop [decreases <expr>] { ... }`. Always yields
/// `never`; the break-with-value machinery lands later.
///
/// Per `inference-rules.md §4` *`loop` and `for` body re-entry
/// checks*: the body must leave each in-scope binding in a state that
/// equals its pre-body state — otherwise the second iteration would
/// observe a different state than the first. A consumed-inside-loop
/// outer binding is therefore a compile error.
///
/// Divergence injection per
/// `corpus/edda-codex/language/03-verification.md` §5: a `loop` without
/// a `decreases` measure pushes [`Pure(Divergence)`] into the active
/// accumulator. The function-exit row-containment check then surfaces
/// the mismatch when the enclosing function has not admitted
/// `divergence` in its declared row. The measure expression itself
/// (when present) is synthesised for its side-effect — every
/// sub-expression's `ty` field is populated so refinement lifting can
/// build the termination obligation in C5.
pub(crate) fn synth_loop(
    env: &mut TyEnv,
    body: &mut HirBlock,
    decreases: Option<&mut crate::hir::HirExpr>,
    ic: &mut InferCx<'_>,
) -> TyId {
    if let Some(measure) = decreases {
        // B17: a numeric-literal operand in `decreases <expr>` defaults
        // to `i64` under bare synth, which then conflicts with a typed
        // sibling such as `usize band`. Probe for any non-literal
        // integer-typed leaf in the measure and, when one is found,
        // route through `check_expr` so `check_binary` narrows the
        // literal to the sibling's primitive width. Falling back to
        // `synth_expr` preserves the both-literal-default case
        // (`decreases 10 - 5` keeps the `i64` default).
        let expected = probe_integer_expected(measure, env, ic);
        match expected {
            Some(ty) => check_expr(env, measure, ty, ic),
            None => {
                let _ = synth_expr(env, measure, ic);
            }
        }
    } else {
        ic.push_effect_entry(EffectEntry::Pure(PureEffect::Divergence));
    }
    let pre = env.clone();
    let _ = synth_block(env, body, ic);
    super::super::mode::check_loop_reentry(env, &pre, ic, body.span);
    // Re-establish the pre-body state so callers downstream of the
    // loop see the same state they would have without the loop's
    // (possibly diagnosed) body transitions.
    *env = pre;
    ic.lower.ty_interner.prim(Primitive::Never)
}

/// B17 helper — find a non-literal integer-typed leaf in a `decreases`
/// expression so the surrounding `check_expr` narrows literal siblings
/// to the leaf's primitive width.
///
/// Walks the binary/unary/cast skeleton of `expr` (bounded by structure;
/// no recursion through calls / control flow) and returns the first
/// integer-typed `TyId` found, by:
///
/// - single-segment `Path` → lookup `env`;
/// - `Field` → walk through the receiver chain to the head binding,
///   then look up the field's declared type via [`TyCx`];
/// - `Cast { target_ty, .. }` → return `target_ty` if it's integer
///   (a `as <integer>` cast pins the result type);
/// - `Binary` / `Unary` → recurse on the operands.
///
/// Returns `None` for literals (the case we want to *narrow*), control
/// flow, calls, and any other form the simple probe doesn't cover.
fn probe_integer_expected(
    expr: &crate::hir::HirExpr,
    env: &TyEnv,
    ic: &InferCx<'_>,
) -> Option<TyId> {
    use crate::hir::HirExprKind;
    use crate::ty::TyKind;

    fn is_integer(ic: &InferCx<'_>, ty: TyId) -> bool {
        matches!(ic.lower.ty_interner.kind(ty), TyKind::Primitive(p) if p.is_integer())
    }

    match &expr.kind {
        HirExprKind::Literal(_) => None,
        HirExprKind::Path(p) if p.segments.len() == 1 => {
            let ty = env.lookup(p.segments[0].name)?;
            if is_integer(ic, ty) { Some(ty) } else { None }
        }
        HirExprKind::Field { receiver, name } => {
            let head = path_head_binding(receiver)?;
            let head_ty = env.lookup(head)?;
            let TyKind::Nominal(binding_id) = ic.lower.ty_interner.kind(head_ty) else {
                return None;
            };
            let info = ic.ty_cx.type_decl(*binding_id)?;
            let crate::cx::TypeDeclShape::Product { fields } = &info.kind else {
                return None;
            };
            let ty = fields.iter().find(|f| f.name == name.name).map(|f| f.ty)?;
            if is_integer(ic, ty) { Some(ty) } else { None }
        }
        HirExprKind::TupleIndex { receiver, index } => {
            let head = path_head_binding(receiver)?;
            let head_ty = env.lookup(head)?;
            let TyKind::Tuple(elements) = ic.lower.ty_interner.kind(head_ty) else {
                return None;
            };
            let ty = elements.get(*index as usize).copied()?;
            if is_integer(ic, ty) { Some(ty) } else { None }
        }
        HirExprKind::Cast { target_ty, .. } => {
            if is_integer(ic, *target_ty) {
                Some(*target_ty)
            } else {
                None
            }
        }
        HirExprKind::Binary { lhs, rhs, .. } => {
            probe_integer_expected(lhs, env, ic)
                .or_else(|| probe_integer_expected(rhs, env, ic))
        }
        HirExprKind::Unary { expr: operand, .. } => probe_integer_expected(operand, env, ic),
        _ => None,
    }
}

/// Walk a chain of `Field { receiver, .. }` nodes to its head, returning
/// the head's binding [`Symbol`] when the head is a single-segment Path.
/// Returns `None` for non-Path heads (literals, calls, etc.).
fn path_head_binding(expr: &crate::hir::HirExpr) -> Option<edda_intern::Symbol> {
    use crate::hir::HirExprKind;
    let mut current = expr;
    loop {
        match &current.kind {
            HirExprKind::Path(p) if p.segments.len() == 1 => return Some(p.segments[0].name),
            HirExprKind::Field { receiver, .. } => current = receiver,
            HirExprKind::TupleIndex { receiver, .. } => current = receiver,
            _ => return None,
        }
    }
}
