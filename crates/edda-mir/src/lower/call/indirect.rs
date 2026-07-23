//! Indirect (fn-pointer value) call lowering.
//!
//! Lowers a `Call` whose callee resolved to a first-class fn-pointer
//! value rather than a Path-to-Function. The callee operand is
//! materialised, the MIR-side `FnSig` recovered from the callee's typed
//! HIR type, and a `FuncRef::Indirect` Call terminator emitted.

use edda_types::{HirCallArg, HirCallMode, HirExpr};

use crate::effect::CapabilityKind;
use crate::error::{LoweringError, MirError};
use crate::ids::BlockId;
use crate::operand::Operand;
use crate::place::Place;
use crate::terminator::{CallArg, CallMode, FuncRef, TerminatorKind, ThreadedCapability};
use crate::ty::MirTypeKind;

use super::super::ctx::LoweringContext;
use super::super::expr::lower_expr_to_operand;
use super::super::ty::lower_ty;
use super::{allocate_dest_and_target, lower_arg_operand, seal_call};

/// Lower a Call whose callee is *not* a Path-to-Function (i.e., a
/// first-class fn-pointer value). The typechecker has already proven
/// the callee's `ty` is `TyKind::FnPtr(sig)`; here we materialise the
/// callee operand, copy out the signature, and emit a
/// `FuncRef::Indirect` Call terminator.
pub(super) fn lower_indirect_call(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    callee: &HirExpr,
    args: &[HirCallArg],
    on_error: Option<BlockId>,
) -> Operand {
    // The MIR-side fn-ptr signature is recovered from the callee's
    // typed-HIR type. `lower_ty` reads `TyKind::FnPtr(sig)` and
    // produces `MirTypeKind::FnPtr(box mir_sig)`.
    let callee_mir_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, callee.ty);
    let MirTypeKind::FnPtr(mir_sig) = &callee_mir_ty.kind else {
        // The typechecker should reject non-FnPtr callees in
        // synth_indirect_call. Reaching here means the typed HIR is
        // out of sync — surface a structural error rather than emit
        // malformed MIR.
        ctx.errors.push(MirError::from(LoweringError::UnsupportedHirVariant {
            variant: "Call.callee (non-FnPtr indirect callee)",
            span: callee.span,
        }));
        for arg in args.iter() {
            let _ = lower_expr_to_operand(ctx, &arg.expr);
        }
        return Operand::Unit;
    };
    let mut mir_sig: crate::ty::FnSig = (**mir_sig).clone();

    // Raising fn-VALUE: `lower_fn_ptr_sig` left `mir_sig.ret` as the bare
    // success `T`, but the callee body/shim returns the wire-level
    // `__Result<T, E>` ADT. The value-construction seams
    // (`super::expr::overwrite_raising_fn_ptr_ret`,
    // `super::closure::project_fn_ptr_result_ret`) reconcile only their own
    // copy of the sig — NOT this parameter-typed callee's view, which is
    // `lower_ty`'d straight from the declared `function(...) -> T with {err}`
    // type. Project `ret` here so the destination alloca and the
    // `FuncRef::Indirect` sig the backend reads carry the same
    // `{ tag, payload }` shape the shim writes through its sret pointer.
    // Without it the callee overruns a success-sized slot and corrupts the
    // stack.
    let projected_ret =
        super::super::adt::wire_level_ret(ctx, mir_sig.ret.clone(), &mir_sig.may_raise, expr.span);
    mir_sig.ret = projected_ret;

    // Lower the callee operand. The Edda type-side has Param/Local
    // bindings storing the fn-pointer; `lower_expr_to_operand` handles
    // the read.
    let callee_operand = lower_expr_to_operand(ctx, callee);

    // Lower arguments positionally. Modes come from the MIR signature
    // payload — no edda-types FnSig lookup needed because the FnPtr
    // already carries everything.
    let mut call_args: Vec<CallArg> = Vec::with_capacity(args.len());
    for (i, arg) in args.iter().enumerate() {
        let mode = match mir_sig.params.get(i) {
            Some((pm, _)) => CallMode::from_param_mode(*pm),
            None => match arg.mode {
                Some(HirCallMode::Mutable) => CallMode::Mutable,
                Some(HirCallMode::Take) => CallMode::Take,
                Some(HirCallMode::Init) => CallMode::Init,
                None => CallMode::Read,
            },
        };
        let op = lower_arg_operand(ctx, &arg.expr, mode);
        call_args.push(CallArg { mode, operand: op });
    }

    let result_ty = mir_sig.ret.clone();

    // Thread caller capabilities into the indirect call. The MIR
    // `FnSig` stored on the fn-pointer carries one
    // `CapabilityKind::Named(symbol)` per slot (see
    // `lower::ty::lower_fn_ptr_sig`); the symbol is the callee's
    // capability-parameter name, which after typechecker substitution
    // matches the caller's `ctx.capabilities` key. Missing keys here
    // would be a typechecker bug — the caller's row must cover the
    // callee's, or the call wouldn't have type-checked.
    let mut capabilities: Vec<ThreadedCapability> =
        Vec::with_capacity(mir_sig.capabilities.len());
    for kind in &mir_sig.capabilities {
        let CapabilityKind::Named(name) = kind else {
            // `lower_fn_ptr_sig` produces only `Named` variants today;
            // a non-`Named` slot here is a future addition that needs
            // its own threading rule.
            ctx.errors.push(MirError::from(LoweringError::UnsupportedHirVariant {
                variant: "Call.callee (indirect call with non-Named capability slot)",
                span: callee.span,
            }));
            continue;
        };
        // Chase the slot name through the narrowed-capability alias map
        // before the lookup so an indirect call whose capability slot was
        // satisfied by a derived local (`let rfs = wfs.read_only()`)
        // resolves to the threaded parameter.
        // Accounting-only pairing: the fn-pointer type carries no
        // param-name↔position binding to pair a positional value from,
        // so indirect calls keep the slot-loaded value.
        let key = super::super::args::resolve_alias_root(ctx, *name);
        match ctx.capabilities.get(&key).copied() {
            Some(id) => capabilities.push(ThreadedCapability::slot(id)),
            None => {
                ctx.errors.push(MirError::from(LoweringError::UnknownCapability {
                    name: *name,
                    span: expr.span,
                }));
            }
        }
    }

    let func_ref = FuncRef::Indirect {
        callee: callee_operand,
        sig: Box::new(mir_sig),
    };
    let Some((dest_local, target_bb)) = allocate_dest_and_target(ctx, expr.span, result_ty)
    else {
        return Operand::Unit;
    };

    seal_call(
        ctx,
        expr.span,
        TerminatorKind::Call {
            func: func_ref,
            args: call_args,
            capabilities,
            destination: Place::local(dest_local),
            target: target_bb,
            on_error,
        },
    );

    ctx.current_bb = Some(target_bb);
    Operand::Move(Place::local(dest_local))
}
