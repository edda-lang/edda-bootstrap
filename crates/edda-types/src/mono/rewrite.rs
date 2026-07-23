//! Template-call discovery and call-site rewriting (layer 2).
//!
//! Walks one function body (post D-22 expansion), finds every direct
//! call whose callee path resolves to a template binding, binds the
//! type parameters (outbound from the value-argument shapes, inbound
//! `comptime <name>: Type` from their positional comptime type
//! arguments — which are then removed from the call), drives
//! specialization, and rewrites the callee path in place to the
//! specialization's mangled single-segment name with an unresolved
//! ([`Span::DUMMY`]) path span. The unresolved span is load-bearing on
//! both downstream sides: `infer::call::synth_call` misses the
//! `Resolutions` lookup and falls through to the [`MonoFns`] name
//! table, and MIR's `try_resolve_function_binding` misses the same
//! lookup and falls through to its `function_symbols` single-segment
//! name fallback. No production resolution is ever keyed by
//! `Span::DUMMY`, so the miss is guaranteed.

use edda_diag::Diagnostics;
use edda_resolve::Resolved;
use edda_span::Span;
use edda_syntax::ast::{
    Block, CallArg, Expr, ExprKind, FnDecl, Ident, Param, Path, Stmt, StmtKind, TypeKind,
};

use super::infer_arg::{TypeLeafEnv, declared_leaf_of_expr};
use super::type_arg::comptime_type_of_expr;
use super::{BoundTy, MonoCx, MonoState};

/// Rewrite every template call inside `block`. Returns the rewritten
/// clone, or `None` when the body calls no template.
pub(crate) fn rewrite_template_calls(
    block: &Block,
    caller: &FnDecl,
    cx: &MonoCx<'_, '_>,
    state: &mut MonoState,
    diags: &mut Diagnostics,
) -> Option<Block> {
    if cx.templates.is_empty() || !block_calls_template(block, cx) {
        return None;
    }
    let mut out = block.clone();
    rewrite_block_calls(&mut out, &caller.params, 0, cx, state, diags);
    Some(out)
}

/// Cheap pre-scan: does any call in `block` resolve to a template?
fn block_calls_template(block: &Block, cx: &MonoCx<'_, '_>) -> bool {
    let mut found = false;
    walk_block(block, &mut |e| {
        if found {
            return;
        }
        match &e.kind {
            ExprKind::Call { callee, .. } if callee_template(callee, cx).is_some() => {
                found = true;
            }
            ExprKind::MethodCall { receiver, name, .. }
                if qualified_call_template(receiver, name, cx).is_some() =>
            {
                found = true;
            }
            _ => {}
        }
    });
    found
}

/// Resolve a callee expression to a template binding, if it is one.
fn callee_template(
    callee: &Expr,
    cx: &MonoCx<'_, '_>,
) -> Option<edda_resolve::BindingId> {
    let ExprKind::Path(p) = &callee.kind else {
        return None;
    };
    match cx.package.resolutions().lookup_path(p.span) {
        Some(Resolved::Binding(id)) if cx.templates.contains_key(&id) => Some(id),
        _ => None,
    }
}

/// Resolve a leaf-qualified module call (`module_leaf.name(args...)`,
/// parsed as `MethodCall` per the locked `obj.method(args)` /
/// `mod.func(args)` shared postfix grammar) to a template binding in
/// the named module, if `name` names one. This is the cross-package
/// counterpart to `callee_template`, which only recognises a template
/// reached through a same-package unqualified `Call`.
fn qualified_call_template(
    receiver: &Expr,
    name: &Ident,
    cx: &MonoCx<'_, '_>,
) -> Option<edda_resolve::BindingId> {
    let ExprKind::Path(p) = &receiver.kind else {
        return None;
    };
    let Some(Resolved::Module(module_id)) = cx.package.resolutions().lookup_path(p.span) else {
        return None;
    };
    let id = cx.package.module(module_id).items.lookup(name.name)?;
    cx.templates.contains_key(&id).then_some(id)
}

