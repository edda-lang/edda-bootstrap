//! Low-level call/const emission helpers for FString lowering.
//!
//! These three helpers stage a string constant, fold two `String`
//! operands through a concat call, and emit a single-argument call
//! sealing the current block. Shared by the per-segment format dispatch
//! in [`super::format`] and the orchestrator in [`super`].

use edda_intern::Symbol;
use edda_span::Span;
use edda_types::HirExpr;

use crate::constant::{Const, ConstValue};
use crate::operand::Operand;
use crate::place::Place;
use crate::rvalue::{Rvalue, RvalueKind};
use crate::terminator::{CallArg, CallMode, FuncRef, Terminator, TerminatorKind, ThreadedCapability};
use crate::ids::{FieldIdx, VariantIdx};
use crate::ty::{FnSig, MirPrim, MirType, MirTypeKind, ParamMode};

use super::super::cfg::{alloc_block, push_assign, seal_terminator};
use super::super::ctx::LoweringContext;
use super::super::pattern::emit_extract_field_variant;
use super::super::raise::fill_dispatcher_block;

/// Emit a `ConstValue::Str(sym)` into a fresh `Str`-typed temp and return
/// `Move(temp)`.
pub(super) fn emit_str_const(ctx: &mut LoweringContext<'_>, expr: &HirExpr, sym: Symbol) -> Operand {
    let str_ty = MirType::prim(MirPrim::Str);
    let id = ctx.program.push_const(Const {
        ty: str_ty.clone(),
        value: ConstValue::Str(sym),
    });
    let temp = match ctx.body.as_mut() {
        Some(body) => body.temp(str_ty.clone(), expr.span),
        None => return Operand::Unit,
    };
    let rvalue = Rvalue {
        span: expr.span,
        kind: RvalueKind::Use(Operand::Const(id)),
        ty: str_ty,
    };
    push_assign(ctx, expr.span, temp, rvalue);
    Operand::Move(Place::local(temp))
}

/// Emit a two-argument `String`-concatenation call that returns the
/// joined result. First-choice routing goes through the pre-resolved
/// `std.fmt.string_concat` binding; falls back to the legacy extern
/// when the binding is unavailable.
pub(super) fn emit_concat_call(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    a: Operand,
    b: Operand,
) -> Operand {
    let str_ty = MirType::prim(MirPrim::Str);

    // Allocator-taking pure-Edda retarget (additive design):
    // when the enclosing function owns its own `Allocator` and already
    // admits `err: alloc.AllocError`, fold through `std.text.string.concat`
    // instead of the capability-blind `string_concat` extern below.
    // `a`/`b` are only moved once — this arm always returns when taken,
    // so the legacy path's use of `a`/`b` below is mutually exclusive.
    if let Some((alloc_effect_id, alloc_local)) = ctx.own_allocator()
        && ctx.row_admits_alloc_error()
        && let Some(binding) = ctx.alloc_fmt_bindings.concat
        && let Some(body_id) = ctx.function_map.get(&binding).copied()
    {
        let a_arg = CallArg {
            mode: CallMode::Read,
            operand: a,
        };
        let b_arg = CallArg {
            mode: CallMode::Read,
            operand: b,
        };
        let alloc_arg = CallArg {
            mode: CallMode::Read,
            operand: Operand::Copy(Place::local(alloc_local)),
        };
        return emit_call_with_capability(
            ctx,
            span,
            FuncRef::Body(body_id),
            vec![a_arg, b_arg, alloc_arg],
            ThreadedCapability::positional(alloc_effect_id, 2),
            str_ty,
        );
    }

    let dest_local = match ctx.body.as_mut() {
        Some(body) => body.temp(str_ty.clone(), span),
        None => return Operand::Unit,
    };
    let target_bb = match alloc_block(ctx) {
        Some(bb) => bb,
        None => return Operand::Unit,
    };

    let func = if let Some(binding) = ctx.fmt_bindings.string_concat
        && let Some(body_id) = ctx.function_map.get(&binding).copied()
    {
        FuncRef::Body(body_id)
    } else {
        let extern_sym = ctx.interner.intern("__edda_string_concat");
        let sig = Box::new(FnSig {
            params: vec![
                (ParamMode::Let, str_ty.clone()),
                (ParamMode::Let, str_ty.clone()),
            ],
            ret: str_ty.clone(),
            capabilities: Vec::new(),
            may_raise: Vec::new(),
            may_panic: false,
        });
        FuncRef::Extern {
            name: extern_sym,
            sig,
        }
    };

    seal_terminator(
        ctx,
        Terminator {
            span,
            kind: TerminatorKind::Call {
                func,
                args: vec![
                    CallArg {
                        mode: CallMode::Read,
                        operand: a,
                    },
                    CallArg {
                        mode: CallMode::Read,
                        operand: b,
                    },
                ],
                capabilities: Vec::new(),
                destination: Place::local(dest_local),
                target: target_bb,
                on_error: None,
            },
        },
    );

    ctx.current_bb = Some(target_bb);
    Operand::Move(Place::local(dest_local))
}

