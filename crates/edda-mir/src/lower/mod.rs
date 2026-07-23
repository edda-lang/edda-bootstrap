//! Typed-HIR -> MIR lowering pass.
//!
//! # Overview
//!
//! The pass takes [`LoweringInput`] (a borrowed `Interner`, `TyInterner`,
//! and slice of [`FunctionInput`]s) and walks each function body, producing
//! an [`MirProgram`] via the `expr`, `stmt`, `pattern`, `effect`, and `ty`
//! submodules. Short-circuit `&&` / `||` lower to `SwitchBool` over the lhs;
//! user-`let` bindings emit paired `StorageLive` / `StorageDead` over a
//! lexical-scope stack; refutable `Tuple` patterns inside `match` arms
//! decompose into sequential `ExtractField` rvalues plus recursive
//! sub-pattern tests.
//!
//! Two pre-passes run before any function body is walked:
//!   - [`adt::register_type_decls`] reads every
//!     [`TypeDeclInput`]'s borrowed `edda_types::TypeDeclInfo` and
//!     pushes one [`crate::AdtDef`] per declaration onto the program.
//!     The resulting [`crate::AdtId`]s are stored in `ctx.adt_map`
//!     keyed by [`edda_resolve::BindingId`].
//!   - [`register_function_bodies`] RESERVES each [`FunctionInput`]'s
//!     body slot up front and records its `BindingId -> BodyId`
//!     mapping in `ctx.function_map`. Reserving (rather than
//!     predicting `base + i`) fixes each source body's id before the
//!     walk so a synthetic `push_body` mid-walk does not shift it; the
//!     map is the lookup table the `Call` terminator lowering
//!     consults to resolve a callee path to its `FuncRef::Body(BodyId)`.
//!
//! [`ty::lower_ty`] translates
//! `TyKind::Nominal(BindingId)` into `MirTypeKind::Adt(AdtId)` via the
//! `adt_map` these pre-passes build.
//!
//! Layout resolution, the dataflow-driven sanity checker,
//! `Call` / `Raise` / `?`-propagation lowering, `StructLit` /
//! `Field` / `Index` rvalue lowering, and `Variant` / `Struct` pattern
//! tests round out the pass.
//!
//! # Pattern
//!
//! The lowering surface follows the lowering-seam convention:
//! `edda-types` owns the typed HIR; `edda-mir` consumes it directly
//! through immutable borrows over the lowering pass's lifetime.

mod adt;
mod adt_ops;
mod alloc_family;
mod arith;
mod args;
mod call;
mod capability_method_call;
mod cfg;
mod closure;
mod comptime;
mod ctx;
mod effect;
mod expr;
mod flow;
mod for_loop;
mod fstring_emit;
mod handle;
mod input;
mod layout;
mod overflow;
mod pattern;
mod raise;
mod register;
mod scope;
mod slice;
mod spawn;
mod stmt;
mod ty;
mod variant_ctor;
mod variant_ctor_call;

use edda_span::Span;

use crate::arena::Idx;
use crate::builder::BodyBuilder;
use crate::error::MirError;
use crate::ids::VariantIdx;
use crate::operand::Operand;
use crate::place::Place;
use crate::program::MirProgram;
use crate::rvalue::{Rvalue, RvalueKind};
use crate::ty::{MirPrim, MirType, MirTypeKind};

pub(crate) use ctx::LoweringContext;
pub use input::{
    AllocFmtBindings, ConstInput, ExternInput, FmtBindings, FunctionInput, LoweringInput,
    TypeDeclInput,
};

use self::cfg::{alloc_block, assign_into, push_assign, seal_terminator};
use self::ty::lower_ty;