/// Rewrite every template call inside an owned block, threading the
/// caller's declared-type environment.
pub(super) fn rewrite_block_calls(
    block: &mut Block,
    caller_params: &[Param],
    depth: usize,
    cx: &MonoCx<'_, '_>,
    state: &mut MonoState,
    diags: &mut Diagnostics,
) {
    let mut env = TypeLeafEnv::from_params(caller_params);
    rewrite_block(block, &mut env, depth, cx, state, diags);
}

fn rewrite_block(
    block: &mut Block,
    env: &mut TypeLeafEnv,
    depth: usize,
    cx: &MonoCx<'_, '_>,
    state: &mut MonoState,
    diags: &mut Diagnostics,
) {
    for s in &mut block.stmts {
        env.observe_stmt(s);
        rewrite_stmt(s, env, depth, cx, state, diags);
    }
    if let Some(t) = &mut block.trailing {
        rewrite_expr(t, env, depth, cx, state, diags);
    }
}

fn rewrite_stmt(
    s: &mut Stmt,
    env: &mut TypeLeafEnv,
    depth: usize,
    cx: &MonoCx<'_, '_>,
    state: &mut MonoState,
    diags: &mut Diagnostics,
) {
    match &mut s.kind {
        StmtKind::Let { init, .. } => {
            if let Some(e) = init {
                rewrite_expr(e, env, depth, cx, state, diags);
            }
        }
        StmtKind::Assign { target, rhs, .. } => {
            rewrite_expr(target, env, depth, cx, state, diags);
            rewrite_expr(rhs, env, depth, cx, state, diags);
        }
        StmtKind::Expr(e) => rewrite_expr(e, env, depth, cx, state, diags),
    }
}

fn rewrite_expr(
    e: &mut Expr,
    env: &mut TypeLeafEnv,
    depth: usize,
    cx: &MonoCx<'_, '_>,
    state: &mut MonoState,
    diags: &mut Diagnostics,
) {
    // Recurse into children first.
    rewrite_children(e, env, depth, cx, state, diags);

    match &e.kind {
        ExprKind::Call { .. } => rewrite_call(e, env, depth, cx, state, diags),
        ExprKind::MethodCall { .. } => rewrite_qualified_call(e, env, depth, cx, state, diags),
        _ => {}
    }
}

/// Same-package template recognition: `callee(args...)` where `callee`
/// is a plain `Path` resolving to a template binding. Rewrites the
/// callee path in place to the specialization's mangled name.
fn rewrite_call(
    e: &mut Expr,
    env: &mut TypeLeafEnv,
    depth: usize,
    cx: &MonoCx<'_, '_>,
    state: &mut MonoState,
    diags: &mut Diagnostics,
) {
    let ExprKind::Call { callee, args } = &mut e.kind else {
        return;
    };
    let Some(template_binding) = callee_template(callee, cx) else {
        return;
    };
    let Some(mangled) = specialize_call(template_binding, args, env, depth, e.span, cx, state, diags) else {
        return;
    };

    // Rewrite the callee to the specialization's mangled name. The
    // DUMMY path span misses every resolution lookup, routing both the
    // typechecker and MIR through their name-table fallbacks.
    if let ExprKind::Path(p) = &mut callee.kind {
        let head_span = p.segments[0].span;
        p.segments = vec![Ident {
            name: mangled,
            span: head_span,
        }];
        p.span = Span::DUMMY;
    }
}

/// Cross-package template recognition:
/// `module_leaf.name(args...)`, parsed as `MethodCall` per the shared
/// `obj.method(args)` / `mod.func(args)` postfix grammar, where
/// `module_leaf` resolves to an imported module and `name` names a
/// template in it. On success, replaces the whole `MethodCall` node
/// with a `Call` to the specialization's mangled name — the module
/// receiver named no value, so it is discarded rather than threaded
/// through as a first argument.
fn rewrite_qualified_call(
    e: &mut Expr,
    env: &mut TypeLeafEnv,
    depth: usize,
    cx: &MonoCx<'_, '_>,
    state: &mut MonoState,
    diags: &mut Diagnostics,
) {
    let ExprKind::MethodCall { receiver, name, .. } = &e.kind else {
        return;
    };
    let Some(template_binding) = qualified_call_template(receiver, name, cx) else {
        return;
    };
    let ExprKind::MethodCall { args, .. } = &mut e.kind else {
        unreachable!("matched MethodCall above")
    };
    let Some(mangled) = specialize_call(template_binding, args, env, depth, e.span, cx, state, diags) else {
        return;
    };
    let ExprKind::MethodCall { name, args, .. } = std::mem::replace(&mut e.kind, ExprKind::Error) else {
        unreachable!("matched MethodCall above")
    };
    e.kind = ExprKind::Call {
        callee: Box::new(Expr {
            span: name.span,
            kind: ExprKind::Path(Path {
                segments: vec![Ident { name: mangled, span: name.span }],
                span: Span::DUMMY,
            }),
        }),
        args,
    };
}

