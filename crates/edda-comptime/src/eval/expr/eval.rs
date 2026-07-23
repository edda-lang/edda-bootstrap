//! Per-kind reduction helpers for the comptime HIR expression evaluator.

use edda_span::Span;
use edda_syntax::ast::{AssignOp, BinOp, BindingMode, Ident, Literal, RangeKind, UnOp};
use edda_types::{
    HirBlock, HirCallArg, HirExpr, HirExprKind, HirPat, HirPatKind, HirPath, HirStmt, HirStmtKind,
    Primitive, TyId, TyKind,
};

use edda_types::{EffectEntry, PureEffect};

use crate::builtin::builtin_for_name;
use crate::eval::eval_builtin_with_decls;
use crate::eval::expr::{EvalCx, eval_expr};
use crate::eval::expr::diag::{push_not_supported, push_op_error, push_panic, ty_display};
use crate::eval::op::{OpError, apply_binary, apply_unary};
use crate::fndecl::FnDeclInfo;
use crate::layout::TypeDeclLookup;
use crate::value::{FloatValue, IntValue, Value};

pub(super) fn eval_literal(
    lit: &Literal,
    ty: TyId,
    span: Span,
    cx: &mut EvalCx<'_>,
) -> Option<Value> {
    match *lit {
        Literal::Unit => Some(Value::Unit),
        Literal::Bool(b) => Some(Value::Bool(b)),
        Literal::Int { value, .. } => match cx.ty_interner.kind(ty) {
            TyKind::Primitive(p) if p.is_signed_integer() => {
                Some(Value::Int(IntValue::new_signed(*p, value as i128)))
            }
            TyKind::Primitive(p) if p.is_unsigned_integer() => {
                Some(Value::Int(IntValue::new_unsigned(*p, value)))
            }
            other => {
                push_panic(
                    cx.diags,
                    span,
                    format!(
                        "integer literal cannot have non-integer type `{}`",
                        ty_display(other, cx.ty_interner, ty)
                    ),
                );
                None
            }
        },
        Literal::Str(sym) => Some(Value::Str(sym)),
        // The payload is the raw source text (spelling-preserving);
        // parse it at the width inference stamped on the expression —
        // the same raw-text parse the MIR literal lowering performs.
        Literal::Float(sym) => {
            let raw = cx.interner.resolve(sym);
            match cx.ty_interner.kind(ty) {
                TyKind::Primitive(Primitive::F32) => match raw.parse::<f32>() {
                    Ok(v) => Some(Value::Float(FloatValue::F32(v))),
                    Err(_) => {
                        push_panic(cx.diags, span, format!("unparseable float literal `{raw}`"));
                        None
                    }
                },
                TyKind::Primitive(Primitive::F64) => match raw.parse::<f64>() {
                    Ok(v) => Some(Value::Float(FloatValue::F64(v))),
                    Err(_) => {
                        push_panic(cx.diags, span, format!("unparseable float literal `{raw}`"));
                        None
                    }
                },
                other => {
                    push_panic(
                        cx.diags,
                        span,
                        format!(
                            "float literal cannot have non-float type `{}`",
                            ty_display(other, cx.ty_interner, ty)
                        ),
                    );
                    None
                }
            }
        }
    }
}

pub(super) fn eval_path(
    path: &HirPath,
    expr_ty: TyId,
    span: Span,
    cx: &mut EvalCx<'_>,
) -> Option<Value> {
    // §C10 — typechecker-side resolution map. When the typechecker
    // recorded this path as a type-as-value (primitive catalogue head
    // OR user nominal), the entry holds the concrete `TyId`. Consult
    // it before walking the name catalogue so user-type names like
    // `InodeRecord` resolve through their `BindingId`, not by-name.
    if let Some(map) = cx.comptime_type_paths
        && let Some(ty) = map.get(&path.span)
    {
        return Some(Value::Type(*ty));
    }

    // Single-segment paths resolve against the binding environment
    // first, then the primitive catalogue when the expression's type
    // is the `Type` meta-type. Qualified paths already resolved where
    // they can: a qualified *type* name hits the §C10 map above and a
    // qualified *callee* is dispatched inside `eval_call`
    // — so a qualified path reaching this point is a value-position
    // reference the comptime surface has no carrier for.
    if path.segments.len() != 1 {
        push_not_supported(
            cx.diags,
            span,
            "module-qualified path in comptime value position (only type names and callees resolve qualified)",
        );
        return None;
    }
    let segment = &path.segments[0];

    // Local `let` / `var` binding declared earlier in this comptime
    // body — the innermost live binding under this name wins. The
    // scan floor is the innermost user-function frame's base, so a
    // callee body never resolves a caller-local of the same name.
    if let Some(v) = cx.env.lookup_from(cx.frame_base, segment.name) {
        return Some(v.clone());
    }

    let name = cx.interner.resolve(segment.name);

    // Path-as-type: caller expects a Type-valued result (e.g. `i32`
    // in `size_of(i32)`). The expr's type is the `Type` meta-type.
    let is_meta = matches!(
        cx.ty_interner.kind(expr_ty),
        TyKind::Primitive(Primitive::Type)
    );
    if is_meta {
        if let Some(p) = Primitive::from_name(name) {
            return Some(Value::Type(cx.ty_interner.prim(p)));
        }
        push_panic(
            cx.diags,
            span,
            format!("unresolved type name `{name}` in comptime"),
        );
        return None;
    }

    push_panic(
        cx.diags,
        span,
        format!("unresolved identifier `{name}` in comptime"),
    );
    None
}

