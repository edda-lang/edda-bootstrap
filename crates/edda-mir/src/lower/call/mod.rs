//! `HirExprKind::Call` -> `TerminatorKind::Call` lowering.
//!
//! Lowering for the in-package function call. Resolves the callee
//! through the `function_symbols` -> `function_map` chain, evaluates
//! every argument operand in declaration order, threads the caller's
//! capabilities for each capability the callee declares in its effect row,
//! and seals the current block with `TerminatorKind::Call` branching to a
//! fresh post-call block. The destination is a fresh user-temp local that
//! callers read via `Operand::Move`.
//!
//! [`lower_call`] takes an `on_error: Option<BlockId>`
//! parameter so the `?`-propagation entry point in [`super::raise`] can
//! reuse the same lowering and only differ in the terminator's on-error
//! successor. The plain `Call` arm in [`super::expr`] continues to pass
//! `None`. Multi-segment paths are not yet supported; extern calls (primitive-static / alloc-family) and indirect fn-pointer calls are lowered here.
//!
//! # Layout
//!
//! - [`method`] — `MethodCall` desugaring to a receiver-prepended `Call`.
//! - [`indirect`] — fn-pointer-value (`FuncRef::Indirect`) calls.
//! - [`primitive_static`] — primitive-headed static-method extern calls.
//! - this file — the `Call` dispatch entry point, the shared
//!   binding-resolved tail [`lower_call_to_binding`], and the
//!   argument / destination helpers the submodules reuse.

mod indirect;
mod method;
mod primitive_static;

use edda_intern::Symbol;
use edda_resolve::BindingId;
use edda_span::Span;
use edda_types::{HirCallArg, HirExpr, HirExprKind, ReturnMode};

use crate::error::{LoweringError, MirError};
use crate::ids::{BlockId, LocalId};
use crate::operand::Operand;
use crate::place::{Place, Projection};
use crate::terminator::{CallArg, CallMode, FuncRef, Terminator, TerminatorKind};
use crate::ty::{MirPrim, MirType, MirTypeKind};

use super::cfg::{alloc_block, seal_terminator};
use super::ctx::LoweringContext;
use super::expr::lower_expr_to_operand;
use super::ty::lower_ty;

use indirect::lower_indirect_call;
pub(in crate::lower) use method::{lower_method_call, lower_method_call_with_on_error};

/// Lower a `HirExprKind::Call { callee, args }` into a `TerminatorKind::Call`
/// sealing the current block and continuing into a fresh post-call block.
/// Returns the operand naming the call's return value.
///
/// `on_error` is the propagation successor for `?`-style error propagation.
/// Plain `Call` arms pass `None`; the [`super::raise::lower_try`] entry
/// allocates a propagation block first and passes `Some(propagation_bb)`.
pub(in crate::lower) fn lower_call(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    callee: &HirExpr,
    args: &[HirCallArg],
    on_error: Option<BlockId>,
) -> Operand {
    // Variant-constructor call site: `<Type>.<variant>(args)` where the
    // path resolves to a sum-type ADT. The typechecker validated the
    // payload shape; here we synthesise `MakeVariant` rather than a
    // function call. This must run before `resolve_callee_binding` because
    // a sum TypeDecl binding is not in `function_map`.
    if let Some((adt_id, variant_idx)) =
        super::variant_ctor_call::resolve_variant_constructor(ctx, callee)
    {
        return super::variant_ctor_call::lower_variant_constructor_call(
            ctx, expr, adt_id, variant_idx, args,
        );
    }
    // Primitive-headed static-method call: entries in the locked
    // catalogue [`edda_types::resolve_primitive_static_method`]
    // (currently empty). The typechecker already verified arity / arg
    // types and recorded the dispatch on `primitive_static_calls`
    // keyed by call span — the resolver gives these paths no binding,
    // so lowering reads the map directly and emits a `FuncRef::Extern`
    // against the variant's `__edda_*` runtime symbol. Runs before
    // `try_resolve_function_binding` because the callee path carries
    // no resolver entry to find.
    if let Some(&method) = ctx.primitive_static_calls.get(&expr.span) {
        return primitive_static::lower_primitive_static_call(ctx, expr, method, args, on_error);
    }
    // Direct named-function call: callee is a Path that names a body
    // or extern in this program. `try_resolve_function_binding` is the
    // diagnostic-free probe; we fall through to the indirect path when
    // it returns None.
    if let Some(binding) = try_resolve_function_binding(ctx, callee) {
        return lower_call_to_binding(ctx, expr, binding, None, args, on_error);
    }
    // Indirect call: the typechecker proved the callee's type is
    // `TyKind::FnPtr(sig)`. Lower the callee to an operand and emit a
    // `FuncRef::Indirect` Call terminator.
    lower_indirect_call(ctx, expr, callee, args, on_error)
}

