//! HIR expression evaluator for the comptime-pure subset.
//!
//! [`eval_expr`] walks a typed [`HirExpr`] and reduces it to a
//! [`Value`]. This surface implements the **predicate fragment
//! plus block/conditional/bindings/bounded-iteration**: literals,
//! identifier paths that resolve to local bindings or primitive type
//! names, arithmetic / comparison / logical / bitwise operators,
//! `if` / `else`, blocks with `let` / `var` statements and (compound)
//! assignment (including index-lvalue assignment) against a
//! block-scoped [`crate::eval::env::ComptimeEnv`], calls to the
//! locked comptime built-ins
//! (`size_of`/`align_of`/`offset_of`/`target_has`), `[e1, ..., en]`
//! array construction, `receiver[index]` reads, `for pat in iter`
//! over an integer `Range` or an already-evaluated `Array`,
//! and `Path { field: e, ... }` struct-literal construction to a
//! [`Value::Record`]. `comptime` and `comptime { … }` are
//! transparent in this layer because the caller is already in a comptime
//! context. The unbounded `loop` form ([`HirExprKind::Loop`]) and
//! `break`/`continue` inside a `for`/`loop` body are evaluated too,
//! with unlabeled `break`/`continue` targeting the
//! innermost loop. Everything outside that subset (record method access,
//! `match`, `try`/`raise`/`await`, labeled `break`/`continue`) reports a
//! precise "not yet supported" diagnostic.
//!
//! Recursion depth is bounded by [`MAX_DEPTH`]; exceeding it surfaces
//! a comptime panic per `comptime.md` *A comptime block must
//! terminate*. Bounded `for`/`loop` iteration is bounded independently
//! by [`MAX_ITERATIONS`], since loop trips don't recurse the Rust call
//! stack the way nested expression evaluation does — so a bare `loop`
//! with no reachable `break` fails fast on the iteration budget rather
//! than hanging the compiler.

mod cast;
mod diag;
mod eval;
mod field;
mod structlit;

use ahash::AHashMap;
use edda_diag::Diagnostics;
use edda_intern::Interner;
use edda_span::Span;
use edda_target::TargetCfg;
use edda_types::{HirExpr, HirExprKind, TyId, TyInterner};

use crate::eval::env::ComptimeEnv;
use crate::eval::expr::cast::eval_cast;
use crate::eval::expr::diag::{push_not_supported, push_panic, variant_name};
use crate::eval::expr::eval::{
    eval_array, eval_binary, eval_block, eval_break, eval_call, eval_continue, eval_for, eval_if,
    eval_index, eval_literal, eval_loop, eval_panic, eval_path, eval_return, eval_unary,
};
use crate::eval::expr::field::eval_field;
use crate::eval::expr::structlit::eval_struct_lit;
use crate::fndecl::FnDeclLookup;
use crate::layout::TypeDeclLookup;
use crate::value::Value;

/// Maximum recursion depth the evaluator admits.
///
/// `comptime.md` requires comptime blocks to terminate; the spec
/// leaves the exact bound an implementation detail. 1024 is high
/// enough that hand-written predicates and small CRC-table-style
/// blocks never hit it, low enough that runaway recursion fails fast
/// at compile time.
pub const MAX_DEPTH: u32 = 1024;

/// Maximum total bounded `for`-loop iteration steps the
/// evaluator admits across one `EvalCx`'s lifetime.
///
/// Loop trips don't grow the Rust call stack the way nested
/// expression evaluation does, so [`MAX_DEPTH`] cannot bound a
/// pathological `for i in 0..<u64::MAX`. `100_000` comfortably covers
/// realistic comptime tables (a 256-entry CRC table with an 8-step
/// inner bit loop is 2048 steps) while still failing fast on runaway
/// iteration counts.
pub const MAX_ITERATIONS: u32 = 100_000;