pub(super) fn eval_binary(
    op: BinOp,
    lhs: &HirExpr,
    rhs: &HirExpr,
    span: Span,
    cx: &mut EvalCx<'_>,
) -> Option<Value> {
    // Short-circuit on `&&` and `||` if the LHS already decides the
    // result — matches surface semantics from `expressions.md`.
    if matches!(op, BinOp::And | BinOp::Or) {
        let op_str = if matches!(op, BinOp::And) { "&&" } else { "||" };
        let lhs_v = eval_expr(lhs, cx)?;
        let Value::Bool(lb) = lhs_v else {
            push_op_error(
                cx.diags,
                span,
                OpError::KindMismatch {
                    op: op_str,
                    operands: (lhs_v.kind().name().to_string(), String::new()),
                },
            );
            return None;
        };
        if matches!(op, BinOp::And) && !lb {
            return Some(Value::Bool(false));
        }
        if matches!(op, BinOp::Or) && lb {
            return Some(Value::Bool(true));
        }
        let rhs_v = eval_expr(rhs, cx)?;
        let Value::Bool(rb) = rhs_v else {
            push_op_error(
                cx.diags,
                span,
                OpError::KindMismatch {
                    op: op_str,
                    operands: ("bool".to_string(), rhs_v.kind().name().to_string()),
                },
            );
            return None;
        };
        return Some(Value::Bool(rb));
    }

    let lhs_v = eval_expr(lhs, cx)?;
    let rhs_v = eval_expr(rhs, cx)?;
    match apply_binary(op, &lhs_v, &rhs_v) {
        Ok(v) => Some(v),
        Err(err) => {
            push_op_error(cx.diags, span, err);
            None
        }
    }
}

pub(super) fn eval_unary(
    op: UnOp,
    operand: &HirExpr,
    span: Span,
    cx: &mut EvalCx<'_>,
) -> Option<Value> {
    let v = eval_expr(operand, cx)?;
    match apply_unary(op, &v) {
        Ok(out) => Some(out),
        Err(err) => {
            push_op_error(cx.diags, span, err);
            None
        }
    }
}

/// `[e1, ..., en]` array/slice literal, including the empty `[]` form.
/// Elements evaluate left-to-right into a
/// [`Value::Array`].
pub(super) fn eval_array(elems: &[HirExpr], cx: &mut EvalCx<'_>) -> Option<Value> {
    let mut values = Vec::with_capacity(elems.len());
    for elem in elems {
        values.push(eval_expr(elem, cx)?);
    }
    Some(Value::Array(values))
}

/// `receiver[index]` read. The receiver must evaluate to
/// a [`Value::Array`] and the index to an integer within bounds;
/// either shape mismatch or an out-of-range index is a comptime panic.
pub(super) fn eval_index(receiver: &HirExpr, index: &HirExpr, span: Span, cx: &mut EvalCx<'_>) -> Option<Value> {
    let recv_v = eval_expr(receiver, cx)?;
    let idx_v = eval_expr(index, cx)?;
    let Value::Array(items) = &recv_v else {
        push_panic(
            cx.diags,
            span,
            format!(
                "comptime index requires an array/slice receiver, found `{}`",
                recv_v.kind().name()
            ),
        );
        return None;
    };
    let Some(idx) = value_as_index(&idx_v) else {
        push_panic(
            cx.diags,
            span,
            format!(
                "comptime index must be a non-negative integer, found `{}`",
                idx_v.kind().name()
            ),
        );
        return None;
    };
    match items.get(idx) {
        Some(v) => Some(v.clone()),
        None => {
            push_panic(
                cx.diags,
                span,
                format!("comptime index {idx} out of range for array of length {}", items.len()),
            );
            None
        }
    }
}

/// Read an integer [`Value`] as a Rust `usize` array index. `None` for
/// a non-integer value, a signed-negative value, or a value too large
/// to fit `usize` on the host.
fn value_as_index(v: &Value) -> Option<usize> {
    let Value::Int(i) = v else { return None };
    if let Some(u) = i.as_u128() {
        usize::try_from(u).ok()
    } else {
        i.as_i128().and_then(|s| usize::try_from(s).ok())
    }
}