/// Lower a set of typed-HIR function bodies and user-declared type
/// declarations into an [`MirProgram`].
///
/// Two pre-passes run before any body is walked:
///
/// 1. [`adt::register_type_decls`] lifts every [`TypeDeclInput`] into a
///    [`crate::AdtDef`] and records the resulting `BindingId -> AdtId` in
///    `ctx.adt_map`.
/// 2. [`register_function_bodies`] RESERVES every [`FunctionInput`]'s
///    body slot (capturing its real `BodyId`) and records the
///    `BindingId -> BodyId` in `ctx.function_map`. Reserving up front
///    fixes each source body's id before the walk, so a synthetic
///    `push_body` mid-walk (fn-value shim / closure body) appends a
///    trailing id and never shifts a source id — `lower_function` later
///    FILLS the reserved slot via [`crate::builder::ProgramBuilder::fill_body`].
///
/// The result is validated before return; failures include both lowering
/// errors and structural validation issues.
pub fn lower(input: LoweringInput<'_>) -> Result<MirProgram, Vec<MirError>> {
    let mut ctx = LoweringContext::new(&input);
    adt::register_type_decls(&mut ctx, input.type_decls);
    ctx.alloc_error_adt = input.alloc_error.and_then(|b| ctx.adt_map.get(&b).copied());
    register::register_function_bodies(&mut ctx, input.functions);
    register::register_externs(&mut ctx, input.externs);
    register::register_consts(&mut ctx, input.consts);
    for func in input.functions {
        lower_function(&mut ctx, func);
    }
    // Lowering owns its own errors; validation is the final structural
    // check applied to the finished program. Always run validation so a
    // single failure report combines both lowering errors and structural
    // issues. `ctx.finish()` returns the in-progress program plus the
    // accumulated lowering errors regardless of success.
    let (program, mut errors) = ctx.finish_with_errors();
    for v in crate::validate::validate(&program) {
        errors.push(MirError::from(v));
    }
    if errors.is_empty() {
        Ok(program)
    } else {
        Err(errors)
    }
}


/// Lower one [`FunctionInput`] into a [`crate::Body`] and push it onto the
/// program.
fn lower_function(ctx: &mut LoweringContext<'_>, func: &FunctionInput<'_>) {
    // Look up the pre-synthesized result ADT for this function (if raising).
    let body_id = *ctx
        .function_map
        .get(&func.binding)
        .expect("body_id pre-registered in register_function_bodies");
    // A return-position borrow (`-> let/mutable T`, non-raising) lowers
    // its body return type to `HeapPtr`: the body stores the *address*
    // of a place rooted at a by-reference parameter and the caller reads
    // it back through a `Projection::Deref`, so a `mutable` write threads
    // to the borrowed storage. Raising +
    // borrow is not yet handled — those keep the by-value path.
    let mut borrow_pointee = None;
    let (return_ty, ok_variant) =
        if let Some(&result_adt_id) = ctx.function_result_adts.get(&body_id) {
            (
                MirType::new(MirTypeKind::Adt(result_adt_id)),
                Some((result_adt_id, VariantIdx::new(0))),
            )
        } else {
            let value_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, func.sig.return_ty);
            // A slice `[T]` is already a borrowed fat-pointer view, so a
            // `let` / `mutable` mode on a slice return is redundant and the
            // returned sub-slice (`xs[lo..<hi]`) is a *computed* value with
            // no addressable place to take the address of. Lower it by
            // value — identically to a plain `-> [T]` return — instead of
            // routing through the address-of-place borrow path, which
            // models only sized by-reference returns. Without this guard the
            // borrow path runs `resolve_place` over the sub-slice index and
            // mis-blames the range literal with a "Range (value position)"
            // error.
            let returns_slice_view = matches!(value_ty.kind, MirTypeKind::Slice(_));
            if func.sig.return_mode != edda_types::ReturnMode::ByValue && !returns_slice_view {
                borrow_pointee = Some(value_ty);
                (MirType::prim(MirPrim::HeapPtr), None)
            } else {
                (value_ty, None)
            }
        };
    let mut builder = BodyBuilder::new(func.name, func.span, return_ty.clone());
    let return_slot = builder.return_slot(return_ty, func.span);
    builder.set_export_symbol(func.export_symbol);
    builder.set_abi(func.abi.clone());
    // Thread the driver's deterministic module-qualified mangle onto the
    // body the builder will hand back, so it survives the `fill_body`
    // overwrite of the reserved stub.
    if let Some(qualified_name) = func.qualified_name {
        builder.set_qualified_name(qualified_name);
    }
    ctx.body = Some(builder);
    ctx.return_slot = Some(return_slot);
    ctx.result_adt = ok_variant;
    ctx.return_borrow_pointee = borrow_pointee;

    // Capability params land first per the BodyBuilder ordering invariant.
    let has_panic =
        effect::lower_effect_row(ctx, &func.sig.effects, &func.sig.params, func.span);
    if has_panic && let Some(body_builder) = ctx.body.as_mut() {
        body_builder.set_panic(true);
    }

    lower_user_params(ctx, func);

    // Allocate the entry block before the body lowers anything into it.
    let Some(entry) = alloc_block(ctx) else {
        // No body builder; nothing more to do — error already pushed.
        ctx.reset_body_state();
        return;
    };
    if let Some(body_builder) = ctx.body.as_mut() {
        body_builder.set_entry(entry);
    }
    ctx.current_bb = Some(entry);

    lower_function_body(ctx, func);

    seal_trailing(ctx, func, return_slot);
    finish_body(ctx, body_id);
}