/// Lower one call argument to its [`Operand`], honouring the resolved
/// call `mode`. By-reference modes (`Mutable` / `Init`) resolve the
/// argument expression to the lvalue [`Place`] it names via
/// [`super::stmt::resolve_place`] — so a field- or index-projected
/// argument (`mutable map.files`) threads the callee's writes back to
/// the caller's storage rather than to a discarded copy. The one
/// exception is a `mutable` slice subrange (`mutable buf[lo..<hi]`),
/// which materialises an aliasing view temp via the value path.
/// By-value modes lower through [`lower_expr_to_operand`] unchanged.
pub(super) fn lower_arg_operand(
    ctx: &mut LoweringContext<'_>,
    arg_expr: &HirExpr,
    mode: CallMode,
) -> Operand {
    if matches!(mode, CallMode::Mutable | CallMode::Init) {
        // `mutable data[lo..<hi]` / `mutable data[lo..]`: the subrange
        // is not a place — route through the regular value lowering
        // (`slice::lower_index` → `SliceSubrange`) and hand the call
        // the view temp's place.
        if mode == CallMode::Mutable
            && let HirExprKind::Index { index, .. } = &arg_expr.kind
            && matches!(index.kind, HirExprKind::Range { .. })
        {
            return match lower_expr_to_operand(ctx, arg_expr) {
                Operand::Move(place) | Operand::Copy(place) => Operand::Copy(place),
                other => other,
            };
        }
        return match super::stmt::resolve_place(ctx, arg_expr) {
            Some(place) => Operand::Copy(place),
            None => Operand::Unit,
        };
    }
    lower_expr_to_operand(ctx, arg_expr)
}

