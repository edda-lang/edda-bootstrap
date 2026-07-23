//! Capability-method call lowering.
//!
//! A capability-method call like `allocator.alloc_array(u32, 4)` has no
//! Edda-side declaration — the typechecker recognises the
//! `(capability, method-name)` pair against the locked catalogue
//! (see [`edda_types::resolve_capability_method`]) and records the
//! resolution on `ic.capability_method_calls`. This module reads that
//! map at MIR-lowering time and synthesises a `TerminatorKind::Call`
//! against the variant's `__edda_*` extern symbol so the existing
//! [`super::alloc_family::maybe_rewrite_alloc_family`] rewrite kicks in
//! and prepends `size_of(T)` / `align_of(T)` constants — yielding the
//! exact wire form `__edda_alloc_array_raw` expects in `edda-rt`.
//!
//! The Type-as-value first argument is resolved through
//! `ctx.comptime_type_paths` keyed by the first argument's `HirPath`
//! span (the typechecker recorded the concrete `TyId` there via
//! [`edda_types::infer::path::synth_path`]'s comptime branch).

use edda_intern::Symbol;
use edda_span::Span;
use edda_types::{CapabilityMethod, HirCallArg, HirExpr, HirExprKind};

use crate::effect::CapabilityKind;
use crate::error::{LoweringError, MirError};
use crate::ids::{AdtId, BlockId};
use crate::operand::Operand;
use crate::place::Place;
use crate::terminator::{CallArg, CallMode, FuncRef, Terminator, TerminatorKind, ThreadedCapability};
use crate::ty::{FnSig, MirPrim, MirType, MirTypeKind, ParamMode};

use super::adt::synthesize_result_adt;
use super::cfg::{alloc_block, seal_terminator};
use super::ctx::LoweringContext;
use super::expr::lower_expr_to_operand;
use super::ty::lower_ty;

/// Lower a capability-method call (`allocator.alloc_array(T, n)` and
/// future entries in the locked catalogue). Synthesises a Call against
/// the variant's `__edda_*` extern name with `T`'s identity threaded
/// through `comptime_type_paths`; the alloc-family rewrite prepends
/// `size_of(T)` / `align_of(T)` so the runtime ABI is honoured.
pub(super) fn lower_capability_method_call(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    receiver: &HirExpr,
    method: CapabilityMethod,
    args: &[HirCallArg],
    on_error: Option<BlockId>,
) -> Operand {
    match method {
        CapabilityMethod::AllocatorAllocArray => {
            lower_alloc_array_call(ctx, expr, receiver, args, on_error)
        }
        CapabilityMethod::AllocatorFork => lower_fork_call(ctx, expr, receiver),
        CapabilityMethod::RandomDeterministic => {
            lower_random_deterministic_call(ctx, expr, receiver, args)
        }
    }
}

/// Lower `random.deterministic(seed)` to a Call against
/// `__edda_random_deterministic`.
fn lower_random_deterministic_call(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    receiver: &HirExpr,
    args: &[HirCallArg],
) -> Operand {
    debug_assert_eq!(args.len(), 1);
    let _ = lower_expr_to_operand(ctx, receiver);

    let seed_operand = lower_expr_to_operand(ctx, &args[0].expr);

    let ret_ty = MirType::new(MirTypeKind::Capability(CapabilityKind::Typed(
        edda_types::CapabilityType::DeterministicRandom,
    )));
    let sig = Box::new(FnSig {
        params: vec![(ParamMode::Let, MirType::prim(MirPrim::U64))],
        ret: ret_ty.clone(),
        capabilities: vec![CapabilityKind::Random],
        may_raise: Vec::new(),
        may_panic: false,
    });

    let capabilities = thread_receiver_capability(ctx, receiver, expr.span);
    let extern_sym = ctx
        .interner
        .intern(CapabilityMethod::RandomDeterministic.extern_symbol());
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
            args: vec![CallArg {
                mode: CallMode::Read,
                operand: seed_operand,
            }],
            capabilities,
            destination: Place::local(dest_local),
            target: target_bb,
            on_error: None,
        },
    );

    ctx.current_bb = Some(target_bb);
    Operand::Move(Place::local(dest_local))
}

