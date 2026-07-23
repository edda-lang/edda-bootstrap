//! Expression dispatch for the typed-HIR -> MIR lowering pass.
//!
//! Every `HirExprKind` enters [`lower_expr_to_operand`], which either returns
//! an operand naming the produced value directly (literals, binary ops,
//! locals) or threads control flow through fresh basic blocks and returns the
//! operand naming the join's result local (`if` / `match` / `loop`).
//!
//! Diverging variants (`Return`, `Break`, `Continue`, `Panic`) seal the
//! current block with the matching terminator, clear `current_bb`, and return
//! `Operand::Unit` so the caller has a placeholder when the value is never
//! used. Control-flow lowering lives in [`super::flow`]; this file focuses on
//! the value-producing forms. Path-expression lowering lives in [`path`];
//! primitive-cast lowering lives in [`cast`].

mod cast;
mod path;

use edda_span::Span;
use edda_syntax::ast::{Literal, ScopeKind};
use edda_types::{HirBlock, HirExpr, HirExprKind, IntrinsicKind};

use crate::constant::{Const, ConstValue};
use crate::error::{LoweringError, MirError};
use crate::operand::Operand;
use crate::place::Place;
use crate::rvalue::{Rvalue, RvalueKind};

use super::cfg::push_assign;
use super::ctx::LoweringContext;
use super::stmt::lower_stmt;
use super::ty::lower_ty;

/// Lower an expression and return an [`Operand`] naming its value.
pub(super) fn lower_expr_to_operand(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
) -> Operand {
    match &expr.kind {
        HirExprKind::Literal(lit) => lower_literal(ctx, expr, lit),
        HirExprKind::FString(parts) => super::fstring_emit::lower_fstring(ctx, expr, parts),
        HirExprKind::Path(path) => path::lower_path(ctx, expr, path),
        HirExprKind::Binary { op, lhs, rhs } => {
            super::arith::lower_binary(ctx, expr, *op, lhs, rhs)
        }
        HirExprKind::Unary { op, expr: inner } => {
            super::arith::lower_unary(ctx, expr, *op, inner)
        }
        HirExprKind::If { cond, then_block, else_branch } => super::flow::lower_if(
            ctx,
            expr,
            cond,
            then_block,
            else_branch.as_deref(),
        ),
        HirExprKind::Match { scrutinee, arms } => super::flow::lower_match(ctx, expr, scrutinee, arms),
        HirExprKind::Block(block) => lower_block_expr(ctx, block),
        HirExprKind::Cast { expr: inner, target_ty, mode } => {
            cast::lower_cast(ctx, expr, inner, *target_ty, *mode)
        }
        HirExprKind::Tuple(elems) => lower_tuple(ctx, expr, elems),
        HirExprKind::Array(elems) => lower_array(ctx, expr, elems),
        HirExprKind::Loop { body, .. } => super::flow::lower_loop(ctx, expr, body),
        HirExprKind::Return(value) => {
            super::flow::lower_return(ctx, expr.span, value.as_deref());
            Operand::Unit
        }
        HirExprKind::Break { value, .. } => {
            super::flow::lower_break(ctx, expr.span, value.as_deref());
            Operand::Unit
        }
        HirExprKind::Continue { .. } => {
            super::flow::lower_continue(ctx, expr.span);
            Operand::Unit
        }
        HirExprKind::Panic(msg) => {
            super::flow::lower_panic(ctx, expr.span, msg);
            Operand::Unit
        }
        HirExprKind::Error => Operand::Unit,
        HirExprKind::Raise(inner) => super::raise::lower_raise(ctx, expr, inner),
        HirExprKind::Try(inner) => super::raise::lower_try(ctx, expr, inner),
        HirExprKind::Call { callee, args } => {
            // `discriminant_of(v)` is the lone call-shaped (non-method)
            // compiler intrinsic — recorded by the typechecker keyed by
            // the call span; it lowers to a tag-read rvalue, never a
            // `Call` terminator (D-22 READ).
            if let Some(IntrinsicKind::DiscriminantOf) = ctx.intrinsic_calls.get(&expr.span) {
                return lower_discriminant_of(ctx, expr, args);
            }
            super::call::lower_call(ctx, expr, callee, args, None)
        }
        HirExprKind::MethodCall { receiver, name, args } => {
            // Check if this is a compiler intrinsic before free-function dispatch.
            if let Some(&intrinsic) = ctx.intrinsic_calls.get(&expr.span) {
                return lower_intrinsic_method(ctx, expr, receiver, intrinsic);
            }
            super::call::lower_method_call(ctx, expr, receiver, *name, args)
        }
        HirExprKind::Field { receiver, name } => {
            super::adt_ops::lower_field(ctx, expr, receiver, *name)
        }
        HirExprKind::TupleIndex { receiver, index } => {
            super::adt_ops::lower_tuple_index(ctx, expr, receiver, *index)
        }
        HirExprKind::Index { receiver, index } => {
            super::slice::lower_index(ctx, expr, receiver, index)
        }
        // Free-standing Range expressions (`let r = 0..<10`) need the
        // implicit `Range_<T>` spec instantiation in
        // `edda-codegen`; until that lands we only admit `Range` as a
        // slice index (handled inside `lower_index`).
        HirExprKind::Range { .. } => unsupported_and_unit(ctx, expr.span, "Range (value position)"),
        HirExprKind::StructLit { path: _, fields } => {
            super::adt_ops::lower_struct_lit(ctx, expr, fields)
        }
        HirExprKind::For { pat, iter, body, .. } => {
            super::for_loop::lower_for(ctx, expr, pat, iter, body)
        }
        HirExprKind::Await(inner) => super::spawn::lower_await(ctx, expr, inner),
        HirExprKind::Comptime(inner) => super::comptime::lower_comptime(ctx, expr, inner),
        HirExprKind::ComptimeBlock(block) => super::comptime::lower_comptime_block(ctx, expr, block),
        HirExprKind::Scope { kind, name, body } => lower_scope(ctx, expr.span, *kind, *name, body),
        HirExprKind::EffectRow(_) => unsupported_and_unit(ctx, expr.span, "EffectRow"),
        HirExprKind::Handle { handled_ty, binder, recovery, body, .. } => {
            super::handle::lower_handle(ctx, expr, *handled_ty, *binder, recovery, body)
        }
        // Bounded quantifiers are refinement-only forms — the typechecker
        // admits them only inside `where` / `requires` / `ensures` clauses,
        // which never reach MIR lowering. Reaching this arm means a
        // quantifier slipped into expression position; the typechecker
        // should have diagnosed it earlier, but we surface a clean error
        // here as defence-in-depth rather than panicking on the unhandled
        // variant.
        HirExprKind::Forall { .. } => unsupported_and_unit(ctx, expr.span, "forall (admitted only in refinement positions)"),
        HirExprKind::Exists { .. } => unsupported_and_unit(ctx, expr.span, "exists (admitted only in refinement positions)"),
        // Closure literals lower to a synthesised body + `FunctionRef` /
        // `MakeClosure`. A capability-bearing
        // or panicking closure threads its capability slots and forwards
        // them at the call site; only a raising (`err:`) closure keeps
        // emitting a precise `UnsupportedHirVariant` from inside
        // `lower_closure` (the Result-shaped return projection on fn-ptr
        // types is owed before raising fn values round-trip).
        HirExprKind::Closure(closure) => super::closure::lower_closure(ctx, expr, closure),
        HirExprKind::Spawn(spawn) => super::spawn::lower_spawn(ctx, expr, spawn),
    }
}