/// Evaluate `target.supports(Cap)`.
///
/// The argument was type-checked as a capability type-name and
/// recorded on `comptime_type_paths`, so it evaluates to a
/// `Value::Type` carrying a `TyKind::Capability`. The result is the
/// per-target availability of that capability, read from
/// [`edda_target::TargetTriple::supports_capability`] — the same locked
/// catalogue the signature-walk cap-availability check consults.
fn eval_target_supports(
    args: &[HirCallArg],
    span: Span,
    cx: &mut EvalCx<'_>,
) -> Option<Value> {
    if let Some(arg) = args.iter().find(|a| a.mode.is_some()) {
        push_not_supported(
            cx.diags,
            arg.span,
            "call-site mode keyword on a comptime built-in argument",
        );
        return None;
    }
    if let Some(arg) = args.iter().find(|a| a.name.is_some()) {
        push_not_supported(
            cx.diags,
            arg.span,
            "named-payload argument on a comptime built-in",
        );
        return None;
    }
    if args.len() != 1 {
        push_not_supported(
            cx.diags,
            span,
            "`target.supports` takes exactly one capability argument",
        );
        return None;
    }
    let arg_val = eval_expr(&args[0].expr, cx)?;
    let Value::Type(tyid) = arg_val else {
        push_panic(
            cx.diags,
            span,
            "`target.supports` expects a capability type argument".to_string(),
        );
        return None;
    };
    let TyKind::Capability(cap) = cx.ty_interner.kind(tyid) else {
        push_panic(
            cx.diags,
            span,
            "`target.supports` expects a capability type-name (e.g. `Subprocess`)".to_string(),
        );
        return None;
    };
    Some(Value::Bool(
        cx.target.triple().supports_capability(cap.name()),
    ))
}

pub(super) fn eval_call(
    callee: &HirExpr,
    args: &[HirCallArg],
    span: Span,
    cx: &mut EvalCx<'_>,
) -> Option<Value> {
    let HirExprKind::Path(path) = &callee.kind else {
        push_not_supported(cx.diags, span, "comptime call to non-path callee");
        return None;
    };
    // Dotted `target.supports(Cap)` comptime built-in
    // — the lone two-segment built-in.
    // The typechecker recorded its capability arg as a `Value::Type`
    // (a `TyKind::Capability` handle); answer against the active
    // target's cap-availability catalogue.
    if path.segments.len() == 2
        && cx.interner.resolve(path.segments[0].name) == "target"
        && cx.interner.resolve(path.segments[1].name) == "supports"
    {
        return eval_target_supports(args, span, cx);
    }
    // A module-qualified callee (`theme.contrast_ok(...)`) is never a
    // built-in — the catalogue heads are single-segment and the lone
    // two-segment `target.supports` was dispatched above. The
    // typechecker recorded the resolved binding by call span exactly
    // as for single-segment callees, so route straight to the
    // user-function path.
    if path.segments.len() != 1 {
        return eval_user_call(args, span, cx);
    }
    let name = cx.interner.resolve(path.segments[0].name);
    let Some(builtin) = builtin_for_name(name) else {
        // Not a built-in: a user-function call. Resolve through the
        // typechecker-recorded call map + the caller's FnDeclLookup
        // and interpret the callee's body.
        return eval_user_call(args, span, cx);
    };
    // A call-site mode keyword has no defined comptime semantics — the
    // built-ins are pure functions of their argument values. Reject so
    // the user's intent is not silently dropped.
    if let Some(arg) = args.iter().find(|a| a.mode.is_some()) {
        push_not_supported(
            cx.diags,
            arg.span,
            "call-site mode keyword on a comptime built-in argument",
        );
        return None;
    }
    // Named-payload arguments only appear at variant-constructor call
    // sites per `declarations.md` §252; the comptime built-ins are
    // positional pure functions, so reject rather than silently drop.
    if let Some(arg) = args.iter().find(|a| a.name.is_some()) {
        push_not_supported(
            cx.diags,
            arg.span,
            "named-payload argument on a comptime built-in",
        );
        return None;
    }
    // Evaluate arguments left-to-right; if any pushes a diagnostic,
    // we still bail out without invoking the built-in.
    let mut values = Vec::with_capacity(args.len());
    for arg in args {
        let v = eval_expr(&arg.expr, cx)?;
        values.push(v);
    }
    // Route through `eval_builtin_with_decls` so `size_of(InodeRecord)`
    // — a nominal user type — resolves through the threaded
    // `TypeDeclLookup`. When no lookup is attached (`cx.type_decls` is
    // `None`), the lookup falls back to the no-op resolver, matching
    // the prior `eval_builtin` behaviour.
    match cx.type_decls {
        Some(decls) => eval_builtin_with_decls(
            builtin,
            &values,
            span,
            cx.ty_interner,
            cx.target,
            cx.interner,
            &mut DynDeclLookup(decls),
            cx.diags,
        ),
        None => eval_builtin_with_decls(
            builtin,
            &values,
            span,
            cx.ty_interner,
            cx.target,
            cx.interner,
            &mut crate::eval::NoDecls,
            cx.diags,
        ),
    }
}

