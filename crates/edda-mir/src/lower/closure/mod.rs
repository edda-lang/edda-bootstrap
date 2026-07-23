//! Closure-literal MIR lowering under the unified fat function-value
//! ABI.
//!
//! A `function(p: T) -> U with {} captures {...} { body }` literal
//! type-checks to a [`edda_types::TyKind::FnPtr`] (see
//! `edda_types::infer::closure`). Under the unified ABI every
//! `function(...)` value lowers to the fat pair `{ code: ptr, env:
//! usize }` (see `edda-compile`'s `fn_value_struct_type`), so closures
//! and named-function values are interchangeable wherever a fn-typed
//! value is expected.
//!
//! This pass synthesises a fresh MIR [`crate::Body`] whose *leading*
//! parameter is the env word, then emits:
//! - [`crate::RvalueKind::FunctionRef`] for the env-less case
//!   (captureless closures) — the codegen wraps it as `{ code, 0 }`;
//! - [`crate::RvalueKind::MakeClosure`] for a capturing closure — the
//!   env word carries one inline word-sized capture by value.
//!
//! # Scope
//!
//! Captureless and single-word-`let`-capture, effect-free closures.
//! The env word is pointer-width and holds one word-sized **integer-
//! class** primitive (`i8..i64`/`u8..u64`/`bool`/`char`/`usize`/
//! `isize`/`HeapPtr`) recovered in the body via [`RvalueKind::Cast`].
//! A non-empty effect row, a `take` capture, more than one capture, or
//! a non-word/float/aggregate capture keeps emitting a precise
//! `UnsupportedHirVariant` so the build fails cleanly. These richer
//! shapes need a pointer-backed environment (allocator + MIR
//! read-through-pointer), tracked as a follow-up.
//!
//! # Re-entrancy
//!
//! The enclosing function is mid-lowering when a closure literal is
//! reached, so [`build_closure_body`] runs against a *saved-then-
//! restored* copy of the per-body [`LoweringContext`] fields (the set
//! [`LoweringContext::reset_body_state`] clears). The synthesised body
//! is pushed onto the program-wide `ProgramBuilder`, which is untouched
//! by the swap. The env-packing `Cast` is emitted into the *enclosing*
//! body before the swap; only the closure body's scratch state is
//! built against the saved-out state.

mod body;

use edda_intern::Symbol;
use edda_syntax::ast::CaptureMode;
use edda_types::{EffectEntry, EffectRow, HirClosure, HirExpr, PureEffect};

use super::cfg::push_assign;
use super::ctx::LoweringContext;
use super::ty::lower_ty;
use crate::ids::AdtId;
use crate::operand::Operand;
use crate::place::Place;
use crate::rvalue::{Rvalue, RvalueKind};
use crate::ty::{MirPrim, MirType, MirTypeKind};

/// Lower a `HirExprKind::Closure` literal to a fat function value.
pub(super) fn lower_closure(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    closure: &HirClosure,
) -> Operand {
    let mut fn_ptr_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, expr.ty);
    debug_assert!(
        matches!(fn_ptr_ty.kind, MirTypeKind::FnPtr(_)),
        "lower_closure called with non-FnPtr HIR type {:?}",
        expr.ty,
    );
    // Raising closures: synthesise the
    // body's wire-level `Result<T, E>` ADT now (so the body builds against
    // it and Ok-wraps), and project the fat-value `FnPtr` ret to match. A
    // closure body is synthesised on the fly, so — unlike a named function
    // — it has no `ctx.function_result_adts` entry; the ADT is minted here
    // via `synthesize_result_adt`, the same machinery `register.rs` uses.
    // `None` means the err payloads did not resolve to ADTs (upstream
    // cascade); the body then lowers `T`-shaped and the indirect call site
    // surfaces the mismatch rather than this layer double-reporting.
    let result_adt = if row_raises(&closure.effects) {
        let adt = closure_result_adt(ctx, closure);
        if let Some(result_adt_id) = adt {
            fn_ptr_ty = project_fn_ptr_result_ret(result_adt_id, fn_ptr_ty);
        }
        adt
    } else {
        None
    };
    match closure.captures.len() {
        0 => {
            let Some(body_id) = body::synthesise_closure_body(ctx, closure, None, result_adt) else {
                return Operand::Unit;
            };
            emit_fn_value(ctx, expr.span, fn_ptr_ty, RvalueKind::FunctionRef(body_id))
        }
        1 => lower_capturing_closure(ctx, expr, closure, fn_ptr_ty, result_adt),
        _ => super::expr::unsupported_and_unit(
            ctx,
            expr.span,
            "closure capturing more than one binding (multi-field env)",
        ),
    }
}

/// Whether a closure's effect row raises (`err:`).
fn row_raises(row: &EffectRow) -> bool {
    row.entries()
        .iter()
        .any(|entry| matches!(entry, EffectEntry::Pure(PureEffect::Err(_))))
}