/// Lower a primitive literal into a [`Const`]-backed operand.
fn lower_literal(ctx: &mut LoweringContext<'_>, expr: &HirExpr, lit: &Literal) -> Operand {
    if matches!(lit, Literal::Unit) {
        return Operand::Unit;
    }
    let ty = lower_ty(ctx.ty_interner, &ctx.adt_map, expr.ty);
    let value = match lit {
        Literal::Int { value, .. } => ConstValue::Int(*value as i128),
        Literal::Bool(b) => ConstValue::Bool(*b),
        Literal::Str(sym) => ConstValue::Str(*sym),
        Literal::Float(sym) => {
            let raw = ctx.interner.resolve(*sym);
            match raw.parse::<f64>() {
                Ok(parsed) => ConstValue::Float(parsed.to_bits()),
                Err(_) => return unsupported_and_unit(ctx, expr.span, "Float (unparseable)"),
            }
        }
        Literal::Unit => unreachable!("Unit handled above"),
    };
    let id = ctx.program.push_const(Const { ty, value });
    Operand::Const(id)
}

/// Lower a block expression — every statement, then the trailing
/// expression (or `Unit`) for the block's value.
pub(super) fn lower_block_expr(
    ctx: &mut LoweringContext<'_>,
    block: &HirBlock,
) -> Operand {
    super::scope::enter_scope(ctx);
    for stmt in block.stmts.iter() {
        lower_stmt(ctx, stmt);
        if ctx.current_bb.is_none() {
            super::scope::exit_scope(ctx, block.span);
            return Operand::Unit;
        }
    }
    let result = match block.trailing.as_deref() {
        Some(trailing) => {
            let raw = lower_expr_to_operand(ctx, trailing);
            // The block's scope-exit is about to emit `StorageDead` for
            // every user-let local declared inside this block. If the
            // trailing expression's operand names one of those locals
            // (e.g. `{ let x = 1; x }`), the caller would read the local
            // *after* its `StorageDead`. Copy through a fresh temp so the
            // returned operand stays valid past scope exit.
            stabilise_block_value(ctx, trailing.span, raw)
        }
        None => Operand::Unit,
    };
    super::scope::exit_scope(ctx, block.span);
    result
}