/// State carried through a single comptime evaluation.
///
/// `EvalCx` borrows the type interner, target config, and string
/// interner immutably (they're shared with the typechecker) and holds
/// a mutable reference to the diagnostics take. The `depth` counter
/// guards against unbounded recursion.
pub struct EvalCx<'a> {
    /// Type-interner the [`TyId`]s in the HIR were issued by.
    pub ty_interner: &'a TyInterner,
    /// Active build target — `target_has` and pointer-width primitive
    /// layout resolve against this.
    pub target: &'a TargetCfg,
    /// String interner the path-segment / literal-string symbols were
    /// issued by.
    pub interner: &'a Interner,
    /// Diagnostics take. Diagnostics emitted during evaluation are
    /// pushed here; the function return is `None` whenever a
    /// diagnostic is pushed.
    pub diags: &'a mut Diagnostics,
    /// Optional typechecker-side §C10 resolution map: a single-segment
    /// `Path` whose span lives here resolves to the recorded `TyId`
    /// instead of being looked up by name. MIR lowering threads this
    /// in via [`crate::eval::expr::EvalCx::with_resolutions`] so
    /// `comptime size_of(MyType)` over a user type works without the
    /// evaluator re-walking the resolver.
    pub comptime_type_paths: Option<&'a AHashMap<Span, TyId>>,
    /// Optional [`TypeDeclLookup`] used by `size_of` / `align_of` for
    /// nominal types. Falls back to the no-op lookup (which yields
    /// [`crate::LayoutUnsupported::NominalLayoutDeferred`]) when
    /// absent.
    pub type_decls: Option<&'a dyn TypeDeclLookup>,
    /// Binding environment for `let` / `var` declared inside the
    /// comptime body. Owned by the context: one evaluation, one env.
    /// Block evaluation saves [`ComptimeEnv::depth`] on entry and
    /// truncates back on exit so bindings stay block-scoped.
    pub env: ComptimeEnv,
    /// Optional typechecker-side user-function call resolution map:
    /// a `Call` expression whose span lives here has
    /// its callee's `BindingId` recorded. MIR lowering threads this in
    /// via [`EvalCx::with_fn_calls`] alongside the [`FnDeclLookup`] so
    /// the evaluator can interpret the callee's body.
    pub comptime_fn_calls: Option<&'a ahash::AHashMap<Span, edda_resolve::BindingId>>,
    /// Optional [`FnDeclLookup`] resolving a callee `BindingId` to its
    /// declaration (signature + typed-HIR body). Absent means
    /// user-function calls report "not yet supported".
    pub fn_decls: Option<&'a dyn FnDeclLookup>,
    /// Value carried by an in-flight `return` unwind. The `Return` arm
    /// stores the evaluated payload here and returns `None` *without*
    /// pushing a diagnostic; the user-function call frame that entered
    /// the body takes it as the call's result. Distinguishes
    /// control-flow unwinding from genuine evaluation failure.
    pub(super) pending_return: Option<Value>,
    /// Number of user-function body frames currently on the eval
    /// stack. `return` outside any frame (`0`) is a diagnostic —
    /// mirroring the native cteval's "comptime `return` reached
    /// outside function-body context".
    pub(super) fn_call_depth: u32,
    /// Value carried by an in-flight `break` unwind. The `Break` arm
    /// stores the (optional) yielded payload here and returns `None`
    /// *without* pushing a diagnostic; the innermost `loop`/`for` driver
    /// takes it to stop iterating (a `loop` yields it as the loop's
    /// value; a `for` — a statement — discards it and yields `Unit`).
    pub(super) pending_break: Option<Value>,
    /// Set by the `Continue` arm to unwind to the innermost loop
    /// driver, which clears it and advances to the next iteration.
    /// Distinguishes a control-flow unwind from a genuine failure the
    /// same way `pending_return` does.
    pub(super) pending_continue: bool,
    /// Number of loop bodies (`loop` / `for`) currently on the eval
    /// stack. `break`/`continue` outside any loop (`0`) is a diagnostic
    /// — mirroring the native cteval's "reached outside loop context".
    pub(super) loop_depth: u32,
    /// Env-stack index below which path resolution must not reach —
    /// the innermost user-function frame's floor. Keeps a callee body
    /// from resolving caller-local bindings by name collision (the
    /// env is name-keyed, unlike the native cteval's DefId keys).
    pub(super) frame_base: usize,
    depth: u32,
    /// Total bounded `for`-loop iteration steps executed so far,
    /// checked against [`MAX_ITERATIONS`] by
    /// [`EvalCx::bump_iteration`].
    iterations: u32,
}

impl<'a> EvalCx<'a> {
    /// Construct a context at depth zero.
    pub fn new(
        ty_interner: &'a TyInterner,
        target: &'a TargetCfg,
        interner: &'a Interner,
        diags: &'a mut Diagnostics,
    ) -> Self {
        Self {
            ty_interner,
            target,
            interner,
            diags,
            comptime_type_paths: None,
            type_decls: None,
            env: ComptimeEnv::new(),
            comptime_fn_calls: None,
            fn_decls: None,
            pending_return: None,
            fn_call_depth: 0,
            pending_break: None,
            pending_continue: false,
            loop_depth: 0,
            frame_base: 0,
            depth: 0,
            iterations: 0,
        }
    }

    /// Attach a typechecker-side comptime user-function call
    /// resolution map. See
    /// [`EvalCx::comptime_fn_calls`] for the semantics.
    pub fn with_fn_calls(
        mut self,
        calls: &'a ahash::AHashMap<Span, edda_resolve::BindingId>,
    ) -> Self {
        self.comptime_fn_calls = Some(calls);
        self
    }

    /// Attach a [`FnDeclLookup`] resolving callee `BindingId`s to
    /// their declarations for comptime user-function calls.
    pub fn with_fn_decls(mut self, decls: &'a dyn FnDeclLookup) -> Self {
        self.fn_decls = Some(decls);
        self
    }

