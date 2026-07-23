//! MIR lowering for `HirExprKind::Comptime` / `HirExprKind::ComptimeBlock`
//! (§C10).
//!
//! Evaluates a comptime expression with [`edda_comptime::EvalCx`]
//! against the active build target, threaded with the typechecker's
//! `comptime_type_paths` map and a [`edda_comptime::TypeDeclLookup`]
//! over the package `TyCx`. Successful evaluation produces a runtime
//! constant ([`crate::operand::Operand::Const`]); a failed evaluation
//! pushes the upstream diagnostic and yields the existing
//! `Operand::Unit` sentinel so caller diagnostics remain linkable.
//!
//! The surface this arm admits is the evaluator's: the predicate
//! fragment (`comptime <intlit>`, `comptime size_of(T)`,
//! `comptime align_of(T)`, `comptime target_has("...")`, arithmetic —
//! integer and float — / `if` / blocks), `let` / `var` bindings with
//! assignment, and calls to comptime-pure user
//! functions resolved through `comptime_fn_calls` + the package-wide
//! function registry. Anything outside that
//! surfaces the evaluator's "not yet supported" diagnostic and falls
//! back to `Operand::Unit`.

use std::collections::HashMap;

use edda_comptime::{
    EvalCx, FloatValue, FnDeclInfo, FnDeclLookup, IntValue, TypeDeclLookup, Value, eval_expr,
};
use edda_diag::Diagnostics;
use edda_intern::Symbol;
use edda_resolve::BindingId;
use edda_span::Span;
use edda_types::{FnSig, HirBlock, HirExpr, HirExprKind, Primitive, TyCx, TypeDeclInfo};

use crate::constant::{Const, ConstValue};
use crate::error::{LoweringError, MirError};
use crate::operand::Operand;

use super::ctx::LoweringContext;

/// Lower `comptime <expr>` to a runtime constant.
pub(crate) fn lower_comptime(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    inner: &HirExpr,
) -> Operand {
    let value = match evaluate_inner(ctx, inner) {
        Some(v) => v,
        None => return Operand::Unit,
    };
    materialise(ctx, expr.span, expr.ty, value)
}

/// Lower `comptime { … }` to a runtime constant — see [`lower_comptime`].
pub(crate) fn lower_comptime_block(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    block: &HirBlock,
) -> Operand {
    let synthetic = HirExpr {
        span: expr.span,
        ty: expr.ty,
        kind: HirExprKind::Block(block.clone()),
    };
    let value = match evaluate_inner(ctx, &synthetic) {
        Some(v) => v,
        None => return Operand::Unit,
    };
    materialise(ctx, expr.span, expr.ty, value)
}

/// Evaluate the comptime body. Requires the driver to have threaded a
/// [`edda_target::TargetCfg`] and [`edda_types::TyCx`] in; absence is
/// reported once and the caller short-circuits to `Operand::Unit`.
fn evaluate_inner(ctx: &mut LoweringContext<'_>, inner: &HirExpr) -> Option<Value> {
    let (Some(target), Some(ty_cx)) = (ctx.target_cfg, ctx.ty_cx) else {
        push_internal(
            &mut ctx.errors,
            inner.span,
            "comptime lowering requires the driver to thread `target_cfg` and `ty_cx`",
        );
        return None;
    };
    let mut diags = Diagnostics::new();
    let lookup = TyCxLookup { ty_cx };
    let fn_lookup = FnDeclMapLookup {
        decls: &ctx.comptime_fn_decls,
    };
    let mut cx = EvalCx::new(ctx.ty_interner, target, ctx.interner, &mut diags)
        .with_resolutions(ctx.comptime_type_paths)
        .with_type_decls(&lookup)
        .with_fn_calls(ctx.comptime_fn_calls)
        .with_fn_decls(&fn_lookup);
    let result = eval_expr(inner, &mut cx);
    // Drain evaluator diagnostics into the MIR error report so the
    // build's exit code reflects comptime failures. The evaluator
    // already framed each as a `Diagnostic`; we reproject onto MIR's
    // `LoweringError::InternalError` carrier so they reach
    // `Driver::run`'s diagnostic-emit path through the same channel
    // as other MIR-level failures.
    for d in diags.into_vec() {
        push_internal(&mut ctx.errors, d.primary.span, d.message.clone());
    }
    result
}