/// Lower a function call given a pre-resolved binding. Shared by the
/// regular `Call` path (after `resolve_callee_binding` produces the
/// binding) and the `MethodCall` path (after `method_resolutions`
/// produces the binding). When `implicit_receiver` is `Some` the
/// receiver is prepended at argument position 0; the callee's
/// declared `params[0]` is the receiver's type, validated by the
/// typechecker.
pub(super) fn lower_call_to_binding(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    binding: BindingId,
    implicit_receiver: Option<&HirExpr>,
    args: &[HirCallArg],
    on_error: Option<BlockId>,
) -> Operand {
    let func_ref = match ctx.function_map.get(&binding).copied() {
        Some(body_id) => FuncRef::Body(body_id),
        None => match ctx.function_externs.get(&binding) {
            Some((extern_symbol, extern_sig)) => FuncRef::Extern {
                name: *extern_symbol,
                sig: Box::new(extern_sig.clone()),
            },
            None => {
                push_unknown_function(ctx, Symbol::DUMMY, expr.span);
                if let Some(r) = implicit_receiver {
                    let _ = lower_expr_to_operand(ctx, r);
                }
                for arg in args.iter() {
                    let _ = lower_expr_to_operand(ctx, &arg.expr);
                }
                return Operand::Unit;
            }
        },
    };
    let Some(sig) = ctx.function_sigs.get(&binding).copied() else {
        push_internal_error(
            ctx,
            expr.span,
            "function_sigs missing entry for registered binding",
        );
        return Operand::Unit;
    };

    // Build the full CallArg list. When an implicit receiver is
    // present, evaluate it first and pair it with the callee's
    // `params[0]` mode (the typechecker has already validated mode
    // compatibility — receivers in method-call position default to
    // `Read`).
    let mut call_args: Vec<CallArg> = Vec::with_capacity(args.len() + 1);
    if let Some(receiver) = implicit_receiver {
        let recv_mode = super::args::call_arg_mode(sig, 0, None);
        let recv_op = lower_arg_operand(ctx, receiver, recv_mode);
        call_args.push(CallArg {
            mode: recv_mode,
            operand: recv_op,
        });
    }
    let user_base = call_args.len();
    for (i, arg) in args.iter().enumerate() {
        let mode = super::args::call_arg_mode(sig, user_base + i, arg.mode);
        let op = lower_arg_operand(ctx, &arg.expr, mode);
        call_args.push(CallArg { mode, operand: op });
    }

    // Extern-bodied callees receive their capability slots in DECLARED
    // PARAMETER order (the caps-first wire contract their
    // implementations are written against); body-backed callees keep the
    // canonical row order their own `lower_effect_row` slot registration
    // uses.
    let extern_param_order = matches!(func_ref, FuncRef::Extern { .. });
    let mut capabilities = super::args::thread_capabilities(
        ctx,
        sig,
        implicit_receiver,
        args,
        &call_args,
        expr.span,
        extern_param_order,
    );
    // The destination's MIR type must be the callee's actual return type, NOT `expr.ty`
    // (the typechecker-side success type). For raising functions — source-bodied via
    // `function_result_adts` or extern via `register_externs` — the actual return is
    // a `__Result<T, E>` sum. Sizing the alloca for `expr.ty` (= T) would overrun on
    // the call's store-back. `lower_try` is responsible for extracting the Ok payload
    // back to `expr.ty` along the success path.
    // A non-raising return-position borrow (`-> let/mutable T`) callee
    // physically returns a `HeapPtr` (the address of the borrowed
    // place); the call's value is a `Deref` place over that pointer so a
    // downstream `mutable` write threads back.
    let borrow_pointee = borrow_return_pointee(ctx, &func_ref, sig, expr.ty);
    let result_ty = match &borrow_pointee {
        Some(_) => MirType::prim(MirPrim::HeapPtr),
        None => callee_actual_ret_ty(ctx, &func_ref, expr.ty),
    };
    let Some((dest_local, target_bb)) = allocate_dest_and_target(ctx, expr.span, result_ty)
    else {
        return Operand::Unit;
    };

    // Option-C runtime ABI: if the callee is one of the six
    // reserved alloc-family extern names, rewrite to the
    // `_raw`-suffixed runtime symbol and prepend `size_of(T)` /
    // `align_of(T)` constants. The runtime is type-erased so the layout
    // constants are how it sizes / aligns each allocation. The rewrite
    // shifts every recorded capability `value_arg` by the two injected
    // leading constants.
    let (func_ref, call_args) =
        super::alloc_family::maybe_rewrite_alloc_family(ctx, func_ref, call_args, &mut capabilities);

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
    match borrow_pointee {
        Some(pointee) => {
            let mut place = Place::local(dest_local);
            place.projection.push(Projection::Deref(pointee));
            Operand::Copy(place)
        }
        None => Operand::Move(Place::local(dest_local)),
    }
}

//            whose signature declares a `-> let/mutable T` return; this
//            mirrors `lower_function`'s definition-side guard, which only
//            lowers the body return type to `HeapPtr` in the same case
/// The borrowed value type `T` when `func_ref` names a non-raising
/// return-position borrow function, else `None`. The caller sizes the
/// call destination as `HeapPtr` and reads it back through a
/// `Projection::Deref(T)`.
fn borrow_return_pointee(
    ctx: &LoweringContext<'_>,
    func_ref: &FuncRef,
    sig: &edda_types::FnSig,
    expr_ty: edda_types::TyId,
) -> Option<MirType> {
    if sig.return_mode == ReturnMode::ByValue {
        return None;
    }
    match func_ref {
        FuncRef::Body(body_id) if !ctx.function_result_adts.contains_key(body_id) => {
            Some(lower_ty(ctx.ty_interner, &ctx.adt_map, expr_ty))
        }
        _ => None,
    }
}

