//! Expression, statement, and pattern walking — blocks, control flow,
//! closures, spawns, quantifiers, and the lexical-scope bookkeeping
//! that declares pattern / loop / scope bindings.

use edda_syntax::ast::{
    Block, CallArg, Expr, ExprKind, FStringPart, Ident, MatchArm, Pat, PatKind, Path as AstPath,
    Stmt, StmtKind, VariantPatPayload,
};

use crate::binding::BindingKind;

use super::{PathPos, Resolver};

impl<'a, 'i> Resolver<'a, 'i> {
    pub(super) fn walk_block(&mut self, b: &Block) {
        self.enter_scope();
        for s in &b.stmts {
            self.walk_stmt(s);
        }
        if let Some(t) = &b.trailing {
            self.walk_expr(t);
        }
        self.exit_scope();
    }

    pub(super) fn walk_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Let { pat, ty, init, .. } => {
                if let Some(i) = init {
                    self.walk_expr(i);
                }
                if let Some(t) = ty {
                    self.walk_type(t);
                }
                self.declare_pat(pat);
            }
            StmtKind::Assign { target, op: _, rhs } => {
                self.walk_expr(target);
                self.walk_expr(rhs);
            }
            StmtKind::Expr(e) => self.walk_expr(e),
        }
    }

    pub(super) fn declare_pat(&mut self, p: &Pat) {
        match &p.kind {
            PatKind::Wildcard | PatKind::Literal(_) | PatKind::Error => {}
            PatKind::Binding(ident) => {
                self.declare(BindingKind::Local, ident.name, ident.span);
            }
            PatKind::Tuple(ps) => {
                for p in ps {
                    self.declare_pat(p);
                }
            }
            PatKind::Variant { path, payload } => {
                // The leading-dot shorthand `.<variant>` parses as a
                // single-segment path naming the variant; per
                // `expressions.md` §"Pattern grammar" the type prefix
                // is implicit and resolved by the typechecker against
                // the scrutinee's ADT. Skipping path resolution here
                // prevents a spurious `unresolved path` diagnostic on
                // the shorthand form. Multi-segment paths (`Tag.a`)
                // route through the normal resolver — the head must
                // resolve to a TypeDecl binding, and the variant name
                // is then matched against the ADT by the typechecker.
                if path.segments.len() >= 2 {
                    self.resolve_path(path);
                }
                match payload {
                    VariantPatPayload::None => {}
                    VariantPatPayload::Tuple(ps) => {
                        for p in ps {
                            self.declare_pat(p);
                        }
                    }
                    VariantPatPayload::Struct(fields) => {
                        for f in fields {
                            self.declare_pat(&f.pat);
                        }
                    }
                }
            }
            PatKind::Struct { path, fields, .. } => {
                self.resolve_path(path);
                for f in fields {
                    self.declare_pat(&f.pat);
                }
            }
            PatKind::Guard { pat, cond } => {
                self.declare_pat(pat);
                self.walk_expr(cond);
            }
            // A range pattern binds no names; both bounds are literals.
            PatKind::Range { .. } => {}
            // `name @ inner` declares `name`, then recurses into the
            // sub-pattern (which may bind further names).
            PatKind::AtBinding { name, inner } => {
                self.declare(BindingKind::Local, name.name, name.span);
                self.declare_pat(inner);
            }
            // Slice elements bind element-wise; the `..name` rest
            // (when present) binds the remaining sub-slice.
            PatKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                for p in prefix {
                    self.declare_pat(p);
                }
                if let Some(Some(name)) = rest {
                    self.declare(BindingKind::Local, name.name, name.span);
                }
                for p in suffix {
                    self.declare_pat(p);
                }
            }
        }
    }

    pub(super) fn walk_expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Literal(_) | ExprKind::Error => {}
            ExprKind::FString(parts) => {
                for part in parts {
                    if let FStringPart::Slot(slot) = part {
                        self.walk_expr(slot);
                    }
                }
            }
            ExprKind::Path(p) => {
                // Expression-position value path: `head.tail` on a
                // Param/Local head is field/method access on that binding,
                // not a module-qualified
                // walk — see [`PathPos`].
                self.resolve_path_pos(p, PathPos::Value);
            }
            ExprKind::Binary { lhs, rhs, .. } => {
                self.walk_expr(lhs);
                self.walk_expr(rhs);
            }
            ExprKind::Unary { expr, .. } => self.walk_expr(expr),
            ExprKind::Call { callee, args } => self.walk_direct_call(callee, args),
            ExprKind::MethodCall { receiver, args, .. } => self.walk_call(receiver, args),
            ExprKind::Field { receiver, .. } => self.walk_expr(receiver),
            ExprKind::TupleIndex { receiver, .. } => self.walk_expr(receiver),
            ExprKind::CompField { receiver, index } => {
                // Comptime-indexed field access (D-22). Resolve the
                // receiver and the index expression (which references the
                // enclosing `comptime for` loop binding); the field name
                // itself is materialised by the pass-2 comptime expansion.
                self.walk_expr(receiver);
                self.walk_expr(index);
            }
            ExprKind::Index { receiver, index } => {
                self.walk_expr(receiver);
                self.walk_expr(index);
            }
            ExprKind::If { cond, then_block, else_branch } => self.walk_if(cond, then_block, else_branch.as_deref()),
            ExprKind::Match { scrutinee, arms } => self.walk_match(scrutinee, arms),
            ExprKind::Block(b) => self.walk_block(b),
            ExprKind::Cast { expr, ty, mode: _ } => {
                self.walk_expr(expr);
                self.walk_type(ty);
            }
            ExprKind::Range { lo, hi, .. } => {
                if let Some(lo) = lo {
                    self.walk_expr(lo);
                }
                if let Some(hi) = hi {
                    self.walk_expr(hi);
                }
            }
            ExprKind::Tuple(es) | ExprKind::Array(es) => {
                for e in es {
                    self.walk_expr(e);
                }
            }
            ExprKind::StructLit { path, fields } => {
                self.resolve_path(path);
                for f in fields {
                    self.walk_expr(&f.value);
                }
            }
            ExprKind::Loop { body, decreases, .. } => {
                if let Some(measure) = decreases {
                    // Pre-bind `box_depth` for the same reason the
                    // function-level `decreases` arm does (B-021): the
                    // termination-measure recogniser in
                    // edda-types::refine::termination::box_depth admits
                    // `box_depth(<box-typed binding>)`, so the resolver
                    // must let the predicate walker see `box_depth` as
                    // an in-scope name instead of raising
                    // `import_resolution_error`.
                    self.enter_scope();
                    let box_depth_sym = self.cx.interner.intern("box_depth");
                    self.declare(BindingKind::Local, box_depth_sym, measure.span);
                    self.walk_expr(measure);
                    self.exit_scope();
                }
                self.walk_block(body);
            }
            ExprKind::For { pat, iter, body, .. } => self.walk_for(pat, iter, body),
            ExprKind::Try(e)
            | ExprKind::Await(e)
            | ExprKind::Raise(e)
            | ExprKind::Panic(e)
            | ExprKind::Comptime(e) => self.walk_expr(e),
            ExprKind::ComptimeBlock(b) => self.walk_block(b),
            ExprKind::Scope { kind: _, name, body } => self.walk_scope(name.as_ref(), body),
            ExprKind::Return(o) => {
                if let Some(e) = o {
                    self.walk_expr(e);
                }
            }
            ExprKind::Break { value, .. } => {
                if let Some(e) = value {
                    self.walk_expr(e);
                }
            }
            ExprKind::Continue { .. } => {}
            ExprKind::EffectRow(row) => self.walk_effects(row),
            // `function(params) -> ret [with {row}] [captures {caps}] { body }`
            // closure literal (PR-B1).
            ExprKind::Closure(c) => self.walk_closure(c),
            // `handle err: T [as <binder>] -> recovery { body }` — walk
            // every sub-tree; type-position paths inside `T` resolve
            // through walk_type so edda-types::lower_type can lift the
            // handled payload into a TyId during inference. `ty` is
            // `None` for the payload-less pure-effect forms
            // (`cancellation` / `divergence`), which have nothing to
            // walk. When the optional `as <binder>` clause is present,
            // declare the binder as a fresh local visible only inside
            // `recovery` — the `body` never sees it.
            ExprKind::Handle { ty, binder, recovery, body, .. } => {
                if let Some(ty) = ty {
                    self.walk_type(ty);
                }
                if let Some(b) = binder {
                    self.enter_scope();
                    self.declare(BindingKind::Local, b.name, b.span);
                }
                self.walk_expr(recovery);
                if binder.is_some() {
                    self.exit_scope();
                }
                self.walk_block(body);
            }
            // `<scope>.spawn (take a = ..., ...)? { body }` — record the
            // scope-binder reference through resolve_path (so the same
            // single-source-of-truth resolution map carries it), walk
            // every arg's parent-scope initialiser (and optional type),
            // then walk the body in a new scope with the args declared
            // as locals. Mirror walk_scope's enter/exit discipline so
            // bindings introduced in the spawn body do not leak.
            ExprKind::Spawn(s) => self.walk_spawn(s),
            // `forall <bound> in <iter>: <body>` / `exists <bound> in <iter>: <body>`.
            // The bound name is a fresh local visible only inside `body`;
            // `iter` is walked in the enclosing scope (it may reference
            // outer bindings but not the bound name yet). The body sees
            // `bound` as a Local binding and any outer-scope names too.
            ExprKind::Forall { bound, iter, body } | ExprKind::Exists { bound, iter, body } => {
                self.walk_expr(iter);
                self.enter_scope();
                self.declare(BindingKind::Local, bound.name, bound.span);
                self.walk_expr(body);
                self.exit_scope();
            }
        }
    }

    pub(super) fn walk_spawn(&mut self, s: &edda_syntax::ast::SpawnExpr) {
        let scope_path = AstPath {
            segments: vec![s.scope_name],
            span: s.scope_name.span,
        };
        self.resolve_path(&scope_path);
        for arg in &s.args {
            if let Some(ty) = &arg.ty {
                self.walk_type(ty);
            }
            self.walk_expr(&arg.init);
        }
        self.enter_scope();
        for arg in &s.args {
            self.declare(BindingKind::Local, arg.name.name, arg.name.span);
        }
        self.walk_block(&s.body);
        self.exit_scope();
    }

    pub(super) fn walk_closure(&mut self, c: &edda_syntax::ast::Closure) {
        // Capture names reference bindings in the enclosing scope; resolve
        // them there (records the binding and marks any leaf import used)
        // before the closure's own scope is pushed.
        if let Some(captures) = &c.captures {
            for cap in captures {
                let cap_path = AstPath {
                    segments: vec![cap.name],
                    span: cap.name.span,
                };
                self.resolve_path(&cap_path);
            }
        }
        self.enter_scope();
        // Parameter `x` must be in scope before its own inline `where`
        // refinement walks; the effect row likewise sees the params it
        // names (mirrors `walk_fn`).
        for p in &c.params {
            self.declare(BindingKind::Param, p.name.name, p.name.span);
            self.walk_type(&p.ty);
        }
        self.walk_type(&c.ret);
        if let Some(row) = &c.effects {
            self.walk_effects(row);
        }
        // Captured names are body-visible locals (distinct from the outer
        // binding resolved above) so a body reference resolves to the
        // closure's captured copy rather than re-binding the outer name.
        if let Some(captures) = &c.captures {
            for cap in captures {
                self.declare(BindingKind::Local, cap.name.name, cap.name.span);
            }
        }
        self.walk_block(&c.body);
        self.exit_scope();
    }

    pub(super) fn walk_call(&mut self, callee: &Expr, args: &[CallArg]) {
        self.walk_expr(callee);
        for a in args {
            self.walk_expr(&a.expr);
        }
    }

    /// `offset_of(T, field)`'s second positional argument is a bare
    /// field-name token, not a value binding — skip walking it as an
    /// expression so `field` doesn't need to resolve against any
    /// scope (a bootstrap-side parity fix). Every other direct call
    /// walks all of its arguments as usual.
    pub(super) fn walk_direct_call(&mut self, callee: &Expr, args: &[CallArg]) {
        self.walk_expr(callee);
        let skip_field_name =
            matches!(&callee.kind, ExprKind::Path(p) if self.is_unshadowed_offset_of(p));
        for (i, a) in args.iter().enumerate() {
            if skip_field_name && i == 1 {
                continue;
            }
            self.walk_expr(&a.expr);
        }
    }

    pub(super) fn walk_if(&mut self, cond: &Expr, then_block: &Block, else_branch: Option<&Expr>) {
        self.walk_expr(cond);
        self.walk_block(then_block);
        if let Some(eb) = else_branch {
            self.walk_expr(eb);
        }
    }

    pub(super) fn walk_match(&mut self, scrutinee: &Expr, arms: &[MatchArm]) {
        self.walk_expr(scrutinee);
        for a in arms {
            self.enter_scope();
            self.declare_pat(&a.pat);
            if let Some(g) = &a.guard {
                self.walk_expr(g);
            }
            self.walk_expr(&a.body);
            self.exit_scope();
        }
    }

    pub(super) fn walk_for(&mut self, pat: &Pat, iter: &Expr, body: &Block) {
        self.walk_expr(iter);
        self.enter_scope();
        self.declare_pat(pat);
        self.walk_block(body);
        self.exit_scope();
    }

    pub(super) fn walk_scope(&mut self, name: Option<&Ident>, body: &Block) {
        self.enter_scope();
        if let Some(name) = name {
            self.declare(BindingKind::Local, name.name, name.span);
        }
        self.walk_block(body);
        self.exit_scope();
    }
}