/// Route a non-built-in callee to its user-function declaration:
/// the typechecker recorded the callee's `BindingId`
/// per call span (like `comptime_type_paths`); the caller threaded a
/// [`crate::FnDeclLookup`] resolving that id to the declaration.
fn eval_user_call(args: &[HirCallArg], span: Span, cx: &mut EvalCx<'_>) -> Option<Value> {
    let (Some(calls), Some(decls)) = (cx.comptime_fn_calls, cx.fn_decls) else {
        push_not_supported(
            cx.diags,
            span,
            "comptime call to non-built-in (caller did not thread fn-call resolutions)",
        );
        return None;
    };
    let Some(binding) = calls.get(&span).copied() else {
        push_not_supported(
            cx.diags,
            span,
            "comptime call to non-built-in (no recorded callee resolution at this call site)",
        );
        return None;
    };
    let Some(decl) = decls.lookup_fn_decl(binding) else {
        push_panic(
            cx.diags,
            span,
            "comptime callee does not resolve to a function body in this package \
             (extern-bodied callees cannot evaluate at comptime)"
                .to_string(),
        );
        return None;
    };
    eval_fn_body_call(decl, args, span, cx)
}

/// Interpret a resolved user-function call: purity-gate the callee's
/// row, evaluate arguments in the caller's scope, bind parameters in
/// a fresh frame, evaluate the body, and take a `return` unwind as
/// the call's value. Mirrors the native cteval's
/// `evaluate_user_function_body` / `bind_params` /
/// `check_callee_purity`.
fn eval_fn_body_call(
    decl: FnDeclInfo<'_>,
    args: &[HirCallArg],
    span: Span,
    cx: &mut EvalCx<'_>,
) -> Option<Value> {
    // Comptime-purity gate — defence-in-depth mirror of the
    // typechecker's P-CompTimePure call-site rule: the callee's
    // declared row must be a subset of `{panic, yield: T}`.
    let impure = decl.sig.effects.entries().iter().any(|e| {
        !matches!(
            e,
            EffectEntry::Pure(PureEffect::Panic) | EffectEntry::Pure(PureEffect::Yield(_))
        )
    });
    if impure {
        push_panic(
            cx.diags,
            span,
            format!(
                "comptime call to `{}` is not comptime-pure: effect row `{}` is not a subset of {{panic, yield: T}}",
                cx.interner.resolve(decl.name),
                decl.sig.effects.display(cx.interner, cx.ty_interner),
            ),
        );
        return None;
    }
    // A call-site mode keyword has no comptime semantics — values are
    // copied into the frame, so a `mutable` argument's writes would be
    // silently lost. Reject rather than drop intent, matching the
    // built-in path.
    if let Some(arg) = args.iter().find(|a| a.mode.is_some()) {
        push_not_supported(
            cx.diags,
            arg.span,
            "call-site mode keyword on a comptime user-function argument",
        );
        return None;
    }
    if let Some(arg) = args.iter().find(|a| a.name.is_some()) {
        push_not_supported(
            cx.diags,
            arg.span,
            "named-payload argument on a comptime user-function call",
        );
        return None;
    }
    // Arity is typechecker-guaranteed; check defensively so a manual
    // HIR construction fails loudly instead of mis-binding.
    if args.len() != decl.sig.params.len() {
        push_panic(
            cx.diags,
            span,
            format!(
                "comptime call arity mismatch: `{}` expects {} argument(s), {} supplied",
                cx.interner.resolve(decl.name),
                decl.sig.params.len(),
                args.len(),
            ),
        );
        return None;
    }
    // Evaluate arguments left-to-right in the caller's scope (the
    // caller's frame base still applies).
    let mut values = Vec::with_capacity(args.len());
    for arg in args {
        values.push(eval_expr(&arg.expr, cx)?);
    }
    // Fresh frame: parameters go on the env stack at the new frame
    // base so the body sees them but none of the caller's locals.
    let saved_depth = cx.env.depth();
    let saved_base = cx.frame_base;
    cx.frame_base = saved_depth;
    for (param, value) in decl.sig.params.iter().zip(values) {
        cx.env.push_binding(param.name, value);
    }
    // A callee's `break`/`continue` must not target a caller's loop, so
    // the loop nesting resets to zero across the frame (mirroring the
    // `frame_base` env isolation above) and is restored on return.
    let saved_loop_depth = cx.loop_depth;
    cx.loop_depth = 0;
    cx.fn_call_depth += 1;
    let body_result = eval_block(decl.body, cx);
    cx.fn_call_depth -= 1;
    cx.loop_depth = saved_loop_depth;
    cx.frame_base = saved_base;
    cx.env.truncate_to(saved_depth);
    match body_result {
        Some(v) => Some(v),
        // `None` with a pending return is the callee's `return`
        // unwinding — that value is the call's result. `None` without
        // one is a genuine failure (diagnostic already pushed).
        None => cx.pending_return.take(),
    }
}

pub(super) fn eval_return(
    inner: Option<&HirExpr>,
    span: Span,
    cx: &mut EvalCx<'_>,
) -> Option<Value> {
    if cx.fn_call_depth == 0 {
        // Native parity: a `return` that does not belong to a comptime-
        // interpreted function body cannot mean anything here.
        push_panic(
            cx.diags,
            span,
            "comptime `return` reached outside a function-body context".to_string(),
        );
        return None;
    }
    let value = match inner {
        Some(e) => eval_expr(e, cx)?,
        None => Value::Unit,
    };
    cx.pending_return = Some(value);
    None
}