/// Emit a single-argument call (extern symbol or stdlib body), seal the
/// current block, and return `Move(dest)` from the fresh post-call
/// block. Shared by [`super::format::emit_format_call`].
pub(super) fn emit_one_arg_call(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    func: FuncRef,
    arg: CallArg,
    ret_ty: MirType,
) -> Operand {
    let dest_local = match ctx.body.as_mut() {
        Some(body) => body.temp(ret_ty.clone(), span),
        None => return Operand::Unit,
    };
    let target_bb = match alloc_block(ctx) {
        Some(bb) => bb,
        None => return Operand::Unit,
    };

    seal_terminator(
        ctx,
        Terminator {
            span,
            kind: TerminatorKind::Call {
                func,
                args: vec![arg],
                capabilities: Vec::new(),
                destination: Place::local(dest_local),
                target: target_bb,
                on_error: None,
            },
        },
    );

    ctx.current_bb = Some(target_bb);
    Operand::Move(Place::local(dest_local))
}

/// Emit a multi-argument call carrying one threaded capability, seal
/// the current block, and return the call's value from the fresh
/// post-call block. Shared by the alloc-retargeted call sites in
/// [`super::format::try_emit_alloc_format_call`] and
/// [`emit_concat_call`]'s own retarget arm. For a raising callee the
/// emitted shape mirrors `lower_try`: Result-typed destination,
/// dispatcher on `on_error`, Ok-payload extraction on the success path.
pub(super) fn emit_call_with_capability(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    func: FuncRef,
    args: Vec<CallArg>,
    capability: ThreadedCapability,
    ret_ty: MirType,
) -> Operand {
    let result_adt = match &func {
        FuncRef::Body(body_id) => ctx.function_result_adts.get(body_id).copied(),
        _ => None,
    };
    let Some(result_adt_id) = result_adt else {
        // Non-raising callee: plain call, destination typed as the
        // callee's (equal) return type, unconditional fallthrough.
        let dest_local = match ctx.body.as_mut() {
            Some(body) => body.temp(ret_ty.clone(), span),
            None => return Operand::Unit,
        };
        let target_bb = match alloc_block(ctx) {
            Some(bb) => bb,
            None => return Operand::Unit,
        };
        seal_terminator(
            ctx,
            Terminator {
                span,
                kind: TerminatorKind::Call {
                    func,
                    args,
                    capabilities: vec![capability],
                    destination: Place::local(dest_local),
                    target: target_bb,
                    on_error: None,
                },
            },
        );
        ctx.current_bb = Some(target_bb);
        return Operand::Move(Place::local(dest_local));
    };

    // Raising callee (`err: alloc.AllocError` for both retarget
    // families). The destination must be the callee's wire-level
    // `__Result<T, E>` sum and the error edge must be dispatched —
    // exactly what `lower_try` does for a source-level `call(...)?`.
    let Some(alloc_error_adt) = ctx.alloc_error_adt else {
        // Unreachable behind `row_admits_alloc_error()`, which is
        // `false` whenever `alloc_error_adt` is `None`; bail rather
        // than emit an undispatchable error edge.
        return Operand::Unit;
    };
    let result_local = match ctx.body.as_mut() {
        Some(body) => body.temp(MirType::new(MirTypeKind::Adt(result_adt_id)), span),
        None => return Operand::Unit,
    };
    let Some(dispatcher_bb) = alloc_block(ctx) else {
        return Operand::Unit;
    };
    let Some(target_bb) = alloc_block(ctx) else {
        return Operand::Unit;
    };

    seal_terminator(
        ctx,
        Terminator {
            span,
            kind: TerminatorKind::Call {
                func,
                args,
                capabilities: vec![capability],
                destination: Place::local(result_local),
                target: target_bb,
                on_error: Some(dispatcher_bb),
            },
        },
    );
    ctx.current_bb = Some(target_bb);

    let result_op = Operand::Move(Place::local(result_local));
    fill_dispatcher_block(ctx, span, dispatcher_bb, &[alloc_error_adt], &result_op);

    // Success path: extract the Ok payload (variant 0, field 0) into a
    // fresh `ret_ty`-typed local — the value the f-string sees. An Ok
    // variant with no payload (Unit success) has no LLVM representation;
    // mirror `extract_ok_payload`'s no-op in that case.
    let ok_has_payload = ctx
        .program
        .program()
        .adts
        .get(result_adt_id)
        .and_then(|adt| adt.variants.first())
        .map(|v| !v.fields.is_empty())
        .unwrap_or(true);
    if !ok_has_payload {
        return Operand::Unit;
    }
    let value_local = match ctx.body.as_mut() {
        Some(body) => body.temp(ret_ty.clone(), span),
        None => return Operand::Unit,
    };
    emit_extract_field_variant(
        ctx,
        span,
        result_local,
        Some(VariantIdx::from_raw(0)),
        FieldIdx::from_raw(0),
        value_local,
        ret_ty,
    );
    Operand::Move(Place::local(value_local))
}
