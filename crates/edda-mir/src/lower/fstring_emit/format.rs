//! Per-segment `format_<T>` dispatch for FString lowering.
//!
//! Maps a local's MIR type to the matching `__edda_format_<T>` runtime
//! routing (or stdlib `std.fmt.format_<T>` body when pre-resolved),
//! widening narrow integer / float operands to the wire parameter type
//! before the call.

use edda_span::Span;
use edda_types::{HirExpr, TyKind};

use crate::constant::{Const, ConstValue};
use crate::operand::Operand;
use crate::place::Place;
use crate::rvalue::{BinOp, Rvalue, RvalueKind};
use crate::terminator::{CallArg, CallMode, FuncRef, Terminator, TerminatorKind, ThreadedCapability};
use crate::ty::{FnSig, MirPrim, MirType, MirTypeKind, ParamMode};

use super::super::cfg::{alloc_block, push_assign, seal_terminator};
use super::super::ctx::LoweringContext;
use super::emit::{emit_call_with_capability, emit_one_arg_call};

/// Lower one interpolation-slot expression and emit its `format_<T>`,
/// yielding the slot's `String`-typed format.
pub(super) fn emit_format_for_slot(
    ctx: &mut LoweringContext<'_>,
    slot: &HirExpr,
) -> Option<Operand> {
    let operand = super::super::expr::lower_expr_to_operand(ctx, slot);
    if matches!(operand, Operand::Unit) && ctx.current_bb.is_none() {
        return None;
    }
    // Aggregate slot whose nominal type derives `debug`: dispatch to its
    // synthesised pure `std.core.fmt.debug_<T>.format(v) -> String`
    // formatter instead of the first-word `format_i64` fallback.
    // Structural mirror of
    // `super::super::arith::lower_nominal_eq`'s comparator dispatch.
    if let TyKind::Nominal(binding) = ctx.ty_interner.kind(slot.ty) {
        let binding = *binding;
        if let Some(fmt_fn) = ctx.debug_formatters.get(&binding).copied()
            && let Some(body_id) = ctx.function_map.get(&fmt_fn).copied()
        {
            let arg = CallArg {
                mode: CallMode::Read,
                operand,
            };
            return Some(emit_one_arg_call(
                ctx,
                slot.span,
                FuncRef::Body(body_id),
                arg,
                MirType::prim(MirPrim::Str),
            ));
        }
    }
    let ty = super::super::ty::lower_ty(ctx.ty_interner, &ctx.adt_map, slot.ty);
    let temp = ctx.body.as_mut()?.temp(ty.clone(), slot.span);
    let rvalue = Rvalue {
        span: slot.span,
        kind: RvalueKind::Use(operand),
        ty,
    };
    push_assign(ctx, slot.span, temp, rvalue);
    Some(emit_format_call(ctx, slot.span, temp))
}

/// Emit a call that produces the `String`-typed format of `local`.
/// First-choice routing is through `std.fmt.format_<T>` via the
/// pre-resolved `BindingId` on [`super::super::FmtBindings`]; if the binding
/// is unavailable, the call falls back to a direct `FuncRef::Extern`
/// against the matching `__edda_format_<T>` symbol.
pub(super) fn emit_format_call(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    local_id: crate::ids::LocalId,
) -> Operand {
    let local_ty = match ctx
        .body
        .as_ref()
        .and_then(|b| b.body_ref().locals.get(local_id))
    {
        Some(decl) => decl.ty.clone(),
        None => return Operand::Unit,
    };

    if let MirTypeKind::Prim(MirPrim::I128) = local_ty.kind {
        return emit_wide_format_call(ctx, span, local_id, true);
    }
    if let MirTypeKind::Prim(MirPrim::U128) = local_ty.kind {
        return emit_wide_format_call(ctx, span, local_id, false);
    }

    if let Some(operand) = try_emit_alloc_format_call(ctx, span, local_id, &local_ty) {
        return operand;
    }

    let (extern_name, param_ty, binding) = format_routing_for_type(ctx, &local_ty);
    let str_ty = MirType::prim(MirPrim::Str);
    let arg_operand = widen_operand_to_param_ty(ctx, span, local_id, &local_ty, &param_ty);
    let arg = CallArg {
        mode: CallMode::Read,
        operand: arg_operand,
    };

    if let Some(binding) = binding
        && let Some(body_id) = ctx.function_map.get(&binding).copied()
    {
        return emit_one_arg_call(ctx, span, FuncRef::Body(body_id), arg, str_ty);
    }

    let extern_sym = ctx.interner.intern(extern_name);
    let sig = Box::new(FnSig {
        params: vec![(ParamMode::Let, param_ty)],
        ret: str_ty.clone(),
        capabilities: Vec::new(),
        may_raise: Vec::new(),
        may_panic: false,
    });
    emit_one_arg_call(
        ctx,
        span,
        FuncRef::Extern {
            name: extern_sym,
            sig,
        },
        arg,
        str_ty,
    )
}