/// Lower `allocator.fork()` to a Call against `__edda_heap_fork`.
fn lower_fork_call(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    receiver: &HirExpr,
) -> Operand {
    let _ = lower_expr_to_operand(ctx, receiver);

    let ret_ty = MirType::new(MirTypeKind::Capability(CapabilityKind::Allocator));
    let sig = Box::new(FnSig {
        params: Vec::new(),
        ret: ret_ty.clone(),
        capabilities: vec![CapabilityKind::Allocator],
        may_raise: Vec::new(),
        may_panic: false,
    });

    let capabilities = thread_receiver_capability(ctx, receiver, expr.span);
    let extern_sym = ctx
        .interner
        .intern(CapabilityMethod::AllocatorFork.extern_symbol());
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
            args: Vec::new(),
            capabilities,
            destination: Place::local(dest_local),
            target: target_bb,
            on_error: None,
        },
    );

    ctx.current_bb = Some(target_bb);
    Operand::Move(Place::local(dest_local))
}

/// Lower `allocator.alloc_array(T, n)` to a Call against
/// `__edda_alloc_array`.
fn lower_alloc_array_call(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    receiver: &HirExpr,
    args: &[HirCallArg],
    on_error: Option<BlockId>,
) -> Operand {
    debug_assert_eq!(args.len(), 2);
    // The receiver is the capability source. Evaluate it for side
    // effects; the alloc-family rewrite reads the capability slot via
    // `thread_capabilities`, not from this operand. We still lower it
    // so any embedded diagnostics surface.
    let _ = lower_expr_to_operand(ctx, receiver);

    // Recover the user-supplied element type `T` from the comptime-
    // type-paths map (keyed by the first arg's path span). When absent,
    // surface an upstream-defect diagnostic and fall back so the call
    // still lowers structurally.
    let element_ty = match &args[0].expr.kind {
        HirExprKind::Path(path) => match ctx.comptime_type_paths.get(&path.span).copied() {
            Some(ty_id) => lower_ty(ctx.ty_interner, &ctx.adt_map, ty_id),
            None => {
                push_missing_type_arg(ctx, expr.span);
                MirType::new(MirTypeKind::Never)
            }
        },
        _ => {
            push_missing_type_arg(ctx, expr.span);
            MirType::new(MirTypeKind::Never)
        }
    };

    // The slice-of-T return type — the source-level surface result.
    let slice_ty = MirType::new(MirTypeKind::Slice(Box::new(element_ty)));

    // Wrap in `__Result<[T], AllocError>` when AllocError is in scope.
    // `lookup_alloc_error_adt` walks `ctx.adt_map` to find the entry
    // whose registered name is `AllocError`.
    let alloc_error_name_sym = ctx.interner.intern("AllocError");
    let (ret_ty, may_raise): (MirType, Vec<AdtId>) =
        match lookup_alloc_error_adt(ctx, alloc_error_name_sym) {
            Some(err_adt_id) => {
                let result_adt_id = synthesize_result_adt(
                    ctx,
                    slice_ty.clone(),
                    vec![(alloc_error_name_sym, err_adt_id)],
                    expr.span,
                );
                (MirType::new(MirTypeKind::Adt(result_adt_id)), vec![err_adt_id])
            }
            None => (slice_ty, Vec::new()),
        };

    // Lower the user-supplied `n` arg.
    let n_operand = lower_expr_to_operand(ctx, &args[1].expr);

    // Synthesise the extern sig:
    //   params       = [(Let, usize)]       — only `n`; the receiver-Allocator
    //                                          is represented by the capability
    //                                          slot below.
    //   capabilities = [CapabilityKind::Allocator]
    //   may_raise    = [AllocError] when registered.
    //   ret          = __Result<[T], AllocError> | [T].
    let sig = Box::new(FnSig {
        params: vec![(ParamMode::Let, MirType::prim(MirPrim::Usize))],
        ret: ret_ty.clone(),
        capabilities: vec![CapabilityKind::Allocator],
        may_raise,
        may_panic: false,
    });

    // The single user-param call arg: the count `n`.
    let call_args: Vec<CallArg> = vec![CallArg {
        mode: CallMode::Read,
        operand: n_operand,
    }];

    // Thread the receiver's capability slot. The receiver expression
    // names the in-scope allocator binding (the typechecker validated
    // this); the leading capability slot at the LLVM call site reads
    // from `ctx.capabilities[allocator_sym]`.
    let mut capabilities = thread_receiver_capability(ctx, receiver, expr.span);

    let extern_sym = ctx
        .interner
        .intern(CapabilityMethod::AllocatorAllocArray.extern_symbol());
    let func_ref = FuncRef::Extern {
        name: extern_sym,
        sig,
    };

    // Apply the alloc-family rewrite — promotes `__edda_alloc_array`
    // to `__edda_alloc_array_raw` with leading size/align constants.
    let (func_ref, call_args) =
        super::alloc_family::maybe_rewrite_alloc_family(ctx, func_ref, call_args, &mut capabilities);

    let Some((dest_local, target_bb)) = allocate_dest_and_target(ctx, expr.span, ret_ty) else {
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

/// Resolve the receiver's capability source against `ctx.capabilities`
/// and return a single-entry [`ThreadedCapability`] vector for the
/// alloc-family extern's capability slot.
fn thread_receiver_capability(
    ctx: &mut LoweringContext<'_>,
    receiver: &HirExpr,
    call_span: Span,
) -> Vec<ThreadedCapability> {
    let recv_sym = match &receiver.kind {
        HirExprKind::Path(p) if p.segments.len() == 1 => Some(p.segments[0].name),
        _ => None,
    };
    let Some(name) = recv_sym else {
        ctx.errors.push(MirError::from(LoweringError::UnknownCapability {
            name: Symbol::DUMMY,
            span: call_span,
        }));
        return Vec::new();
    };
    // Chase the receiver through the narrowed-capability alias map so a
    // derived local (`let rfs = wfs.read_only()`) resolves to the
    // threaded parameter.
    let name = super::args::resolve_alias_root(ctx, name);
    match ctx.capabilities.get(&name).copied() {
        Some(id) => vec![ThreadedCapability::slot(id)],
        None => {
            ctx.errors.push(MirError::from(LoweringError::UnknownCapability {
                name,
                span: call_span,
            }));
            Vec::new()
        }
    }
}

/// Locate the `AllocError` ADT in the active program by name. Returns
/// `None` when the user's package does not import `std.mem.alloc` and
/// the ADT has not been registered.
/// Err ADTs a capability-method call at `span` propagates through `?`.
/// Capability-method sites live in `ctx.capability_method_calls`, not
/// `method_resolutions`, so `lower_try`'s generic signature lookup cannot
/// see their err row; this is the seam that lets `?` choose the
/// propagation path (err dispatch + Ok-payload extraction) over the raw
/// `__Result`-into-success-local move.
pub(super) fn capability_method_err_adts(
    ctx: &LoweringContext<'_>,
    span: Span,
) -> Option<Vec<AdtId>> {
    let method = ctx.capability_method_calls.get(&span).copied()?;
    match method {
        CapabilityMethod::AllocatorAllocArray => {
            let sym = ctx.interner.intern("AllocError");
            Some(lookup_alloc_error_adt(ctx, sym).into_iter().collect())
        }
        // `fork()` and `deterministic(seed)` are non-raising — `?` on
        // them is rejected upstream.
        CapabilityMethod::AllocatorFork => Some(Vec::new()),
        CapabilityMethod::RandomDeterministic => Some(Vec::new()),
    }
}

fn lookup_alloc_error_adt(ctx: &LoweringContext<'_>, name_sym: Symbol) -> Option<AdtId> {
    let program = ctx.program.program();
    for (_, &adt_id) in ctx.adt_map.iter() {
        let Some(def) = program.adts.get(adt_id) else {
            continue;
        };
        if def.name == name_sym {
            return Some(adt_id);
        }
    }
    None
}

/// Push the `UnsupportedHirVariant` diagnostic surfaced when the
/// typechecker did not record a `comptime_type_paths` entry for the
/// first argument — an upstream contract violation.
fn push_missing_type_arg(ctx: &mut LoweringContext<'_>, span: Span) {
    ctx.errors.push(MirError::from(LoweringError::UnsupportedHirVariant {
        variant: "capability-method (missing comptime-type-paths entry for `T`)",
        span,
    }));
}

/// Allocate the destination temp for the call's return value and the
/// post-call block. Same shape as
/// [`super::call::allocate_dest_and_target`] but kept local so the
/// sibling module does not need to re-export its helper.
fn allocate_dest_and_target(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    result_ty: MirType,
) -> Option<(crate::ids::LocalId, BlockId)> {
    let dest = ctx.body.as_mut()?.temp(result_ty, span);
    let target = alloc_block(ctx)?;
    Some((dest, target))
}

/// Seal the current block with a `TerminatorKind::Call`.
fn seal_call(ctx: &mut LoweringContext<'_>, span: Span, call: TerminatorKind) {
    seal_terminator(ctx, Terminator { span, kind: call });
}