/// Lower the user-declared parameters in declaration order and seed
/// `ctx.bindings` for each one.
fn lower_user_params(ctx: &mut LoweringContext<'_>, func: &FunctionInput<'_>) {
    for param in func.sig.params.iter() {
        let ty = lower_ty(ctx.ty_interner, &ctx.adt_map, param.ty);
        if matches!(ty.kind, MirTypeKind::Capability(_)) {
            // Effect-row capability param: alias the name to the pre-allocated
            // leading capability slot so body references resolve to the
            // existing ptr value. Avoids a duplicate slot for the same
            // capability.
            if let Some(&effect_id) = ctx.capabilities.get(&param.name) {
                if let Some(body) = ctx.body.as_ref() {
                    let slot_local = body
                        .body_ref()
                        .effect_row
                        .capabilities
                        .get(effect_id.as_index())
                        .map(|s| s.param_local);
                    if let Some(local) = slot_local {
                        ctx.bindings.insert(param.name, local);
                    }
                }
                continue;
            }
            // Value-position capability: fall through to the regular body-param
            // path below so the binding gets a normal ABI slot.
        } else if matches!(ty.kind, MirTypeKind::Never) {
            // Genuine Never-typed params (`never`, `Type`, the `Error`
            // cascade sentinel) have no runtime representation. Silently
            // skip — matching the pre-fix behavior for these edge cases.
            continue;
        }
        let mode = register::map_param_mode(param.mode);
        let Some(body_builder) = ctx.body.as_mut() else { return };
        let local = body_builder.param(mode, ty, param.span);
        ctx.bindings.insert(param.name, local);
    }
}

/// Lower the function body: every statement, then assign the trailing
/// expression (if any) into the return slot. For raising functions the
/// trailing value is wrapped in the Ok variant of the synthesized
/// `Result<T, E>` sum ADT; the fall-through case (no trailing, or
/// Never-typed trailing) writes the Ok variant explicitly so the
/// return slot holds a well-formed Ok rather than uninitialised bytes.
fn lower_function_body(ctx: &mut LoweringContext<'_>, func: &FunctionInput<'_>) {
    scope::enter_scope(ctx);
    for stmt in func.body.stmts.iter() {
        stmt::lower_stmt(ctx, stmt);
        if ctx.current_bb.is_none() {
            scope::exit_scope(ctx, func.body.span);
            return;
        }
    }
    let mut wrote_return_slot = false;
    if let Some(trailing) = func.body.trailing.as_deref()
        // Return-position borrow with a *bare-place* tail (`{ o.inner }`,
        // no `return` keyword): store its address into the HeapPtr return
        // slot via `RvalueKind::Ref`; `seal_trailing` (in the caller)
        // emits the `Return`. A `return <place>` tail is a
        // `HirExprKind::Return` instead and routes through the normal
        // `lower_return` borrow path below.
        && ctx.return_borrow_pointee.is_some()
        && flow::is_borrow_place_tail(&trailing.kind)
        && flow::try_lower_borrow_return(ctx, trailing.span, Some(trailing))
    {
        wrote_return_slot = true;
    } else if let Some(trailing) = func.body.trailing.as_deref() {
        let op = expr::lower_expr_to_operand(ctx, trailing);
        if let Some(return_slot) = ctx.return_slot {
            // A `Never`-typed trailing expression (an infinite `loop`
            // with no break, an `if` whose branches all diverge, etc.)
            // never produces a value, so there is nothing to wrap or
            // assign. Emitting a MakeVariant whose payload is a
            // `Never`-typed local would fail at the LLVM emitter,
            // which has no representation for `Never`. The
            // surrounding `seal_trailing` still issues a `Return` from
            // whichever block `current_bb` points at — its
            // statically-unreachable but structurally needed by the
            // MIR's "every block has a terminator" rule.
            let trailing_is_never = matches!(
                ctx.ty_interner.kind(trailing.ty),
                edda_types::TyKind::Primitive(edda_types::Primitive::Never)
            );
            if !trailing_is_never {
                // For raising functions, wrap the success value in Ok.
                let final_op = wrap_ok_trailing(ctx, trailing.span, op);
                let return_ty = match ctx.body.as_ref() {
                    Some(b) => b.body_ref().return_ty.clone(),
                    None => {
                        scope::exit_scope(ctx, func.body.span);
                        return;
                    }
                };
                assign_into(ctx, trailing.span, return_slot, final_op, return_ty);
                wrote_return_slot = true;
            }
        }
    }
    // §C3 fix: when the body falls off the end of a raising function
    // (no trailing expression, or a `Never`-typed trailing whose
    // diverging arm did not actually fire) the return slot is still
    // typed as the synthesized `Result<T, E>` sum but no `Ok` value
    // has been written into it. `seal_trailing` will emit
    // `Return(Move(return_slot))` which lowers to a load of an
    // uninitialised aggregate alloca — the resulting tag byte is
    // undef, which the caller's `?`-propagation tag-check
    // (`tag == 0`) reads as a non-Ok in release builds and routes
    // through the err branch on the success path. Writing the Ok
    // variant explicitly here closes the gap; for non-raising
    // functions and for bodies whose trailing already wrote a value,
    // this is a no-op.
    seal_fallthrough_ok(ctx, func, wrote_return_slot);
    scope::exit_scope(ctx, func.body.span);
}