/// Attempt the allocator-taking pure-Edda retarget for a format-call
/// slot. Returns `Some(operand)` when both preconditions hold and the
/// matching [`super::super::AllocFmtBindings`] field resolved to
/// a real body; `None` otherwise, signalling the caller to fall through
/// to the legacy `FmtBindings`/hardcoded-extern path unchanged.
fn try_emit_alloc_format_call(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    local_id: crate::ids::LocalId,
    local_ty: &MirType,
) -> Option<Operand> {
    let (alloc_effect_id, alloc_local) = ctx.own_allocator()?;
    if !ctx.row_admits_alloc_error() {
        return None;
    }
    let target_binding = alloc_target_for_type(ctx, local_ty)?;
    let body_id = ctx.function_map.get(&target_binding).copied()?;

    let str_ty = MirType::prim(MirPrim::Str);
    let wire_ty = alloc_wire_ty(local_ty);
    let value_operand = widen_operand_to_param_ty(ctx, span, local_id, local_ty, &wire_ty);
    let value_arg = CallArg {
        mode: CallMode::Read,
        operand: value_operand,
    };
    let alloc_arg = CallArg {
        mode: CallMode::Read,
        operand: Operand::Copy(Place::local(alloc_local)),
    };

    Some(emit_call_with_capability(
        ctx,
        span,
        FuncRef::Body(body_id),
        vec![value_arg, alloc_arg],
        ThreadedCapability::positional(alloc_effect_id, 1),
        str_ty,
    ))
}

/// The [`super::super::AllocFmtBindings`] field matching `ty`,
/// or `None` when `ty` has no allocator-taking pure-Edda target (`Str`,
/// non-primitive, or `I128`/`U128` — the latter never reach here, see
/// [`emit_wide_format_call`]).
fn alloc_target_for_type(
    ctx: &LoweringContext<'_>,
    ty: &MirType,
) -> Option<edda_resolve::BindingId> {
    match &ty.kind {
        MirTypeKind::Prim(
            MirPrim::I8 | MirPrim::I16 | MirPrim::I32 | MirPrim::I64 | MirPrim::Isize,
        ) => ctx.alloc_fmt_bindings.i64_to_string,
        MirTypeKind::Prim(
            MirPrim::U8 | MirPrim::U16 | MirPrim::U32 | MirPrim::U64 | MirPrim::Usize,
        ) => ctx.alloc_fmt_bindings.u64_to_string,
        MirTypeKind::Prim(MirPrim::F32 | MirPrim::F64) => ctx.alloc_fmt_bindings.f64_to_string,
        MirTypeKind::Prim(MirPrim::Bool) => ctx.alloc_fmt_bindings.bool_to_string,
        _ => None,
    }
}