    /// Attach a typechecker-side comptime-path resolution map. See
    /// [`crate::eval::expr::EvalCx::comptime_type_paths`] for the
    /// semantics.
    pub fn with_resolutions(mut self, paths: &'a AHashMap<Span, TyId>) -> Self {
        self.comptime_type_paths = Some(paths);
        self
    }

    /// Attach a [`TypeDeclLookup`] for resolving nominal types in
    /// `size_of` / `align_of`.
    pub fn with_type_decls(mut self, decls: &'a dyn TypeDeclLookup) -> Self {
        self.type_decls = Some(decls);
        self
    }

    /// Current recursion depth. Useful for assertions in tests.
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// Register one bounded loop iteration step (`for` or `loop`).
    /// `construct` names the surface form for the diagnostic. Pushes a
    /// comptime panic and returns `None` the first time the running
    /// total exceeds [`MAX_ITERATIONS`]; every prior call in the same
    /// loop (and every enclosing loop/frame sharing this `EvalCx`)
    /// returns `Some(())`.
    pub(super) fn bump_iteration(&mut self, span: Span, construct: &str) -> Option<()> {
        self.iterations += 1;
        if self.iterations > MAX_ITERATIONS {
            push_panic(
                self.diags,
                span,
                format!("comptime `{construct}` exceeded iteration bound {MAX_ITERATIONS}"),
            );
            return None;
        }
        Some(())
    }
}

/// Evaluate a typed HIR expression to a comptime [`Value`].
///
/// On success returns the value the expression produces; on failure
/// pushes a `Diagnostic` into `cx.diags` and returns `None`. The
/// failure modes include: depth-bound exceeded, unsupported variant,
/// unresolved path, operator shape/overflow/divzero
/// problems, built-in misuse, and explicit `panic` expressions.
///
/// Some `None`s are *not* failures — they are control-flow unwinds
/// carrying no diagnostic: a `return` inside a user-function body sets
/// `cx.pending_return` (taken by the call frame that entered the body),
/// and a `break`/`continue` inside a loop sets `cx.pending_break` /
/// `cx.pending_continue` (taken by the innermost `loop`/`for` driver).
pub fn eval_expr(expr: &HirExpr, cx: &mut EvalCx<'_>) -> Option<Value> {
    cx.depth += 1;
    if cx.depth > MAX_DEPTH {
        push_panic(
            cx.diags,
            expr.span,
            format!("comptime evaluation exceeded recursion depth {MAX_DEPTH}"),
        );
        cx.depth -= 1;
        return None;
    }
    let result = match &expr.kind {
        HirExprKind::Literal(lit) => eval_literal(lit, expr.ty, expr.span, cx),
        HirExprKind::Path(path) => eval_path(path, expr.ty, expr.span, cx),
        HirExprKind::Binary { op, lhs, rhs } => {
            eval_binary(*op, lhs, rhs, expr.span, cx)
        }
        HirExprKind::Unary { op, expr: inner } => {
            eval_unary(*op, inner, expr.span, cx)
        }
        HirExprKind::Call { callee, args } => eval_call(callee, args, expr.span, cx),
        HirExprKind::If {
            cond,
            then_block,
            else_branch,
        } => eval_if(cond, then_block, else_branch.as_deref(), expr.span, cx),
        HirExprKind::Block(block) => eval_block(block, cx),
        HirExprKind::Cast {
            expr: inner,
            target_ty,
            mode,
        } => eval_cast(inner, *target_ty, *mode, expr.span, cx),
        HirExprKind::Comptime(inner) => eval_expr(inner, cx),
        HirExprKind::ComptimeBlock(block) => eval_block(block, cx),
        HirExprKind::Return(inner) => eval_return(inner.as_deref(), expr.span, cx),
        HirExprKind::Panic(msg) => eval_panic(msg, expr.span, cx),
        HirExprKind::Array(elems) => eval_array(elems, cx),
        HirExprKind::StructLit { path: _, fields } => eval_struct_lit(fields, expr.span, cx),
        HirExprKind::Field { receiver, name } => eval_field(receiver, name, expr.span, cx),
        HirExprKind::Index { receiver, index } => eval_index(receiver, index, expr.span, cx),
        HirExprKind::For {
            pat,
            iter,
            body,
            label: _,
        } => eval_for(pat, iter, body, expr.span, cx),
        HirExprKind::Loop {
            body,
            label: _,
            decreases: _,
        } => eval_loop(body, expr.span, cx),
        HirExprKind::Break { label, value } => {
            eval_break(label.as_ref(), value.as_deref(), expr.span, cx)
        }
        HirExprKind::Continue { label } => eval_continue(label.as_ref(), expr.span, cx),
        HirExprKind::Error => None,
        _ => {
            push_not_supported(cx.diags, expr.span, variant_name(&expr.kind));
            None
        }
    };
    cx.depth -= 1;
    result
}