/// Write the Ok variant of the synthesized `Result<T, E>` into the
/// return slot when the body falls off the end of a raising function
/// without an explicit return. No-op for non-raising functions, bodies
/// that already wrote the return slot via a non-Never trailing
/// expression, or bodies whose current block has been sealed by a
/// diverging terminator.
fn seal_fallthrough_ok(
    ctx: &mut LoweringContext<'_>,
    func: &FunctionInput<'_>,
    wrote_return_slot: bool,
) {
    if wrote_return_slot || ctx.current_bb.is_none() {
        return;
    }
    if ctx.result_adt.is_none() {
        return;
    }
    let Some(return_slot) = ctx.return_slot else {
        return;
    };
    let return_ty = match ctx.body.as_ref() {
        Some(b) => b.body_ref().return_ty.clone(),
        None => return,
    };
    let ok_op = wrap_ok_trailing(ctx, func.span, Operand::Unit);
    assign_into(ctx, func.span, return_slot, ok_op, return_ty);
}

/// Wrap the trailing-expression operand in the Ok variant when the current
/// function is raising. Standalone copy of `flow::wrap_ok_if_raising` so
/// `mod.rs` avoids importing `flow` module internals.
fn wrap_ok_trailing(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    op: Operand,
) -> Operand {
    let Some((result_adt_id, ok_idx)) = ctx.result_adt else {
        return op;
    };
    let ok_has_payload = ctx
        .program
        .program()
        .adts
        .get(result_adt_id)
        .and_then(|adt| adt.variants.get(ok_idx.as_index()))
        .map(|v| !v.fields.is_empty())
        .unwrap_or(true);
    let fields = if ok_has_payload { vec![op] } else { Vec::new() };
    let ok_ty = MirType::new(MirTypeKind::Adt(result_adt_id));
    let ok_rvalue = Rvalue {
        span,
        kind: RvalueKind::MakeVariant {
            adt: result_adt_id,
            variant: ok_idx,
            fields,
        },
        ty: ok_ty.clone(),
    };
    let tmp = match ctx.body.as_mut() {
        Some(b) => b.temp(ok_ty, span),
        None => return Operand::Unit,
    };
    push_assign(ctx, span, tmp, ok_rvalue);
    Operand::Move(Place::local(tmp))
}

/// If the current block is still active when the body ends, seal it with
/// `Return(Move(return_slot))`. Bodies whose trailing expression diverged
/// (e.g. ended in `return` / `panic`) leave `current_bb` already `None`.
fn seal_trailing(
    ctx: &mut LoweringContext<'_>,
    func: &FunctionInput<'_>,
    return_slot: crate::ids::LocalId,
) {
    if ctx.current_bb.is_some() {
        seal_terminator(
            ctx,
            crate::terminator::Terminator {
                span: func.span,
                kind: crate::terminator::TerminatorKind::Return(Operand::Move(
                    Place::local(return_slot),
                )),
            },
        );
        // `seal_terminator` clears current_bb on success.
    }
}

/// Fill the function's reserved body slot with the finished body and
/// reset per-body state. `body_id` is the slot reserved up front by
/// `register::register_function_bodies`.
fn finish_body(ctx: &mut LoweringContext<'_>, body_id: crate::ids::BodyId) {
    let Some(builder) = ctx.body.take() else {
        ctx.reset_body_state();
        return;
    };
    let body = builder.finish();
    ctx.program.fill_body(body_id, body);
    ctx.reset_body_state();
}


