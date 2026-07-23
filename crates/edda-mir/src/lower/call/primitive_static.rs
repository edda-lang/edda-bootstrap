//! Primitive-headed static-method call lowering.
//!
//! Entries in the locked catalogue
//! [`edda_types::resolve_primitive_static_method`] (currently empty)
//! lower to a `FuncRef::Extern` Call against the variant's `__edda_*`
//! runtime symbol. The typechecker recorded the dispatch on
//! `ctx.primitive_static_calls`; this module synthesises the MIR-side
//! signature byte-for-byte against the runtime extern's C-ABI.

use edda_types::{HirCallArg, PrimitiveStaticMethod};
use edda_types::HirExpr;

use crate::ids::BlockId;
use crate::operand::Operand;
use crate::place::Place;
use crate::terminator::{CallArg, CallMode, FuncRef, TerminatorKind};
use crate::ty::{FnSig, MirType, ParamMode};

use super::super::ctx::LoweringContext;
use super::super::expr::lower_expr_to_operand;
use super::{allocate_dest_and_target, seal_call};

/// Lower a `PrimitiveStaticMethod::*` call by synthesising the
/// MIR-side [`FnSig`] for the catalogued method, lowering each argument
/// to a [`CallArg`] with the locked mode, and emitting a
/// `TerminatorKind::Call` against the runtime extern symbol. Mirrors
/// [`super::lower_call_to_binding`]'s shape but skips every binding-table
/// lookup because primitive-headed callee paths carry no resolver
/// entry.
pub(super) fn lower_primitive_static_call(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    method: PrimitiveStaticMethod,
    args: &[HirCallArg],
    on_error: Option<BlockId>,
) -> Operand {
    let (param_modes, param_tys, ret_ty) = primitive_static_signature(method);
    debug_assert_eq!(args.len(), param_tys.len());

    let mut call_args: Vec<CallArg> = Vec::with_capacity(args.len());
    for (i, arg) in args.iter().enumerate() {
        let op = lower_expr_to_operand(ctx, &arg.expr);
        let mode = match param_modes[i] {
            ParamMode::Let => CallMode::Read,
            ParamMode::Mutable => CallMode::Mutable,
            ParamMode::Take => CallMode::Take,
            ParamMode::Init => CallMode::Init,
        };
        call_args.push(CallArg { mode, operand: op });
    }

    let sig = Box::new(FnSig {
        params: param_modes
            .into_iter()
            .zip(param_tys.into_iter())
            .map(|(m, t)| (m, t))
            .collect(),
        ret: ret_ty.clone(),
        capabilities: Vec::new(),
        may_raise: Vec::new(),
        may_panic: false,
    });
    let extern_sym = ctx.interner.intern(method.extern_symbol());
    let func_ref = FuncRef::Extern {
        name: extern_sym,
        sig,
    };

    let Some((dest_local, target_bb)) = allocate_dest_and_target(ctx, expr.span, ret_ty) else {
        return Operand::Unit;
    };

    seal_call(
        ctx,
        expr.span,
        TerminatorKind::Call {
            func: func_ref,
            args: call_args,
            capabilities: Vec::new(),
            destination: Place::local(dest_local),
            target: target_bb,
            on_error,
        },
    );

    ctx.current_bb = Some(target_bb);
    Operand::Move(Place::local(dest_local))
}

/// Synthesised MIR-side signature of a primitive static method.
fn primitive_static_signature(
    method: PrimitiveStaticMethod,
) -> (Vec<ParamMode>, Vec<MirType>, MirType) {
    match method {}
}