/// Shared template-call specialization: binds `template_binding`'s
/// inbound `comptime <name>: Type` and outbound `<comptime U: Type>`
/// parameters from `args`, drives `get_or_create_specialization`, and
/// returns the resulting mangled leaf name. Shared by `rewrite_call`
/// (same-package, unqualified) and `rewrite_qualified_call`
/// (cross-package, module-qualified) — the two only differ in how they
/// recognise the template and how they rewrite the call-site node
/// afterward.
fn specialize_call(
    template_binding: edda_resolve::BindingId,
    args: &mut Vec<CallArg>,
    env: &TypeLeafEnv,
    depth: usize,
    call_span: Span,
    cx: &MonoCx<'_, '_>,
    state: &mut MonoState,
    diags: &mut Diagnostics,
) -> Option<edda_intern::Symbol> {
    let template = &cx.templates[&template_binding];

    // Inbound `comptime <name>: Type` generics occupy positional
    // argument slots. Reconstruct each one's slot by interleaving the
    // lifted generics with the value params on source position, bind it
    // from the comptime type expression at that slot, then remove those
    // arguments so the remaining ones align with the value params for
    // both the outbound inference below and the typecheck of the
    // rewritten call.
    let inbound = super::inbound_type_generics(template.decl);
    let mut inbound_bound: Vec<BoundTy> = Vec::new();
    let mut inbound_keys: Vec<super::MonoKey> = Vec::new();
    if !inbound.is_empty() {
        let mut arg_idxs: Vec<usize> = Vec::with_capacity(inbound.len());
        for gp in &inbound {
            let idx = template
                .decl
                .params
                .iter()
                .filter(|p| p.span.lo < gp.span.lo)
                .count()
                + inbound.iter().filter(|g| g.span.lo < gp.span.lo).count();
            arg_idxs.push(idx);
        }
        for (gp, &idx) in inbound.iter().zip(&arg_idxs) {
            let evaluated = args
                .get(idx)
                .and_then(|a| comptime_type_of_expr(&a.expr, cx.shapes, cx.lower_cx.interner));
            let Some(bt) = evaluated else {
                let gname = cx.lower_cx.interner.resolve(gp.name.name);
                let fname = cx.lower_cx.interner.resolve(template.decl.name.name);
                crate::lower::emit_typecheck_error(
                    diags,
                    cx.lint_cfg,
                    call_span,
                    format!(
                        "cannot bind comptime type parameter `{gname}` of \
                         `{fname}` at this call site: the argument is not a \
                         comptime type expression (a named type or \
                         `field_type_at(T, k)` with a constant index)",
                    ),
                );
                return None;
            };
            let Some(key) = super::classify_bound_type(&bt, cx) else {
                let gname = cx.lower_cx.interner.resolve(gp.name.name);
                let desc = describe_bound(&bt, cx.lower_cx.interner);
                crate::lower::emit_typecheck_error(
                    diags,
                    cx.lint_cfg,
                    call_span,
                    format!(
                        "cannot specialize comptime type parameter `{gname}`: \
                         `{desc}` is neither a primitive nor a resolved type",
                    ),
                );
                return None;
            };
            inbound_bound.push(bt);
            inbound_keys.push(key);
        }
        let mut drop_idxs = arg_idxs;
        drop_idxs.sort_unstable_by(|a, b| b.cmp(a));
        for idx in drop_idxs {
            if idx < args.len() {
                args.remove(idx);
            }
        }
    }

    // Map each outbound generic to the declared type of the first
    // argument whose parameter type is exactly that generic.
    let mut bound: Vec<BoundTy> = Vec::new();
    let mut keys: Vec<super::MonoKey> = Vec::new();
    for gp in &template.decl.outbound_generics {
        let param_idx = template.decl.params.iter().position(|p| {
            matches!(&p.ty.kind, TypeKind::Path(tp)
                if tp.segments.len() == 1 && tp.segments[0].name == gp.name.name)
        });
        let inferred = param_idx
            .and_then(|i| args.get(i))
            .and_then(|a| declared_leaf_of_expr(&a.expr, env, cx.shapes));
        let Some(bt) = inferred else {
            let gname = cx.lower_cx.interner.resolve(gp.name.name);
            let fname = cx.lower_cx.interner.resolve(template.decl.name.name);
            crate::lower::emit_typecheck_error(
                diags,
                cx.lint_cfg,
                call_span,
                format!(
                    "cannot infer comptime type parameter `{gname}` of \
                     `{fname}` at this call site: the matching argument's \
                     declared type is not a named type or payload composite",
                ),
            );
            return None;
        };
        let Some(key) = super::classify_bound_type(&bt, cx) else {
            let gname = cx.lower_cx.interner.resolve(gp.name.name);
            let desc = describe_bound(&bt, cx.lower_cx.interner);
            crate::lower::emit_typecheck_error(
                diags,
                cx.lint_cfg,
                call_span,
                format!(
                    "cannot specialize comptime type parameter `{gname}`: \
                     `{desc}` is neither a primitive nor a resolved type",
                ),
            );
            return None;
        };
        bound.push(bt);
        keys.push(key);
    }

    // Outbound first, then inbound — the order `get_or_create_specialization`
    // zips the template's type generics in.
    bound.extend(inbound_bound);
    keys.extend(inbound_keys);

    super::get_or_create_specialization(
        template_binding,
        keys,
        &bound,
        depth,
        call_span,
        cx,
        state,
        diags,
    )
}