/// Bridge so the trait-object `&dyn TypeDeclLookup` flowing through
/// `EvalCx::type_decls` can satisfy [`crate::eval::eval_builtin_with_decls`]'s
/// `&mut L: TypeDeclLookup` constraint without exposing the trait
/// object directly. The wrapper holds the trait object and forwards
/// the single lookup method.
struct DynDeclLookup<'a>(&'a dyn TypeDeclLookup);

impl TypeDeclLookup for DynDeclLookup<'_> {
    fn lookup_type_decl(
        &self,
        binding: edda_resolve::BindingId,
    ) -> Option<&edda_types::TypeDeclInfo> {
        self.0.lookup_type_decl(binding)
    }
}

pub(super) fn eval_if(
    cond: &HirExpr,
    then_block: &HirBlock,
    else_branch: Option<&HirExpr>,
    span: Span,
    cx: &mut EvalCx<'_>,
) -> Option<Value> {
    let cond_v = eval_expr(cond, cx)?;
    let Value::Bool(b) = cond_v else {
        push_op_error(
            cx.diags,
            span,
            OpError::KindMismatch {
                op: "if",
                operands: (cond_v.kind().name().to_string(), String::new()),
            },
        );
        return None;
    };
    if b {
        eval_block(then_block, cx)
    } else if let Some(else_expr) = else_branch {
        eval_expr(else_expr, cx)
    } else {
        Some(Value::Unit)
    }
}

/// `for pat in iter { body }` bounded iteration. `iter`
/// is either a literal `Range` (evaluated as an integer loop with no
/// intermediate `Value` representation for the range itself) or any
/// other expression, which must reduce to a [`Value::Array`]. `for` is
/// a statement in the surface language, so evaluation always yields
/// `Value::Unit` on success.
pub(super) fn eval_for(
    pat: &HirPat,
    iter: &HirExpr,
    body: &HirBlock,
    span: Span,
    cx: &mut EvalCx<'_>,
) -> Option<Value> {
    if let HirExprKind::Range { lo, hi, kind } = &iter.kind {
        let (Some(lo_expr), Some(hi_expr)) = (lo.as_deref(), hi.as_deref()) else {
            push_not_supported(
                cx.diags,
                span,
                "comptime `for` over an open-ended range (both endpoints must be present)",
            );
            return None;
        };
        cx.loop_depth += 1;
        let r = eval_for_range(pat, lo_expr, hi_expr, *kind, body, span, cx);
        cx.loop_depth -= 1;
        return r;
    }
    let iterable = eval_expr(iter, cx)?;
    match iterable {
        Value::Array(items) => {
            cx.loop_depth += 1;
            let r = eval_for_array(pat, items, body, span, cx);
            cx.loop_depth -= 1;
            r
        }
        other => {
            push_not_supported(
                cx.diags,
                span,
                &format!(
                    "comptime `for` over a `{}` (only ranges and arrays iterate)",
                    other.kind().name()
                ),
            );
            None
        }
    }
}

/// Integer `for i in lo..<hi` / `for i in lo..=hi` iteration. Walks
/// the bound as a plain Rust `i128`/`u128` counter (matching the
/// endpoints' signedness) rather than routing each step through
/// checked [`apply_binary`] arithmetic — a `Closed` range whose `hi`
/// sits at the width's maximum representable value must not attempt a
/// one-past-the-end increment, so the loop stops immediately after the
/// iteration where the counter equals `hi` instead of incrementing
/// first and comparing after.
fn eval_for_range(
    pat: &HirPat,
    lo: &HirExpr,
    hi: &HirExpr,
    kind: RangeKind,
    body: &HirBlock,
    span: Span,
    cx: &mut EvalCx<'_>,
) -> Option<Value> {
    let lo_v = eval_expr(lo, cx)?;
    let hi_v = eval_expr(hi, cx)?;
    let (Value::Int(lo_i), Value::Int(hi_i)) = (&lo_v, &hi_v) else {
        push_panic(
            cx.diags,
            span,
            "comptime `for` range endpoints must be integers".to_string(),
        );
        return None;
    };
    let width = lo_i.width();
    if width.is_signed_integer() {
        let mut n = lo_i.as_i128().expect("signed width has an i128 payload");
        let hi_n = hi_i.as_i128().expect("signed width has an i128 payload");
        loop {
            let in_range = match kind {
                RangeKind::HalfOpen => n < hi_n,
                RangeKind::Closed => n <= hi_n,
            };
            if !in_range {
                break;
            }
            cx.bump_iteration(span, "for")?;
            match run_for_iteration(pat, Value::Int(IntValue::new_signed(width, n)), body, cx)? {
                LoopStep::Next => {}
                LoopStep::Break => break,
            }
            if matches!(kind, RangeKind::Closed) && n == hi_n {
                break;
            }
            n += 1;
        }
    } else {
        let mut n = lo_i.as_u128().expect("unsigned width has a u128 payload");
        let hi_n = hi_i.as_u128().expect("unsigned width has a u128 payload");
        loop {
            let in_range = match kind {
                RangeKind::HalfOpen => n < hi_n,
                RangeKind::Closed => n <= hi_n,
            };
            if !in_range {
                break;
            }
            cx.bump_iteration(span, "for")?;
            match run_for_iteration(pat, Value::Int(IntValue::new_unsigned(width, n)), body, cx)? {
                LoopStep::Next => {}
                LoopStep::Break => break,
            }
            if matches!(kind, RangeKind::Closed) && n == hi_n {
                break;
            }
            n += 1;
        }
    }
    Some(Value::Unit)
}