/// Lower a [`Value`] to an [`Operand`] at the expression's typed slot.
fn materialise(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    expr_ty: edda_types::TyId,
    value: Value,
) -> Operand {
    match value {
        Value::Unit => Operand::Unit,
        Value::Bool(b) => emit_const(ctx, expr_ty, ConstValue::Bool(b)),
        Value::Int(int) => emit_const(ctx, expr_ty, ConstValue::Int(int_payload(int))),
        // Same payload contract as the runtime literal lowering in
        // `expr::lower_literal`: the `f64::to_bits` pattern, with the
        // MIR type distinguishing f32 from f64 slots.
        Value::Float(float) => {
            let bits = match float {
                FloatValue::F32(v) => (v as f64).to_bits(),
                FloatValue::F64(v) => v.to_bits(),
            };
            emit_const(ctx, expr_ty, ConstValue::Float(bits))
        }
        Value::Str(sym) => emit_const(ctx, expr_ty, ConstValue::Str(sym)),
        Value::Type(_) => {
            push_internal(
                &mut ctx.errors,
                span,
                "comptime `Type` values have no runtime form",
            );
            Operand::Unit
        }
        Value::Array(_) => {
            push_internal(
                &mut ctx.errors,
                span,
                "comptime array/slice values have no runtime constant form yet",
            );
            Operand::Unit
        }
        Value::Record(_) => {
            push_internal(
                &mut ctx.errors,
                span,
                "comptime record values have no runtime constant form yet",
            );
            Operand::Unit
        }
    }
}

/// Emit a `ConstValue` into `program.consts` and return the
/// `Operand::Const(id)` referencing it. The literal-lowering helper in
/// `lower_literal` follows the same pattern.
fn emit_const(
    ctx: &mut LoweringContext<'_>,
    expr_ty: edda_types::TyId,
    value: ConstValue,
) -> Operand {
    let ty = super::ty::lower_ty(ctx.ty_interner, &ctx.adt_map, expr_ty);
    let id = ctx.program.push_const(Const { ty, value });
    Operand::Const(id)
}

/// Project a comptime [`IntValue`] onto MIR's `i128` constant payload.
///
/// Signed widths interpret the bits as `i128`; unsigned widths
/// interpret as `u128` and clamp to `i128::MAX` if necessary —
/// `size_of` / `align_of` results never approach that bound,
/// so the truncation is observable only on hand-rolled wide-unsigned
/// comptime literals.
fn int_payload(value: IntValue) -> i128 {
    match value.width() {
        Primitive::I8
        | Primitive::I16
        | Primitive::I32
        | Primitive::I64
        | Primitive::I128
        | Primitive::Isize => value.as_i128().unwrap_or(0),
        _ => {
            let u = value.as_u128().unwrap_or(0);
            if u > i128::MAX as u128 {
                i128::MAX
            } else {
                u as i128
            }
        }
    }
}

/// Push an MIR-side `InternalError` diagnostic — used both for the
/// "missing driver wiring" case and for evaluator-side failures we
/// reproject onto the MIR error channel.
fn push_internal(errors: &mut Vec<MirError>, span: Span, msg: impl Into<String>) {
    errors.push(MirError::from(LoweringError::InternalError {
        message: msg.into(),
        span,
    }));
}

/// [`TypeDeclLookup`] over the package's [`TyCx`]. Built once per
/// `lower_comptime` call so the trait object's `'a` lifetime ties to
/// the `&LoweringContext` borrow.
struct TyCxLookup<'a> {
    ty_cx: &'a TyCx,
}

impl TypeDeclLookup for TyCxLookup<'_> {
    fn lookup_type_decl(&self, binding: BindingId) -> Option<&TypeDeclInfo> {
        self.ty_cx.type_decl(binding)
    }
}

/// [`FnDeclLookup`] over the package-wide function registry the
/// lowering context built from `LoweringInput::functions`.
/// Built once per `lower_comptime` call, like
/// [`TyCxLookup`].
struct FnDeclMapLookup<'a> {
    decls: &'a HashMap<BindingId, (Symbol, &'a FnSig, &'a HirBlock)>,
}

impl FnDeclLookup for FnDeclMapLookup<'_> {
    fn lookup_fn_decl(&self, binding: BindingId) -> Option<FnDeclInfo<'_>> {
        self.decls
            .get(&binding)
            .map(|(name, sig, body)| FnDeclInfo {
                name: *name,
                sig,
                body,
            })
    }
}