/// The wire parameter type [`alloc_target_for_type`]'s callee declares
/// for `ty`'s classification.
fn alloc_wire_ty(ty: &MirType) -> MirType {
    match &ty.kind {
        MirTypeKind::Prim(
            MirPrim::I8 | MirPrim::I16 | MirPrim::I32 | MirPrim::I64 | MirPrim::Isize,
        ) => MirType::prim(MirPrim::I64),
        MirTypeKind::Prim(
            MirPrim::U8 | MirPrim::U16 | MirPrim::U32 | MirPrim::U64 | MirPrim::Usize,
        ) => MirType::prim(MirPrim::U64),
        MirTypeKind::Prim(MirPrim::F32 | MirPrim::F64) => MirType::prim(MirPrim::F64),
        _ => ty.clone(),
    }
}

/// Widen the operand naming `local_id` to match `param_ty` when the
/// formatter's declared parameter is wider than the source local. The
/// only reason this exists is fstring slot lowering: narrow integer
/// locals (`i8`/`i16`/`i32`, `u8`/`u16`/`u32`) and `f32` flow into
/// formatters declared with `i64`/`u64`/`f64` parameters, and the LLVM
/// backend's raw load+call sequence does not widen on its own.
fn widen_operand_to_param_ty(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    local_id: crate::ids::LocalId,
    local_ty: &MirType,
    param_ty: &MirType,
) -> Operand {
    let raw = Operand::Copy(Place::local(local_id));
    if local_ty == param_ty {
        return raw;
    }
    let (MirTypeKind::Prim(src_prim), MirTypeKind::Prim(dst_prim)) = (&local_ty.kind, &param_ty.kind)
    else {
        return raw;
    };
    if src_prim == dst_prim {
        return raw;
    }
    let temp = match ctx.body.as_mut() {
        Some(body) => body.temp(param_ty.clone(), span),
        None => return raw,
    };
    let rvalue = Rvalue {
        span,
        kind: RvalueKind::Cast {
            src: raw,
            src_prim: *src_prim,
            dst_prim: *dst_prim,
        },
        ty: param_ty.clone(),
    };
    push_assign(ctx, span, temp, rvalue);
    Operand::Move(Place::local(temp))
}

/// Return `(extern_name, wire_param_type, stdlib_binding)` for the
/// format-call dispatch on `ty`. The wire type is the actual LLVM-level
/// parameter type — narrower integers are widened to `i64` / `u64` so
/// the extern ABI stays simple. The stdlib binding is the pre-resolved
/// `std.fmt.format_<T>` id; when present, the call routes through that
/// binding's body (which itself carries the extern declaration).
fn format_routing_for_type(
    ctx: &LoweringContext<'_>,
    ty: &MirType,
) -> (&'static str, MirType, Option<edda_resolve::BindingId>) {
    match &ty.kind {
        MirTypeKind::Prim(MirPrim::I8)
        | MirTypeKind::Prim(MirPrim::I16)
        | MirTypeKind::Prim(MirPrim::I32)
        | MirTypeKind::Prim(MirPrim::I64)
        | MirTypeKind::Prim(MirPrim::Isize) => (
            "__edda_format_i64",
            MirType::prim(MirPrim::I64),
            ctx.fmt_bindings.format_i64,
        ),
        MirTypeKind::Prim(MirPrim::U8)
        | MirTypeKind::Prim(MirPrim::U16)
        | MirTypeKind::Prim(MirPrim::U32)
        | MirTypeKind::Prim(MirPrim::U64)
        | MirTypeKind::Prim(MirPrim::Usize) => (
            "__edda_format_u64",
            MirType::prim(MirPrim::U64),
            ctx.fmt_bindings.format_u64,
        ),
        MirTypeKind::Prim(MirPrim::F32) | MirTypeKind::Prim(MirPrim::F64) => (
            "__edda_format_f64",
            MirType::prim(MirPrim::F64),
            ctx.fmt_bindings.format_f64,
        ),
        MirTypeKind::Prim(MirPrim::Bool) => (
            "__edda_format_bool",
            MirType::prim(MirPrim::Bool),
            ctx.fmt_bindings.format_bool,
        ),
        MirTypeKind::Prim(MirPrim::Str) => (
            "__edda_format_str",
            MirType::prim(MirPrim::Str),
            ctx.fmt_bindings.format_str,
        ),
        // Bootstrap fallback: treat everything else as i64.
        _ => (
            "__edda_format_i64",
            MirType::prim(MirPrim::I64),
            ctx.fmt_bindings.format_i64,
        ),
    }
}