/// Synthesise the wire-level `Result<T, E1, ...>` sum ADT for a raising
/// closure body, mirroring [`super::register::register_function_bodies`].
/// Returns `None` when an err payload does not resolve to an ADT.
fn closure_result_adt(ctx: &mut LoweringContext<'_>, closure: &HirClosure) -> Option<AdtId> {
    let err_tys: Vec<_> = closure
        .effects
        .entries()
        .iter()
        .filter_map(|entry| match entry {
            EffectEntry::Pure(PureEffect::Err(t)) => Some(*t),
            _ => None,
        })
        .collect();
    let success_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, closure.ret_ty);
    let mut err_adts: Vec<(Symbol, AdtId)> = Vec::with_capacity(err_tys.len());
    for err_ty in &err_tys {
        match lower_ty(ctx.ty_interner, &ctx.adt_map, *err_ty).kind {
            MirTypeKind::Adt(err_adt_id) => {
                let err_name = match ctx.program.program().adts.get(err_adt_id) {
                    Some(def) => def.name,
                    None => ctx.interner.intern("__Err"),
                };
                err_adts.push((err_name, err_adt_id));
            }
            _ => return None,
        }
    }
    if err_adts.is_empty() {
        return None;
    }
    Some(super::adt::synthesize_result_adt(ctx, success_ty, err_adts, closure.span))
}

/// Project a raising closure's fat-value `FnPtr` ret onto its body's
/// wire-level `Result<T, E>` ADT. The named-fn-value path does the
/// equivalent in `super::expr::overwrite_raising_fn_ptr_ret`.
fn project_fn_ptr_result_ret(result_adt_id: AdtId, fn_ptr_ty: MirType) -> MirType {
    match fn_ptr_ty.kind {
        MirTypeKind::FnPtr(mut sig) => {
            sig.ret = MirType::new(MirTypeKind::Adt(result_adt_id));
            MirType::new(MirTypeKind::FnPtr(sig))
        }
        other => MirType { kind: other, layout: fn_ptr_ty.layout },
    }
}

/// Lower a single-`let`-capture closure: pack the captured value into
/// the env word in the *enclosing* body, synthesise the body that
/// unpacks it, and emit `MakeClosure`.
fn lower_capturing_closure(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    closure: &HirClosure,
    fn_ptr_ty: MirType,
    result_adt: Option<AdtId>,
) -> Operand {
    let cap = closure.captures[0];
    if !matches!(cap.mode, CaptureMode::Let) {
        return super::expr::unsupported_and_unit(
            ctx,
            cap.span,
            "closure with a `take` capture (ownership transfer)",
        );
    }
    let Some(&enclosing_local) = ctx.bindings.get(&cap.name.name) else {
        return super::expr::unsupported_and_unit(
            ctx,
            cap.span,
            "closure capture does not resolve to a local binding",
        );
    };
    let cap_ty = match ctx
        .body
        .as_ref()
        .and_then(|b| b.body_ref().locals.get(enclosing_local))
    {
        Some(decl) => decl.ty.clone(),
        None => return Operand::Unit,
    };
    let MirTypeKind::Prim(cap_prim) = cap_ty.kind else {
        return super::expr::unsupported_and_unit(
            ctx,
            cap.span,
            "closure capture is not a word-sized primitive (env-by-pointer)",
        );
    };
    if !is_packable_prim(cap_prim, ctx.pointer_width_bytes) {
        return super::expr::unsupported_and_unit(
            ctx,
            cap.span,
            "closure capture is not a word-sized integer-class primitive",
        );
    }

    // Pack the captured value into the pointer-width env word in the
    // enclosing body (a width-changing integer `Cast` is bit-preserving
    // for the integer-class prims accepted above).
    let usize_ty = MirType::prim(MirPrim::Usize);
    let env_temp = match ctx.body.as_mut() {
        Some(b) => b.temp(usize_ty.clone(), expr.span),
        None => return Operand::Unit,
    };
    push_assign(
        ctx,
        expr.span,
        env_temp,
        Rvalue {
            span: expr.span,
            kind: RvalueKind::Cast {
                src: Operand::Copy(Place::local(enclosing_local)),
                src_prim: cap_prim,
                dst_prim: MirPrim::Usize,
            },
            ty: usize_ty,
        },
    );

    let Some(body_id) =
        body::synthesise_closure_body(ctx, closure, Some((cap.name.name, cap_ty, cap_prim)), result_adt)
    else {
        return Operand::Unit;
    };
    emit_fn_value(
        ctx,
        expr.span,
        fn_ptr_ty,
        RvalueKind::MakeClosure {
            code: body_id,
            env: Operand::Move(Place::local(env_temp)),
        },
    )
}

/// Emit a fat-fn-value rvalue into a fresh temp in the enclosing body
/// and return a `Move` of it.
fn emit_fn_value(
    ctx: &mut LoweringContext<'_>,
    span: edda_span::Span,
    fn_ptr_ty: MirType,
    kind: RvalueKind,
) -> Operand {
    let Some(builder) = ctx.body.as_mut() else {
        return Operand::Unit;
    };
    let dest = builder.temp(fn_ptr_ty.clone(), span);
    push_assign(ctx, span, dest, Rvalue { span, kind, ty: fn_ptr_ty });
    Operand::Move(Place::local(dest))
}

/// Whether a primitive fits the pointer-width env word and round-trips
/// through an integer `Cast`.
fn is_packable_prim(prim: MirPrim, pointer_width_bytes: u32) -> bool {
    match prim {
        MirPrim::I8
        | MirPrim::I16
        | MirPrim::I32
        | MirPrim::U8
        | MirPrim::U16
        | MirPrim::U32
        | MirPrim::Bool
        | MirPrim::Char
        | MirPrim::Usize
        | MirPrim::Isize
        | MirPrim::HeapPtr => true,
        MirPrim::I64 | MirPrim::U64 => pointer_width_bytes >= 8,
        _ => false,
    }
}