/// Human-readable spelling of a bound type for a diagnostic: a named
/// reference's leaf, `()` for the unit composite, `(a, b, …)` for a
/// payload tuple, or `[E]` for a slice bound.
fn describe_bound(bt: &BoundTy, interner: &edda_intern::Interner) -> String {
    match bt {
        BoundTy::Named(leaf, _) => interner.resolve(*leaf).to_string(),
        BoundTy::Unit => "()".to_string(),
        BoundTy::Tuple(elems) => {
            let mut out = String::from("(");
            for (i, (leaf, _)) in elems.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(interner.resolve(*leaf));
            }
            out.push(')');
            out
        }
        BoundTy::Slice(leaf, _) => format!("[{}]", interner.resolve(*leaf)),
    }
}

/// Structural recursion into an expression's children.
fn rewrite_children(
    e: &mut Expr,
    env: &mut TypeLeafEnv,
    depth: usize,
    cx: &MonoCx<'_, '_>,
    state: &mut MonoState,
    diags: &mut Diagnostics,
) {
    match &mut e.kind {
        ExprKind::FString(parts) => {
            for part in parts {
                if let edda_syntax::ast::FStringPart::Slot(slot) = part {
                    rewrite_expr(slot, env, depth, cx, state, diags);
                }
            }
        }
        ExprKind::Call { callee, args } => {
            // The callee path itself is handled by `rewrite_expr`;
            // recurse into non-path callees and all arguments.
            if !matches!(&callee.kind, ExprKind::Path(_)) {
                rewrite_expr(callee, env, depth, cx, state, diags);
            }
            for a in args {
                rewrite_expr(&mut a.expr, env, depth, cx, state, diags);
            }
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            rewrite_expr(receiver, env, depth, cx, state, diags);
            for a in args {
                rewrite_expr(&mut a.expr, env, depth, cx, state, diags);
            }
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            rewrite_expr(lhs, env, depth, cx, state, diags);
            rewrite_expr(rhs, env, depth, cx, state, diags);
        }
        ExprKind::Unary { expr, .. } => rewrite_expr(expr, env, depth, cx, state, diags),
        ExprKind::Field { receiver, .. } | ExprKind::TupleIndex { receiver, .. } => {
            rewrite_expr(receiver, env, depth, cx, state, diags);
        }
        ExprKind::CompField { receiver, index } | ExprKind::Index { receiver, index } => {
            rewrite_expr(receiver, env, depth, cx, state, diags);
            rewrite_expr(index, env, depth, cx, state, diags);
        }
        ExprKind::If {
            cond,
            then_block,
            else_branch,
        } => {
            rewrite_expr(cond, env, depth, cx, state, diags);
            rewrite_block(then_block, env, depth, cx, state, diags);
            if let Some(eb) = else_branch {
                rewrite_expr(eb, env, depth, cx, state, diags);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            rewrite_expr(scrutinee, env, depth, cx, state, diags);
            for arm in arms {
                if let Some(g) = &mut arm.guard {
                    rewrite_expr(g, env, depth, cx, state, diags);
                }
                rewrite_expr(&mut arm.body, env, depth, cx, state, diags);
            }
        }
        ExprKind::Block(b) | ExprKind::ComptimeBlock(b) => {
            rewrite_block(b, env, depth, cx, state, diags);
        }
        ExprKind::Cast { expr, .. } => rewrite_expr(expr, env, depth, cx, state, diags),
        ExprKind::Range { lo, hi, .. } => {
            if let Some(l) = lo {
                rewrite_expr(l, env, depth, cx, state, diags);
            }
            if let Some(h) = hi {
                rewrite_expr(h, env, depth, cx, state, diags);
            }
        }
        ExprKind::Tuple(es) | ExprKind::Array(es) => {
            for inner in es {
                rewrite_expr(inner, env, depth, cx, state, diags);
            }
        }
        ExprKind::StructLit { fields, .. } => {
            for f in fields {
                rewrite_expr(&mut f.value, env, depth, cx, state, diags);
            }
        }
        ExprKind::Loop {
            body, decreases, ..
        } => {
            rewrite_block(body, env, depth, cx, state, diags);
            if let Some(d) = decreases {
                rewrite_expr(d, env, depth, cx, state, diags);
            }
        }
        ExprKind::For { pat, iter, body, .. } => {
            rewrite_expr(iter, env, depth, cx, state, diags);
            env.observe_for(pat, iter, cx.shapes);
            rewrite_block(body, env, depth, cx, state, diags);
        }
        ExprKind::Try(inner)
        | ExprKind::Await(inner)
        | ExprKind::Raise(inner)
        | ExprKind::Panic(inner)
        | ExprKind::Comptime(inner) => rewrite_expr(inner, env, depth, cx, state, diags),
        ExprKind::Scope { body, .. } => rewrite_block(body, env, depth, cx, state, diags),
        ExprKind::Return(opt) => {
            if let Some(inner) = opt {
                rewrite_expr(inner, env, depth, cx, state, diags);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(inner) = value {
                rewrite_expr(inner, env, depth, cx, state, diags);
            }
        }
        ExprKind::Closure(c) => rewrite_block(&mut c.body, env, depth, cx, state, diags),
        ExprKind::Handle { recovery, body, .. } => {
            rewrite_expr(recovery, env, depth, cx, state, diags);
            rewrite_block(body, env, depth, cx, state, diags);
        }
        ExprKind::Spawn(s) => {
            for a in &mut s.args {
                rewrite_expr(&mut a.init, env, depth, cx, state, diags);
            }
            rewrite_block(&mut s.body, env, depth, cx, state, diags);
        }
        ExprKind::Forall { iter, body, .. } | ExprKind::Exists { iter, body, .. } => {
            rewrite_expr(iter, env, depth, cx, state, diags);
            rewrite_expr(body, env, depth, cx, state, diags);
        }
        ExprKind::Literal(_)
        | ExprKind::Path(_)
        | ExprKind::EffectRow(_)
        | ExprKind::Continue { .. }
        | ExprKind::Error => {}
    }
}

/// Read-only walk visiting every expression in a block (pre-scan).
fn walk_block(block: &Block, f: &mut impl FnMut(&Expr)) {
    for s in &block.stmts {
        match &s.kind {
            StmtKind::Let { init, .. } => {
                if let Some(e) = init {
                    walk_expr(e, f);
                }
            }
            StmtKind::Assign { target, rhs, .. } => {
                walk_expr(target, f);
                walk_expr(rhs, f);
            }
            StmtKind::Expr(e) => walk_expr(e, f),
        }
    }
    if let Some(t) = &block.trailing {
        walk_expr(t, f);
    }
}

fn walk_expr(e: &Expr, f: &mut impl FnMut(&Expr)) {
    f(e);
    match &e.kind {
        ExprKind::FString(parts) => {
            for part in parts {
                if let edda_syntax::ast::FStringPart::Slot(slot) = part {
                    walk_expr(slot, f);
                }
            }
        }
        ExprKind::Call { callee, args } => {
            walk_expr(callee, f);
            for a in args {
                walk_expr(&a.expr, f);
            }
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            walk_expr(receiver, f);
            for a in args {
                walk_expr(&a.expr, f);
            }
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            walk_expr(lhs, f);
            walk_expr(rhs, f);
        }
        ExprKind::Unary { expr, .. } | ExprKind::Cast { expr, .. } => walk_expr(expr, f),
        ExprKind::Field { receiver, .. } | ExprKind::TupleIndex { receiver, .. } => {
            walk_expr(receiver, f);
        }
        ExprKind::CompField { receiver, index } | ExprKind::Index { receiver, index } => {
            walk_expr(receiver, f);
            walk_expr(index, f);
        }
        ExprKind::If {
            cond,
            then_block,
            else_branch,
        } => {
            walk_expr(cond, f);
            walk_block(then_block, f);
            if let Some(eb) = else_branch {
                walk_expr(eb, f);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            walk_expr(scrutinee, f);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    walk_expr(g, f);
                }
                walk_expr(&arm.body, f);
            }
        }
        ExprKind::Block(b) | ExprKind::ComptimeBlock(b) => walk_block(b, f),
        ExprKind::Range { lo, hi, .. } => {
            if let Some(l) = lo {
                walk_expr(l, f);
            }
            if let Some(h) = hi {
                walk_expr(h, f);
            }
        }
        ExprKind::Tuple(es) | ExprKind::Array(es) => {
            for inner in es {
                walk_expr(inner, f);
            }
        }
        ExprKind::StructLit { fields, .. } => {
            for fld in fields {
                walk_expr(&fld.value, f);
            }
        }
        ExprKind::Loop {
            body, decreases, ..
        } => {
            walk_block(body, f);
            if let Some(d) = decreases {
                walk_expr(d, f);
            }
        }
        ExprKind::For { iter, body, .. } => {
            walk_expr(iter, f);
            walk_block(body, f);
        }
        ExprKind::Try(inner)
        | ExprKind::Await(inner)
        | ExprKind::Raise(inner)
        | ExprKind::Panic(inner)
        | ExprKind::Comptime(inner) => walk_expr(inner, f),
        ExprKind::Scope { body, .. } => walk_block(body, f),
        ExprKind::Return(opt) => {
            if let Some(inner) = opt {
                walk_expr(inner, f);
            }
        }
        ExprKind::Break { value, .. } => {
            if let Some(inner) = value {
                walk_expr(inner, f);
            }
        }
        ExprKind::Closure(c) => walk_block(&c.body, f),
        ExprKind::Handle { recovery, body, .. } => {
            walk_expr(recovery, f);
            walk_block(body, f);
        }
        ExprKind::Spawn(s) => {
            for a in &s.args {
                walk_expr(&a.init, f);
            }
            walk_block(&s.body, f);
        }
        ExprKind::Forall { iter, body, .. } | ExprKind::Exists { iter, body, .. } => {
            walk_expr(iter, f);
            walk_expr(body, f);
        }
        ExprKind::Literal(_)
        | ExprKind::Path(_)
        | ExprKind::EffectRow(_)
        | ExprKind::Continue { .. }
        | ExprKind::Error => {}
    }
}