/// Emit a call that produces the `String`-typed format of a 128-bit
/// integer local, bypassing [`format_routing_for_type`] entirely. The
/// local is split into its raw low/high 64-bit halves with two `Cast`
/// rvalues and a `Shr`, then passed as two `u64` arguments to the
/// matching lohi extern — the only ABI-safe way to cross this local's
/// value into the Rust runtime.
fn emit_wide_format_call(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    local_id: crate::ids::LocalId,
    signed: bool,
) -> Operand {
    let wide_prim = if signed { MirPrim::I128 } else { MirPrim::U128 };
    let wide_ty = MirType::prim(wide_prim);
    let u64_ty = MirType::prim(MirPrim::U64);
    let str_ty = MirType::prim(MirPrim::Str);

    let raw = Operand::Copy(Place::local(local_id));

    let lo_temp = match ctx.body.as_mut() {
        Some(body) => body.temp(u64_ty.clone(), span),
        None => return Operand::Unit,
    };
    push_assign(
        ctx,
        span,
        lo_temp,
        Rvalue {
            span,
            kind: RvalueKind::Cast {
                src: raw.clone(),
                src_prim: wide_prim,
                dst_prim: MirPrim::U64,
            },
            ty: u64_ty.clone(),
        },
    );

    let shift_const = ctx.program.push_const(Const {
        ty: wide_ty.clone(),
        value: ConstValue::Int(64),
    });
    let shifted_temp = match ctx.body.as_mut() {
        Some(body) => body.temp(wide_ty.clone(), span),
        None => return Operand::Unit,
    };
    push_assign(
        ctx,
        span,
        shifted_temp,
        Rvalue {
            span,
            kind: RvalueKind::BinOp {
                op: BinOp::Shr,
                lhs: raw,
                rhs: Operand::Const(shift_const),
                prim: wide_prim,
            },
            ty: wide_ty.clone(),
        },
    );

    let hi_temp = match ctx.body.as_mut() {
        Some(body) => body.temp(u64_ty.clone(), span),
        None => return Operand::Unit,
    };
    push_assign(
        ctx,
        span,
        hi_temp,
        Rvalue {
            span,
            kind: RvalueKind::Cast {
                src: Operand::Move(Place::local(shifted_temp)),
                src_prim: wide_prim,
                dst_prim: MirPrim::U64,
            },
            ty: u64_ty.clone(),
        },
    );

    let extern_name = if signed {
        "__edda_format_i128_lohi"
    } else {
        "__edda_format_u128_lohi"
    };
    let extern_sym = ctx.interner.intern(extern_name);
    let sig = Box::new(FnSig {
        params: vec![(ParamMode::Let, u64_ty.clone()), (ParamMode::Let, u64_ty.clone())],
        ret: str_ty.clone(),
        capabilities: Vec::new(),
        may_raise: Vec::new(),
        may_panic: false,
    });

    let dest_local = match ctx.body.as_mut() {
        Some(body) => body.temp(str_ty.clone(), span),
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
                func: FuncRef::Extern {
                    name: extern_sym,
                    sig,
                },
                args: vec![
                    CallArg {
                        mode: CallMode::Read,
                        operand: Operand::Move(Place::local(lo_temp)),
                    },
                    CallArg {
                        mode: CallMode::Read,
                        operand: Operand::Move(Place::local(hi_temp)),
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