/// `for x in xs` over an already-evaluated [`Value::Array`].
fn eval_for_array(pat: &HirPat, items: Vec<Value>, body: &HirBlock, span: Span, cx: &mut EvalCx<'_>) -> Option<Value> {
    for item in items {
        cx.bump_iteration(span, "for")?;
        match run_for_iteration(pat, item, body, cx)? {
            LoopStep::Next => {}
            LoopStep::Break => break,
        }
    }
    Some(Value::Unit)
}

/// Outcome of one loop-body trip: advance to the next iteration or
/// stop the loop. A `break` inside the body surfaces as
/// [`LoopStep::Break`]; a `continue` is absorbed as [`LoopStep::Next`].
/// A `return` unwind or genuine failure is *not* a `LoopStep` — it
/// propagates as `None` from [`run_for_iteration`].
enum LoopStep {
    Next,
    Break,
}

/// Run one `for`-loop iteration: bind `pat` to `value` for the
/// duration of `body`, then discard the binding. A `break` inside
/// `body` yields [`LoopStep::Break`] and a `continue` yields
/// [`LoopStep::Next`] (both surface as a `None` from `eval_block` that
/// the pending-signal registers disambiguate). A `return` inside `body`
/// unwinds as `None` with no `LoopStep`, exactly as it unwinds through
/// an ordinary nested block — `cx.pending_return` distinguishes it from
/// a genuine failure at the call site that takes the function's result.
fn run_for_iteration(pat: &HirPat, value: Value, body: &HirBlock, cx: &mut EvalCx<'_>) -> Option<LoopStep> {
    let saved_depth = cx.env.depth();
    match &pat.kind {
        HirPatKind::Binding(ident) => cx.env.push_binding(ident.name, value),
        HirPatKind::Wildcard => {}
        _ => {
            push_not_supported(
                cx.diags,
                pat.span,
                "comptime `for` pattern destructuring (only a plain binding or `_` is admitted)",
            );
            cx.env.truncate_to(saved_depth);
            return None;
        }
    }
    let result = eval_block(body, cx);
    cx.env.truncate_to(saved_depth);
    match result {
        Some(_) => Some(LoopStep::Next),
        None => interpret_loop_unwind(cx),
    }
}

/// Classify the `None` an inner loop body produced: `break` (stop the
/// loop), `continue` (advance), or a genuine unwind (`return` / failure)
/// that must keep propagating as `None`. Shared by the `for` and `loop`
/// drivers so both consume the pending-signal registers identically.
fn interpret_loop_unwind(cx: &mut EvalCx<'_>) -> Option<LoopStep> {
    if cx.pending_break.take().is_some() {
        return Some(LoopStep::Break);
    }
    if cx.pending_continue {
        cx.pending_continue = false;
        return Some(LoopStep::Next);
    }
    // `return` unwind (pending_return set) or a genuine failure
    // (diagnostic already pushed) — keep propagating.
    None
}

/// `loop { body }` — the unbounded surface form, bounded here by the
/// same [`MAX_ITERATIONS`] budget as `for`. Yields the value carried by
/// the `break` that exits it (`Unit` for a bare `break`); a `return`
/// inside the body unwinds past the loop to the enclosing function
/// frame. The loop's own `decreases` measure and label are irrelevant
/// to comptime evaluation and ignored.
pub(super) fn eval_loop(body: &HirBlock, span: Span, cx: &mut EvalCx<'_>) -> Option<Value> {
    cx.loop_depth += 1;
    let result = eval_loop_body(body, span, cx);
    cx.loop_depth -= 1;
    result
}

fn eval_loop_body(body: &HirBlock, span: Span, cx: &mut EvalCx<'_>) -> Option<Value> {
    loop {
        cx.bump_iteration(span, "loop")?;
        if eval_block(body, cx).is_some() {
            // Body ran to completion with no control-flow unwind — a
            // bare `loop` re-enters. Termination rests on the body
            // eventually issuing a `break`/`return` or the iteration
            // budget tripping.
            continue;
        }
        // A `loop` (unlike a statement `for`) yields the `break` value,
        // so read it directly rather than through `interpret_loop_unwind`.
        if let Some(v) = cx.pending_break.take() {
            return Some(v);
        }
        if cx.pending_continue {
            cx.pending_continue = false;
            continue;
        }
        // `return` unwind or genuine failure — keep propagating.
        return None;
    }
}

pub(super) fn eval_break(
    label: Option<&Ident>,
    value: Option<&HirExpr>,
    span: Span,
    cx: &mut EvalCx<'_>,
) -> Option<Value> {
    if label.is_some() {
        push_not_supported(
            cx.diags,
            span,
            "comptime labeled `break` (only unlabeled break targeting the innermost loop is supported)",
        );
        return None;
    }
    if cx.loop_depth == 0 {
        push_panic(
            cx.diags,
            span,
            "comptime `break` reached outside a loop context".to_string(),
        );
        return None;
    }
    let v = match value {
        Some(e) => eval_expr(e, cx)?,
        None => Value::Unit,
    };
    cx.pending_break = Some(v);
    None
}