/// Copy the trailing operand of a block expression through a fresh temp so
/// the caller receives an operand whose validity is independent of the
/// block's about-to-die locals. Constants and the unit operand pass
/// through unchanged.
fn stabilise_block_value(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    op: Operand,
) -> Operand {
    let place = match &op {
        Operand::Copy(p) | Operand::Move(p) => p.clone(),
        Operand::Const(_) | Operand::Unit => return op,
    };
    let ty = match ctx
        .body
        .as_ref()
        .and_then(|b| b.body_ref().locals.get(place.local))
    {
        Some(decl) => decl.ty.clone(),
        None => return op,
    };
    let temp = match ctx.body.as_mut() {
        Some(body) => body.temp(ty.clone(), span),
        None => return op,
    };
    super::cfg::assign_into(ctx, span, temp, op, ty);
    Operand::Move(Place::local(temp))
}

/// Lower a tuple constructor `(e1, e2, ...)` to `Rvalue::MakeTuple` into a
/// fresh temp.
fn lower_tuple(ctx: &mut LoweringContext<'_>, expr: &HirExpr, elems: &[HirExpr]) -> Operand {
    let elem_ops: Vec<Operand> = elems.iter().map(|e| lower_expr_to_operand(ctx, e)).collect();
    let result_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, expr.ty);
    let temp = match ctx.body.as_mut() {
        Some(body) => body.temp(result_ty.clone(), expr.span),
        None => return Operand::Unit,
    };
    let rvalue = Rvalue {
        span: expr.span,
        kind: RvalueKind::MakeTuple { elems: elem_ops },
        ty: result_ty,
    };
    push_assign(ctx, expr.span, temp, rvalue);
    Operand::Move(Place::local(temp))
}

/// Lower an array / slice literal `[e1, ..., en]` (including `[]`) to
/// `Rvalue::MakeArray` into a fresh temp.
fn lower_array(ctx: &mut LoweringContext<'_>, expr: &HirExpr, elems: &[HirExpr]) -> Operand {
    let elem_ops: Vec<Operand> = elems.iter().map(|e| lower_expr_to_operand(ctx, e)).collect();
    let result_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, expr.ty);
    let temp = match ctx.body.as_mut() {
        Some(body) => body.temp(result_ty.clone(), expr.span),
        None => return Operand::Unit,
    };
    let rvalue = Rvalue {
        span: expr.span,
        kind: RvalueKind::MakeArray { elems: elem_ops },
        ty: result_ty,
    };
    push_assign(ctx, expr.span, temp, rvalue);
    Operand::Move(Place::local(temp))
}


/// Lower a `scope(<kind>) name? { body }` expression.
///
/// Per `corpus/edda-codex/language/05-concurrency-coherence.md`, two
/// scope kinds exist: `coherence` (observational atomicity) and `exec`
/// (structured concurrency). `coherence` is admitted as a body-unwrap so
/// projects that already write `scope(coherence) region { ... }` build
/// cleanly and gain the locked semantics retroactively when the full
/// implementation lands.
fn lower_scope(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    kind: ScopeKind,
    name: Option<edda_syntax::ast::Ident>,
    body: &HirBlock,
) -> Operand {
    match kind {
        ScopeKind::Coherence => lower_block_expr(ctx, body),
        ScopeKind::Exec => lower_exec_scope(ctx, span, name, body),
    }
}

/// Lower `scope(exec) name { body }` — open the task group, bind `name`,
/// lower `body`, then join the group before the scope's value.
fn lower_exec_scope(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    name: Option<edda_syntax::ast::Ident>,
    body: &HirBlock,
) -> Operand {
    let Some(name) = name else {
        return unsupported_and_unit(ctx, span, "Scope(exec) (binder-free legacy form)");
    };
    let Some(group_local) = super::spawn::open_task_group(ctx, span) else {
        return Operand::Unit;
    };
    ctx.bindings.insert(name.name, group_local);
    let result = lower_block_expr(ctx, body);
    if ctx.current_bb.is_some() {
        super::spawn::close_task_group(ctx, span, group_local);
    }
    result
}