/// Probe-style variant of [`resolve_callee_binding`]: returns
/// `Some(binding)` only when the callee is a Path that names a known
/// function binding. Returns `None` silently for every other shape
/// (non-Path callees, paths to non-function bindings, multi-segment
/// paths that don't resolve to a function, etc.) so the caller can
/// route to the indirect-call path without first emitting a stale
/// diagnostic.
pub(in crate::lower) fn try_resolve_function_binding(
    ctx: &LoweringContext<'_>,
    callee: &HirExpr,
) -> Option<BindingId> {
    use edda_resolve::Resolved;
    let HirExprKind::Path(path) = &callee.kind else {
        return None;
    };
    if let Some(Resolved::Binding(id)) = ctx.resolutions.lookup_path(path.span)
        && (ctx.function_map.contains_key(&id) || ctx.function_externs.contains_key(&id))
    {
        return Some(id);
    }
    if path.segments.len() == 1 {
        return ctx.function_symbols.get(&path.segments[0].name).copied();
    }
    None
}



/// Allocate the destination temp for the call's return value and the
/// post-call block control flow continues into on normal return.
pub(super) fn allocate_dest_and_target(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    result_ty: MirType,
) -> Option<(LocalId, BlockId)> {
    let dest = match ctx.body.as_mut() {
        Some(body) => body.temp(result_ty, span),
        None => return None,
    };
    let target = alloc_block(ctx)?;
    Some((dest, target))
}

/// Project the callee's actual MIR-side return type for the call's
/// destination alloca.
///
/// The typed-HIR's `expr.ty` is the source-level success type `T` —
/// after the `?` operator's effect-row discharge. The callee's actual
/// wire-level return, however, is `__Result<T, E1, ...>` for raising
/// functions. The destination alloca must match the wire-level shape
/// so the store-back of the call result does not overrun.
fn callee_actual_ret_ty(
    ctx: &LoweringContext<'_>,
    func_ref: &FuncRef,
    expr_ty: edda_types::TyId,
) -> MirType {
    match func_ref {
        FuncRef::Body(body_id) => match ctx.function_result_adts.get(body_id) {
            Some(&result_adt_id) => MirType::new(MirTypeKind::Adt(result_adt_id)),
            None => lower_ty(ctx.ty_interner, &ctx.adt_map, expr_ty),
        },
        FuncRef::Extern { sig, .. } => sig.ret.clone(),
        // Indirect callees carry their full signature inline, and
        // `lower_indirect_call` has already projected a raising `sig.ret`
        // to the wire-level `__Result<T, E>` ADT (via
        // `super::adt::wire_level_ret`), so the return type comes directly
        // from it — no `function_result_adts` lookup, which keys on a known
        // body the fn-pointer value does not name.
        FuncRef::Indirect { sig, .. } => sig.ret.clone(),
    }
}

/// Seal the current block with a `TerminatorKind::Call`.
pub(super) fn seal_call(ctx: &mut LoweringContext<'_>, span: Span, call: TerminatorKind) {
    seal_terminator(ctx, Terminator { span, kind: call });
}

/// Push `UnknownFunction { name, span }`.
pub(super) fn push_unknown_function(ctx: &mut LoweringContext<'_>, name: Symbol, span: Span) {
    ctx.errors.push(MirError::from(LoweringError::UnknownFunction {
        name,
        span,
    }));
}

/// Push `InternalError { message, span }`.
fn push_internal_error(ctx: &mut LoweringContext<'_>, span: Span, message: &str) {
    ctx.errors.push(MirError::from(LoweringError::InternalError {
        message: message.to_string(),
        span,
    }));
}