pub(super) fn eval_continue(label: Option<&Ident>, span: Span, cx: &mut EvalCx<'_>) -> Option<Value> {
    if label.is_some() {
        push_not_supported(
            cx.diags,
            span,
            "comptime labeled `continue` (only unlabeled continue targeting the innermost loop is supported)",
        );
        return None;
    }
    if cx.loop_depth == 0 {
        push_panic(
            cx.diags,
            span,
            "comptime `continue` reached outside a loop context".to_string(),
        );
        return None;
    }
    cx.pending_continue = true;
    None
}

pub(super) fn eval_block(block: &HirBlock, cx: &mut EvalCx<'_>) -> Option<Value> {
    // Depth-save / truncate scoping, mirroring the native cteval
    // `evaluate_block`: bindings declared inside this block are popped
    // on every exit path (including the early-out on failure).
    let saved_depth = cx.env.depth();
    let result = eval_block_stmts(block, cx);
    cx.env.truncate_to(saved_depth);
    result
}

fn eval_block_stmts(block: &HirBlock, cx: &mut EvalCx<'_>) -> Option<Value> {
    for stmt in block.stmts.iter() {
        eval_stmt(stmt, cx)?;
    }
    match &block.trailing {
        Some(trailing) => eval_expr(trailing, cx),
        None => Some(Value::Unit),
    }
}

fn eval_stmt(stmt: &HirStmt, cx: &mut EvalCx<'_>) -> Option<()> {
    match &stmt.kind {
        HirStmtKind::Expr(e) => {
            eval_expr(e, cx)?;
            Some(())
        }
        HirStmtKind::Let {
            mutability,
            pat,
            ty: _,
            init,
        } => eval_let_stmt(*mutability, pat, init.as_ref(), stmt.span, cx),
        HirStmtKind::Assign { target, op, rhs } => eval_assign_stmt(target, *op, rhs, stmt.span, cx),
    }
}

fn eval_let_stmt(
    mutability: BindingMode,
    pat: &HirPat,
    init: Option<&HirExpr>,
    span: Span,
    cx: &mut EvalCx<'_>,
) -> Option<()> {
    if matches!(mutability, BindingMode::Uninit) {
        // Native parity: `uninit` slots are filled through `init`-mode
        // calls, which have no comptime interpretation yet.
        push_not_supported(cx.diags, span, "comptime body `uninit` declaration");
        return None;
    }
    let Some(init) = init else {
        push_not_supported(cx.diags, span, "comptime `let`/`var` without an initialiser");
        return None;
    };
    let value = eval_expr(init, cx)?;
    match &pat.kind {
        HirPatKind::Binding(ident) => {
            cx.env.push_binding(ident.name, value);
            Some(())
        }
        // `let _ = e` evaluates `e` for its (comptime-pure) result and
        // discards it.
        HirPatKind::Wildcard => Some(()),
        _ => {
            push_not_supported(
                cx.diags,
                span,
                "comptime `let` destructuring (pattern binding beyond a single name)",
            );
            None
        }
    }
}

fn eval_assign_stmt(
    target: &HirExpr,
    op: AssignOp,
    rhs: &HirExpr,
    span: Span,
    cx: &mut EvalCx<'_>,
) -> Option<()> {
    // A plain binding or a one-level `name[index]` array slot are
    // assignable at comptime — other compound-value lvalues (field /
    // tuple positions, nested indexing) need the post-V1.0 compound
    // carrier in the Value lattice. Same restriction as the native
    // cteval `evaluate_assign_stmt`, extended for `Value::Array`.
    match &target.kind {
        HirExprKind::Path(p) if p.segments.len() == 1 => {
            eval_assign_binding(p.segments[0].name, op, rhs, span, cx)
        }
        HirExprKind::Index { receiver, index } => eval_assign_index(receiver, index, op, rhs, span, cx),
        _ => {
            push_not_supported(
                cx.diags,
                span,
                "comptime assignment to a non-binding target (field / tuple lvalue)",
            );
            None
        }
    }
}

/// `name = rhs` / `name += rhs` (and the other compound forms) against
/// a plain binding.
fn eval_assign_binding(
    name: edda_intern::Symbol,
    op: AssignOp,
    rhs: &HirExpr,
    span: Span,
    cx: &mut EvalCx<'_>,
) -> Option<()> {
    let rhs_value = eval_expr(rhs, cx)?;
    let assign_value = match assign_bin_op(op) {
        AssignKind::Plain => rhs_value,
        AssignKind::Compound(bin_op) => {
            let Some(current) = cx.env.lookup_from(cx.frame_base, name) else {
                push_panic(
                    cx.diags,
                    span,
                    format!(
                        "comptime compound assignment to `{}` which is not bound in this scope",
                        cx.interner.resolve(name)
                    ),
                );
                return None;
            };
            match apply_binary(bin_op, &current.clone(), &rhs_value) {
                Ok(v) => v,
                Err(err) => {
                    push_op_error(cx.diags, span, err);
                    return None;
                }
            }
        }
        AssignKind::Unsupported(what) => {
            push_not_supported(cx.diags, span, what);
            return None;
        }
    };
    if !cx.env.assign_binding_from(cx.frame_base, name, assign_value) {
        push_panic(
            cx.diags,
            span,
            format!(
                "comptime assignment to `{}` which is not bound in this scope",
                cx.interner.resolve(name)
            ),
        );
        return None;
    }
    Some(())
}

