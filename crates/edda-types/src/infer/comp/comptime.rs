//! Comptime-body inference.
//!
//! Walks a `comptime <expr>` / `comptime { … }` body with the
//! P-CompTimePure call gate armed so every nested call verifies the
//! callee is comptime-pure (`row(callee) ⊆ {panic, yield: T}`).

use crate::hir::{HirBlock, HirExpr};
use crate::ty::TyId;

use super::super::{InferCx, TyEnv, synth_block, synth_expr};

/// Synthesise `comptime <expr>` per `inference-rules.md §1a.6`. The
/// inner expression is walked with `ic.in_comptime_context = true`
/// so every nested [`crate::infer::call::synth_call`] verifies the callee
/// is comptime-pure (`row(callee) ⊆ {panic, yield: T}`). Non-call
/// expressions are admissible inside comptime bodies; only the call
/// gate fires the purity check.
///
/// The comptime envelope's own effects (`panic`, `yield: T`) are
/// discharged from the row accumulator on exit: a `comptime panic(…)`
/// fires during compilation (the evaluator turns it into a compile
/// error), so it must not impose a runtime `panic` entry on the
/// enclosing function's declared row.
pub(in crate::infer) fn synth_comptime(
    env: &mut TyEnv,
    inner: &mut HirExpr,
    ic: &mut InferCx<'_>,
) -> TyId {
    let prev = std::mem::replace(&mut ic.in_comptime_context, true);
    let cp = ic.row.checkpoint();
    let ty = synth_expr(env, inner, ic);
    ic.row.discharge_comptime_envelope_since(cp);
    ic.in_comptime_context = prev;
    ty
}

/// Synthesise `comptime { … }` per `inference-rules.md §1a.6`.
/// Same as [`synth_comptime`] but consumes a [`HirBlock`] — including
/// the envelope-effect discharge on exit.
pub(in crate::infer) fn synth_comptime_block(
    env: &mut TyEnv,
    block: &mut HirBlock,
    ic: &mut InferCx<'_>,
) -> TyId {
    let prev = std::mem::replace(&mut ic.in_comptime_context, true);
    let cp = ic.row.checkpoint();
    let ty = synth_block(env, block, ic);
    ic.row.discharge_comptime_envelope_since(cp);
    ic.in_comptime_context = prev;
    ty
}