/// Lower a compiler-intrinsic method call to an rvalue assignment.
///
/// `StringBytes` lowers to `RvalueKind::StringBytes(recv_op)`.
/// `SliceLen` lowers to `RvalueKind::ArrayLen { array: recv_op }`.
/// Both variants produce a temp and return `Move(temp)`.
fn lower_intrinsic_method(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    receiver: &HirExpr,
    intrinsic: IntrinsicKind,
) -> Operand {
    let recv_op = lower_expr_to_operand(ctx, receiver);
    if ctx.current_bb.is_none() {
        return Operand::Unit;
    }
    let result_ty = super::ty::lower_ty(ctx.ty_interner, &ctx.adt_map, expr.ty);
    let rvalue_kind = match intrinsic {
        IntrinsicKind::StringBytes => RvalueKind::StringBytes(recv_op),
        IntrinsicKind::SliceLen => RvalueKind::ArrayLen { array: recv_op },
        // Call-shaped, never method-shaped — dispatched by the
        // `HirExprKind::Call` arm via `lower_discriminant_of`.
        IntrinsicKind::DiscriminantOf => {
            return unsupported_and_unit(ctx, expr.span, "discriminant_of (method form)");
        }
    };
    let rvalue = Rvalue { span: expr.span, kind: rvalue_kind, ty: result_ty.clone() };
    let Some(body_builder) = ctx.body.as_mut() else {
        return Operand::Unit;
    };
    let tmp = body_builder.temp(result_ty, expr.span);
    push_assign(ctx, expr.span, tmp, rvalue);
    Operand::Move(Place::local(tmp))
}

/// Lower a `discriminant_of(v)` intrinsic call — the active variant's
/// declaration-order index of a sum-typed value, typed `usize` (D-22
/// READ surface; bootstrap parity). Reads the subject's tag via
/// [`RvalueKind::ExtractTag`], then widens the tag-width integer to
/// `usize` via [`RvalueKind::Cast`] — mirroring the native's
/// `Rvalue.discriminant` + wrapping `int_to_int` cast lowering.
fn lower_discriminant_of(
    ctx: &mut LoweringContext<'_>,
    expr: &HirExpr,
    args: &[edda_types::HirCallArg],
) -> Operand {
    let Some(arg) = args.first() else {
        return unsupported_and_unit(ctx, expr.span, "discriminant_of (missing argument)");
    };
    let subject = lower_expr_to_operand(ctx, &arg.expr);
    if ctx.current_bb.is_none() {
        return Operand::Unit;
    }
    let Some((adt_id, _kind)) = super::adt_ops::resolve_nominal_adt(ctx, arg.expr.ty, expr.span)
    else {
        return Operand::Unit;
    };
    let tag_prim = match ctx.program.program().adts.get(adt_id).and_then(|adt| adt.tag_width) {
        Some(p) => p,
        None => {
            ctx.errors.push(MirError::from(LoweringError::InternalError {
                message: "discriminant_of on an ADT with no discriminant tag".to_string(),
                span: expr.span,
            }));
            return Operand::Unit;
        }
    };
    let tag_ty = crate::ty::MirType::prim(tag_prim);
    let Some(body_builder) = ctx.body.as_mut() else {
        return Operand::Unit;
    };
    let tag_tmp = body_builder.temp(tag_ty.clone(), expr.span);
    push_assign(
        ctx,
        expr.span,
        tag_tmp,
        Rvalue { span: expr.span, kind: RvalueKind::ExtractTag { subject }, ty: tag_ty },
    );
    let result_ty = lower_ty(ctx.ty_interner, &ctx.adt_map, expr.ty);
    let Some(body_builder) = ctx.body.as_mut() else {
        return Operand::Unit;
    };
    let out_tmp = body_builder.temp(result_ty.clone(), expr.span);
    push_assign(
        ctx,
        expr.span,
        out_tmp,
        Rvalue {
            span: expr.span,
            kind: RvalueKind::Cast {
                src: Operand::Move(Place::local(tag_tmp)),
                src_prim: tag_prim,
                dst_prim: crate::ty::MirPrim::Usize,
            },
            ty: result_ty,
        },
    );
    Operand::Move(Place::local(out_tmp))
}

/// Push `UnsupportedHirVariant { variant }` at `span` and yield
/// `Operand::Unit` so callers have a placeholder.
pub(super) fn unsupported_and_unit(
    ctx: &mut LoweringContext<'_>,
    span: Span,
    variant: &'static str,
) -> Operand {
    ctx.errors.push(MirError::from(LoweringError::UnsupportedHirVariant {
        variant,
        span,
    }));
    Operand::Unit
}