/// `name[index] = rhs` (and the compound forms) against a
/// [`Value::Array`]-holding binding. The receiver must
/// be a single-segment `Path` naming a live binding — nested indexing
/// (`grid[i][j] = ...`) is not yet supported, matching
/// [`eval_assign_stmt`]'s "one level of lvalue" scope.
fn eval_assign_index(
    receiver: &HirExpr,
    index: &HirExpr,
    op: AssignOp,
    rhs: &HirExpr,
    span: Span,
    cx: &mut EvalCx<'_>,
) -> Option<()> {
    let HirExprKind::Path(p) = &receiver.kind else {
        push_not_supported(
            cx.diags,
            span,
            "comptime index-assignment to a non-binding receiver (nested indexing)",
        );
        return None;
    };
    if p.segments.len() != 1 {
        push_not_supported(
            cx.diags,
            span,
            "comptime index-assignment through a qualified path receiver",
        );
        return None;
    }
    let name = p.segments[0].name;
    let idx_v = eval_expr(index, cx)?;
    let Some(idx) = value_as_index(&idx_v) else {
        push_panic(
            cx.diags,
            span,
            format!(
                "comptime index must be a non-negative integer, found `{}`",
                idx_v.kind().name()
            ),
        );
        return None;
    };
    let rhs_v = eval_expr(rhs, cx)?;
    let Some(current) = cx.env.lookup_from(cx.frame_base, name) else {
        push_panic(
            cx.diags,
            span,
            format!(
                "comptime index-assignment to `{}` which is not bound in this scope",
                cx.interner.resolve(name)
            ),
        );
        return None;
    };
    let Value::Array(items) = current else {
        push_panic(
            cx.diags,
            span,
            format!(
                "comptime index-assignment target `{}` is not an array (found `{}`)",
                cx.interner.resolve(name),
                current.kind().name()
            ),
        );
        return None;
    };
    let len = items.len();
    let Some(slot) = items.get(idx) else {
        push_panic(
            cx.diags,
            span,
            format!("comptime index {idx} out of range for array of length {len}"),
        );
        return None;
    };
    let new_elem = match assign_bin_op(op) {
        AssignKind::Plain => rhs_v,
        AssignKind::Compound(bin_op) => match apply_binary(bin_op, slot, &rhs_v) {
            Ok(v) => v,
            Err(err) => {
                push_op_error(cx.diags, span, err);
                return None;
            }
        },
        AssignKind::Unsupported(what) => {
            push_not_supported(cx.diags, span, what);
            return None;
        }
    };
    let mut new_items = items.clone();
    new_items[idx] = new_elem;
    cx.env.assign_binding_from(cx.frame_base, name, Value::Array(new_items));
    Some(())
}

/// How an [`AssignOp`] evaluates: plain overwrite, compound through a
/// binary operator against the current value, or not-yet-supported.
enum AssignKind {
    Plain,
    Compound(BinOp),
    Unsupported(&'static str),
}

/// Map the compound-assignment operator onto the binary operator it
/// applies. Bitwise / shift compound assigns mirror the native cteval:
/// post-V1.0 per the locked decidable fragment.
fn assign_bin_op(op: AssignOp) -> AssignKind {
    match op {
        AssignOp::Plain => AssignKind::Plain,
        AssignOp::Add => AssignKind::Compound(BinOp::Add),
        AssignOp::Sub => AssignKind::Compound(BinOp::Sub),
        AssignOp::Mul => AssignKind::Compound(BinOp::Mul),
        AssignOp::Div => AssignKind::Compound(BinOp::Div),
        AssignOp::Mod => AssignKind::Compound(BinOp::Mod),
        AssignOp::BitAnd => AssignKind::Unsupported("comptime `&=` (bitwise compound assignment)"),
        AssignOp::BitOr => AssignKind::Unsupported("comptime `|=` (bitwise compound assignment)"),
        AssignOp::BitXor => AssignKind::Unsupported("comptime `^=` (bitwise compound assignment)"),
        AssignOp::Shl => AssignKind::Unsupported("comptime `<<=` (shift compound assignment)"),
        AssignOp::Shr => AssignKind::Unsupported("comptime `>>=` (shift compound assignment)"),
    }
}

pub(super) fn eval_panic(msg: &HirExpr, span: Span, cx: &mut EvalCx<'_>) -> Option<Value> {
    let v = eval_expr(msg, cx)?;
    let message = match v {
        Value::Str(sym) => cx.interner.resolve(sym).to_string(),
        other => format!("<non-string panic value: {}>", other.kind().name()),
    };
    push_panic(cx.diags, span, message);
    None
}
