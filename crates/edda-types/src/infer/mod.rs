//! Bidirectional inference dispatchers.
//!
//! Implements the synthesis (`Γ ⊢ e ⇒ T`) and checking (`Γ ⊢ e ⇐ T`)
//! judgments from `docs/types/inference-rules.md §1` over the HIR. The
//! pass walks an already-lowered [`HirExpr`] tree (the AST → HIR lowering output)
//! and mutates each value-producing node's `ty: TyId` field in place.
//!
//! # Module layout
//!
//! - [`env`]: [`TyEnv`] — lexical type-and-state environment.
//! - [`mode`]: [`BindingState`] lattice + the §4 mode-discipline
//!   algorithm (call-arg transitions, loop re-entry, function exit,
//!   the single-segment-path-binding helper).
//! - [`pat`]: pattern-binding pass (the seven `bind_*` helpers).
//! - [`path`]: T-Var path resolution + the literal kernel.
//! - [`fn_body`]: [`check_fn_body`] — function-body typing entry that
//!   ties the dispatcher to the §4 exit rule.
//! - Per-feature dispatchers: [`call`], [`comp`], [`ctrl`], [`field`],
//!   [`lit`], [`op`], [`struct_lit`]. Each owns one HirExprKind family
//!   plus its associated tests.
//!
//! # Coverage
//!
//! Literals and bindings: T-IntLit*, T-FloatLit-Default / T-BoolLit /
//! T-StrLit / T-FStringLit / T-UnitLit, T-Var, T-Block-Synth, M-Let typing.
//!
//! Operators, control flow, composites: binary / unary arithmetic,
//! comparison, logical, bitwise, shift; `if` / `match` with branch
//! joining; `loop`; the diverging originators; tuple construction;
//! primitive `as` casts; slice indexing.
//!
//! [`TyCx`](crate::cx::TyCx), T-FunCall (call), T-FieldAccess (field),
//! T-StructLit (struct_lit), complex pattern destructuring (pat).
//!
//! The §4 mode tracker: [`BindingState`] / [`TyEnv`] lattice methods,
//! M-Let / M-Var-Init / M-Var-Uninit transitions in [`infer_stmt`],
//! M-Mutable-Arg / M-Take-Arg / M-Init-Arg call-arg transitions, branch
//! GLB merging in `synth_if` / `synth_match`, loop re-entry checks in
//! `synth_loop`, and the function-exit rule in [`check_fn_body`].
//!
//! Effect tracking: the `EffectAcc` accumulator, `Raise` / `Panic`
//! originators contributing entries, `T-FunCall` row union, and
//! [`check_fn_body`] enforcing `effect_row_mismatch` containment.
//!
//! `?` propagation per `effect-tracking.md §3`: [`synth_try`] verifies
//! `inner`'s row contains at least one `err: E` entry (emitting `no
//! error effect to propagate` when none do) and passes the row through;
//! declared-row containment is enforced at function exit by the
//! row-containment check.
//!
//! Call-site capability substitution per `effect-tracking.md §2`:
//! [`call::synth_call`] rewrites each callee `Capability(p)` to
//! `Capability(q)` where `q` is the caller's parameter (or derivation
//! root) flowing into the argument bound to `p`. Pure entries pass
//! through unchanged.
//!
//! §5 per-field tracker: [`BindingState::PartialInit(F)`] tracks the
//! set of currently-Valid fields for a partially-initialised binding;
//! field-projection call args route through the §5 M-Take-Field /
//! M-Init-Field / M-Mutable-Field transitions; M-Field-Assign
//! re-validates a field and promotes the binding when all fields
//! rejoin; [`field::synth_field`] gates reads on per-field state; the
//! function-exit rule reports per-field non-Valid bindings.
//!
//! Implicit `Range` spec invocation per `inference-rules.md §3`:
//! [`comp::synth_range`] infers the element type (both-literal default
//! `i64`; mixed checks the literal against the typed endpoint;
//! both-typed must agree) and registers a [`crate::ImplicitSpecRequest`]
//! on `ic.implicit_specs`. The synthesised value type stays the error
//! sentinel until `edda-codegen` materialises the `Range_<T>` nominal.
//! An `Option` companion and the refine handoff (deferred pending an
//! `edda-refine` z3-sys feature-gate) round out this area once their
//! dependencies are ready.
//!
//! Comptime-purity at call sites per `inference-rules.md §1a.6`: the
//! [`InferCx::in_comptime_context`] flag toggles inside `Comptime` /
//! `ComptimeBlock` arms; [`call::synth_call`] enforces the callee row
//! ⊆ `{panic, yield: T}` row-side approximation of P-CompTimePure,
//! emitting `comptime_purity_loss` at the call site.
//!
//! `scope(exec)` / `<scope>.spawn { body }` / `.await` frontend typing
//! per `corpus/edda-codex/language/05-concurrency-coherence.md` §2:
//! `ScopeKind::Exec` type-checks its body as a nested scope (wrong-arity
//! / type-mismatch calls inside the region are rejected at `edda
//! check`); [`spawn::synth_spawn`] type-checks each explicit `take`-arg
//! initialiser in the parent scope, transitions its source binding to
//! `Consumed` (mirroring an ordinary `f(take x)` call arg), and binds
//! fresh locals for the body. The frontend types tasks *transparently*:
//! `.spawn` synthesises the body's return type `T` (not a `Task_<T>`
//! nominal — the linear handle is a MIR-level notion; `Spawn::dest` is
//! `HeapPtr`-typed independently of this) and `.await` synthesises its
//! operand and passes that `T` through, giving MIR's `Await::dest` its
//! real semantic result type. This mirrors the native compiler's
//! `infer_spawn_ty` / `task_await` design — including registering no
//! implicit-spec request for `Task`, unlike the `Range` precedent
//! above: no HIR node ever binds a `Task_<T>` nominal, so materialising
//! one is both source-unreachable and, when the active package's import
//! closure doesn't reach `std.task`, breaks codegen-root collection
//! outright. Deferred to follow-up issues: `cancellation` discharge
//! semantics (`.await`'s row contribution is described below; a `handle
//! cancellation -> ...` discharge form is still open) and the
//! linear-consumption discipline on the transparent task value.
//!
//! The mandatory `Executor`-in-row check per §2.2 (*Mandatory
//! `Executor` capability*): `ScopeKind::Exec` verifies the enclosing
//! function's declared row carries a bare capability entry whose bound
//! parameter type is `Executor`, emitting
//! [`DiagnosticClass::ExecutorMissingInRow`] at the scope's span when
//! absent. The row-spelling question the codex flagged (`exec:
//! Executor` vs. a bare `exec` capability entry) resolved to the
//! bare-identifier form: every other worked example in
//! `02-modes-effects-refinements.md` and the real
//! `edda-xlibs/httpserver` consumer spell it as a bare `exec` entry
//! naming an `exec: Executor` parameter, matching CLAUDE.md's general
//! capability rule; the three `05-concurrency-coherence.md` worked
//! examples that wrote `exec: Executor` as a row entry (with no
//! matching parameter) were a documentation bug, since corrected.
//!
//! The "no `mutable` crosses the spawn boundary" rule per §2.2:
//! [`env::TyEnv::restrict_mutability`] forces every already-open
//! (parent-scope) binding immutable before [`spawn::synth_spawn`] walks
//! the body, so a `mutable` / `init` call-arg referencing an enclosing
//! binding trips [`mode::transitions::reject_immutable_borrow`] exactly
//! as it would for a `let` local; [`env::TyEnv::restore_mutability`]
//! lifts the restriction once the body scope closes. The restriction is
//! scoped to frames open *before* the body's own child scope, so a
//! spawn's `take`-arg locals (bound inside that child scope) keep their
//! normal mutability — `mutate(mutable owned)` on a fresh `take owned =
//! clone(shared)` binding still checks cleanly.
//!
//! `PureEffect::Cancellation` (`crate::effect`) is wired through every
//! exhaustive match over `PureEffect` (row rendering, MIR
//! effect/register/fn-ptr-sig lowering, the §7 stable effect-row
//! whitelist, which explicitly rejects it alongside `nondet` per that
//! module's doc comment). The `HirExprKind::Await` arm pushes
//! `Pure(Cancellation)` onto the accumulator, so
//! `05-concurrency-coherence.md` §2.2's "`await`'s row is
//! `{cancellation}`" is enforced by the row-containment check.
//! `cancellation` is also spellable in a hand-written `with { ... }`
//! row (`crate::lower::row`), mirroring `divergence`. Still deferred:
//! `TaskMethod::CancelAndAwait`'s own row contribution (blocked on
//! `TaskMethod` landing at all) and a `handle cancellation -> ...`
//! discharge form (handlers admit only `err: T` so far).
//!
//! `scope(exec)` absorption of `cancellation`: per
//! `05-concurrency-coherence.md` §2.2 (*"must be either handled with
//! `handle cancellation -> ...` or absorbed by the enclosing
//! `scope(exec)`"*), a `Pure(Cancellation)` entry pushed by an `.await`
//! lexically inside a `ScopeKind::Exec` body is discharged at that
//! scope's closing brace — mirroring `synth_handle`'s
//! checkpoint/`discharge_since` idiom — so it never reaches the
//! enclosing function's declared-row check. Without this, every
//! `.await` directly inside its own spawning `scope(exec)` would force
//! the enclosing function to redundantly declare `cancellation` even
//! though structured concurrency already guarantees the scope cannot
//! exit while its children run. `handle cancellation -> ...` is a
//! *distinct*, still entirely unimplemented mechanism (`synth_handle`
//! still rejects any non-`err` effect label): per the codex worked
//! example, the handler runs a cleanup expression and then re-raises
//! cancellation upward — it is not a discharge form the way `handle
//! err: T` is, so a function using it still declares `cancellation` in
//! its own row. This scope-absorption path is orthogonal and does not
//! depend on that mechanism landing.
//!
//! `PureEffect::Nondet` (`crate::effect`), the last of the six locked
//! pure-effect kinds, is wired through every exhaustive match over
//! `PureEffect` (row rendering, MIR effect/register/fn-ptr-sig
//! lowering, the §7 stable effect-row whitelist, which rejects it
//! alongside `cancellation`). `nondet` is spellable in a hand-written
//! `with { ... }` row (`crate::lower::row`) and lowers to a
//! verification-only effect with no ABI slot, mirroring `divergence` /
//! `cancellation` — previously it fell through `lower::row`'s catch-all
//! and was misclassified as a bare `Capability("nondet")`, threading a
//! vestigial (always-ignored) opaque `ptr` slot through
//! `std.math.random`'s ambient family; `edda-rt`'s ambient externs
//! dropped their matching `_nondet` slot. Still deferred: originating
//! `Pure(Nondet)` from a `scope(exec)` body's `group.race` /
//! `group.any` (blocked on those primitives landing) and from ambient
//! `Random` draws inferred at the call site (the `std.math.random`
//! `extern` functions already declare `with {rng, nondet}` explicitly).
//!
//! # Side effects
//!
//! [`synth_expr`] / [`check_expr`] / [`synth_block`] / [`infer_stmt`]
//! all mutate the input HIR in place. The mutation is the assignment
//! `node.ty = <inferred-or-checked-id>`. Sub-trees recurse first; a
//! parent's `ty` is computed from its already-typed children.

mod block;
mod call;
mod closure;
mod comp;
mod comptime_call;
mod ctrl;
mod cx;
mod effect;
mod env;
mod field;
mod fn_body;
mod for_loop;
mod lit;
mod method;
mod mode;
mod op;
mod pat;
mod path;
mod primitive_static_call;
mod row_acc;
mod scc;
mod spawn;
mod struct_lit;

pub(crate) use self::block::synth_block;
pub(crate) use self::cx::InferCx;
pub(crate) use self::env::TyEnv;
pub(crate) use self::mode::BindingState;
pub(crate) use self::scc::{SccMap, build_scc_map};

pub(crate) use self::fn_body::check_fn_body;

use self::block::{check_array, check_block, check_tuple};

use edda_intern::Symbol;
use edda_span::Span;

use crate::effect::{EffectEntry, PureEffect};
use crate::hir::{HirBlock, HirExpr, HirExprKind, HirPatKind, HirStmt, HirStmtKind};
use crate::lower::LowerCx;
use crate::prim::Primitive;
use crate::ty::TyId;

use self::pat::bind_pat_with_state_mut;
use self::path::{check_literal, check_synth_against_expected, synth_literal, synth_path};

use edda_diag::DiagnosticClass;

use crate::{CapabilityType, TyKind};


/// Synthesis judgment: `Γ ⊢ e ⇒ T`.
///
/// Walks `expr`, sets `expr.ty` to the synthesised type, and returns
/// the same [`TyId`] for caller convenience. Sub-expressions are
/// recursed into and have their own `ty` fields filled along the way.
///
/// Unhandled variants leave `expr.ty` as [`TyInterner::error`] without
/// emitting a diagnostic. Rules are layered on top variant-by-variant
/// as coverage grows. `scope(exec)` / `.spawn` / `.await` are handled;
/// `.spawn` and `.await` synthesise the task's semantic
/// result type `T` — see the module
/// doc.
pub(crate) fn synth_expr(env: &mut TyEnv, expr: &mut HirExpr, ic: &mut InferCx<'_>) -> TyId {
    let span = expr.span;
    let ty = match &mut expr.kind {
        HirExprKind::Literal(lit) => synth_literal(lit, ic),
        HirExprKind::FString(parts) => synth_fstring(env, parts, ic),
        HirExprKind::Path(path) => synth_path(path, env, ic, span),
        HirExprKind::Block(b) => synth_block(env, b, ic),
        HirExprKind::Binary { op, lhs, rhs } => op::synth_binary(env, *op, lhs, rhs, ic, span),
        HirExprKind::Unary { op, expr: operand } => op::synth_unary(env, *op, operand, ic, span),
        HirExprKind::If {
            cond,
            then_block,
            else_branch,
        } => ctrl::synth_if(env, cond, then_block, else_branch.as_deref_mut(), ic, span),
        HirExprKind::Match { scrutinee, arms } => {
            ctrl::synth_match(env, scrutinee, arms, ic, span)
        }
        HirExprKind::Loop {
            body, decreases, ..
        } => ctrl::synth_loop(env, body, decreases.as_deref_mut(), ic),
        HirExprKind::For {
            pat, iter, body, ..
        } => for_loop::synth_for(env, pat, iter, body, ic, span),
        HirExprKind::Return(opt) => ctrl::synth_return(env, opt.as_deref_mut(), ic),
        HirExprKind::Break { value, .. } => ctrl::synth_divergent(env, value.as_deref_mut(), ic),
        HirExprKind::Continue { .. } => ic.lower.ty_interner.prim(Primitive::Never),
        HirExprKind::Raise(inner) => ctrl::synth_raise(env, inner, ic),
        HirExprKind::Panic(inner) => ctrl::synth_panic(env, inner, ic),
        HirExprKind::Try(inner) => effect::synth_try(env, inner, ic, span),
        HirExprKind::Tuple(elems) => comp::synth_tuple(env, elems, ic, span),
        HirExprKind::Array(elems) => comp::synth_array(env, elems, ic, span),
        HirExprKind::Cast {
            expr: operand,
            target_ty,
            mode,
        } => comp::synth_cast(env, operand, *target_ty, *mode, ic, span),
        HirExprKind::Index { receiver, index } => comp::synth_index(env, receiver, index, ic, span),
        HirExprKind::Range { lo, hi, kind: _ } => comp::synth_range(env, lo, hi, ic, span),
        HirExprKind::Comptime(inner) => comp::synth_comptime(env, inner, ic),
        HirExprKind::ComptimeBlock(block) => comp::synth_comptime_block(env, block, ic),
        HirExprKind::Call { callee, args } => call::synth_call(env, callee, args, ic, span),
        HirExprKind::Field { receiver, name } => {
            field::synth_field(env, receiver, *name, ic, span)
        }
        HirExprKind::TupleIndex { receiver, index } => {
            comp::synth_tuple_index(env, receiver, *index, ic, span)
        }
        HirExprKind::StructLit { path, fields } => {
            struct_lit::synth_struct_lit(env, path, fields, ic, span)
        }
        HirExprKind::Handle {
            effect,
            handled_ty,
            binder,
            recovery,
            body,
        } => effect::synth_handle(env, *effect, *handled_ty, *binder, recovery, body, ic, span),
        HirExprKind::MethodCall {
            receiver,
            name,
            args,
        } => method::synth_method_call(env, receiver, *name, args, ic, span),
        HirExprKind::Closure(closure) => closure::synth_closure(env, closure, ic),
        // `scope(coherence) { body }` is observationally atomic but its
        // body types exactly like a bare block — the value at the closing
        // brace commits. Without this descent every expression inside the
        // region keeps the `error()` sentinel HIR lowering seeds, which
        // then surfaces at MIR lowering (e.g. a `total = total + 1` on an
        // outer primitive `var` fails with `BinOp on non-primitive
        // operand`).
        //
        // `scope(exec) group { body }` types its body the
        // same way — a nested scope, not a fresh isolated env, since
        // spawn bodies admit implicit read-only capture of enclosing
        // bindings (unlike `HirExprKind::Closure`'s mandatory-capture
        // discipline). `name`, when present, binds a placeholder so an
        // incidental bare reference to it (outside `.spawn`, which
        // carries `scope_name` directly rather than looking it up in
        // `env`) doesn't spuriously read as undefined. The mandatory
        // `Executor`-in-row check runs ahead of the body walk. Every
        // `Pure(Cancellation)` entry pushed while walking the body is
        // discharged here —
        // structured concurrency already guarantees this scope cannot
        // exit while its children run, so any `.await` lexically inside
        // it is "absorbed by the enclosing `scope(exec)`" per
        // `05-concurrency-coherence.md` §2.2 and never needs to reach
        // the enclosing function's declared row.
        HirExprKind::Scope { kind, name, body } => match kind {
            edda_syntax::ast::ScopeKind::Coherence => synth_block(env, body, ic),
            edda_syntax::ast::ScopeKind::Exec => {
                if !declared_row_has_capability(ic, env, CapabilityType::Executor) {
                    let declared_rendered = ic
                        .declared_row
                        .display(ic.lower.interner, ic.lower.ty_interner)
                        .to_string();
                    ic.emit_diagnostic(
                        DiagnosticClass::ExecutorMissingInRow,
                        span,
                        format!(
                            "`scope(exec)` requires the enclosing function's \
                             effect row to declare an `Executor` capability \
                             (a bare capability entry naming an \
                             `Executor`-typed parameter); declared row is \
                             `{declared_rendered}`",
                        ),
                    );
                }
                env.enter_scope();
                if let Some(name) = name {
                    env.bind(name.name, ic.ty_error());
                }
                let checkpoint = ic.row.checkpoint();
                let ty = synth_block(env, body, ic);
                ic.row
                    .discharge_since(checkpoint, &EffectEntry::Pure(PureEffect::Cancellation));
                env.exit_scope();
                ty
            }
        },
        // `<scope>.spawn { body }` — see `spawn::synth_spawn`.
        HirExprKind::Spawn(spawn) => spawn::synth_spawn(env, spawn, ic),
        // `expr.await` —
        // the join yields the task's semantic result type `T`. The
        // frontend types tasks transparently (`.spawn` synthesises `T`,
        // not a `Task_<T>` nominal — see `spawn::synth_spawn`), so the
        // operand's type IS `T` and passes through; MIR's `Await::dest`
        // lowers from this type per its terminator invariant. Per
        // `05-concurrency-coherence.md` §2.2 `await`'s row is
        // `{cancellation}`:
        // pushes `Pure(Cancellation)` onto the accumulator so the
        // enclosing function's declared row must admit it, exactly as
        // `raise` pushes `Pure(Err(T))`.
        HirExprKind::Await(inner) => {
            let ty = synth_expr(env, inner, ic);
            ic.push_effect_entry(EffectEntry::Pure(PureEffect::Cancellation));
            ty
        }
        // Every other variant is deferred to follow-up work.
        _ => ic.ty_error(),
    };
    expr.ty = ty;
    ty
}

/// `true` iff `ic.declared_row` carries a bare capability entry whose
/// bound parameter (resolved through `env`) has capability type `cap`.
///
/// Capability row entries name a parameter [`edda_intern::Symbol`], not
/// a capability type directly (`effect-tracking.md §2`), so recovering
/// the type requires the enclosing function's still-open `env` frame —
/// which callers of `synth_expr` guarantee is in scope for every
/// program point inside the function body (`check_fn_body`'s
/// caller binds parameters before walking).
fn declared_row_has_capability(ic: &InferCx<'_>, env: &TyEnv, cap: CapabilityType) -> bool {
    ic.declared_row.entries().iter().any(|entry| {
        let EffectEntry::Capability(sym) = entry else {
            return false;
        };
        let Some(ty_id) = env.lookup(*sym) else {
            return false;
        };
        matches!(ic.lower.ty_interner.kind(ty_id), TyKind::Capability(found) if *found == cap)
    })
}

/// Checking judgment: `Γ ⊢ e ⇐ T`.
///
/// Walks `expr` against `expected`. On success, sets `expr.ty =
/// expected`. On failure (range mismatch, structural mismatch, etc.),
/// sets `expr.ty = Error` and emits a
/// [`DiagnosticClass::TypecheckError`].
///
/// Form-specific checking arms propagate `expected` into the
/// sub-expressions that influence the leaf types:
///
/// * `Binary` arithmetic / bitwise / shift checks both operands
///   against `expected` so `return 1 + 2 * 3` with `-> i32` narrows the
///   literals — without this, the MIR emitter's
///   `non-lowerable-local` guard fires on the binary temps because the
///   un-narrowed result type cascades to the `Error` sentinel.
/// * `Unary::Neg` / `Unary::BitNot` propagate `expected` into the
///   operand.
/// * `If` / `Match` push `expected` into every branch / arm body.
/// * `Block` checks the trailing expression against `expected`.
/// * `Tuple` destructures `expected` and checks each element when the
///   expected type is structurally a tuple of matching arity.
///
/// Every other variant (`Path`, `Call`, `Cast`, `Field`, `StructLit`,
/// `Index`, etc.) routes through T-Synth-Check: synthesise the
/// expression then bridge to `expected` via
/// [`check_synth_against_expected`]. This keeps the fallback safe —
/// the historical `_ => ic.ty_error()` arm silently dropped sub-walks
/// and produced `Error` for every non-Literal/Path/Block expression,
/// which is precisely the source of the "non-lowerable-local
/// of type Never" emitter error for `return 1 + 2 * 3`.
/// Synthesise an `f"...{expr}..."` interpolated string: type-check each
/// interpolation slot and yield `String`.
fn synth_fstring(
    env: &mut TyEnv,
    parts: &mut [crate::hir::HirFStringPart],
    ic: &mut InferCx<'_>,
) -> TyId {
    for part in parts.iter_mut() {
        if let crate::hir::HirFStringPart::Slot(slot) = part {
            let _ = synth_expr(env, slot, ic);
        }
    }
    ic.lower.ty_interner.prim(Primitive::String)
}

pub(crate) fn check_expr(env: &mut TyEnv, expr: &mut HirExpr, expected: TyId, ic: &mut InferCx<'_>) {
    let span = expr.span;
    let result = match &mut expr.kind {
        HirExprKind::Literal(lit) => check_literal(lit, expected, ic, span),
        HirExprKind::Path(_) => {
            let synth = synth_expr(env, expr, ic);
            check_synth_against_expected(synth, expected, ic, span)
        }
        HirExprKind::Block(b) => check_block(env, b, expected, ic),
        HirExprKind::Binary { op, lhs, rhs } => {
            op::check_binary(env, *op, lhs, rhs, expected, ic, span)
        }
        HirExprKind::Unary { op, expr: operand } => {
            op::check_unary(env, *op, operand, expected, ic, span)
        }
        HirExprKind::If {
            cond,
            then_block,
            else_branch,
        } => ctrl::check_if(env, cond, then_block, else_branch.as_deref_mut(), expected, ic, span),
        HirExprKind::Match { scrutinee, arms } => {
            ctrl::check_match(env, scrutinee, arms, expected, ic, span)
        }
        HirExprKind::Tuple(elems) => check_tuple(env, elems, expected, ic, span),
        HirExprKind::Array(elems) => check_array(env, elems, expected, ic, span),
        // Diverging originators: synth gives `Never`, which is
        // admissible at any `expected` via `check_synth_against_expected`.
        // Routing through `synth_expr` makes sure the originator's row
        // contributions (Raise / Panic), the divergence-state updates,
        // and any sub-expression diagnostics still fire.
        HirExprKind::Return(_)
        | HirExprKind::Break { .. }
        | HirExprKind::Continue { .. }
        | HirExprKind::Raise(_)
        | HirExprKind::Panic(_)
        | HirExprKind::Loop { .. } => {
            let synth = synth_expr(env, expr, ic);
            check_synth_against_expected(synth, expected, ic, span)
        }
        // Default fallback: T-Synth-Check. Covers Call, Cast, Field,
        // StructLit, Index, Range, Try, Comptime, ComptimeBlock,
        // MethodCall, etc. — every form whose synth result type does
        // not depend on `expected`. Routing here (rather than dropping
        // to the historical `_ => ic.ty_error()` arm) ensures
        // sub-expression `ty` fields are populated and emission can
        // proceed without spurious `Error`/`Never` placeholders.
        _ => {
            let synth = synth_expr(env, expr, ic);
            check_synth_against_expected(synth, expected, ic, span)
        }
    };
    expr.ty = result;
}


/// Resolve the type of a field-projection assign target `x.f`. This
/// bypasses [`synth_expr`] for the LHS so the §5 per-field tracker
/// can transition the field without [`synth_field`]'s read-gate
/// firing on an uninitialised field. Returns the error sentinel if
/// `binding` is unknown, the receiver isn't a product type, or the
/// field isn't declared.
fn field_type(
    env: &TyEnv,
    binding: edda_intern::Symbol,
    target: &HirExpr,
    ic: &InferCx<'_>,
) -> TyId {
    let HirExprKind::Field { name, .. } = &target.kind else {
        return ic.ty_error();
    };
    let Some(binding_ty) = env.lookup(binding) else {
        return ic.ty_error();
    };
    let kind = ic.lower.ty_interner.kind(binding_ty);
    let crate::ty::TyKind::Nominal(binding_id) = kind else {
        return ic.ty_error();
    };
    let Some(decl) = ic.ty_cx.type_decl(*binding_id) else {
        return ic.ty_error();
    };
    match &decl.kind {
        crate::cx::TypeDeclShape::Product { fields } => fields
            .iter()
            .find(|f| f.name == name.name)
            .map(|f| f.ty)
            .unwrap_or_else(|| ic.ty_error()),
        crate::cx::TypeDeclShape::Sum { .. } => ic.ty_error(),
    }
}

/// Resolve the type of a tuple element assignment target `x.(index)` —
/// the positional analogue of [`field_type`] for tuple staged-init.
/// Looks the binding's tuple type up in `env` and reads
/// the element at `index` without routing through [`synth_expr`], so a
/// `Uninit` / `PartialInit` tuple binding is not falsely diagnosed as a
/// read on the assignment LHS. Returns the error sentinel if the binding
/// is unknown, not a tuple, or `index` is out of range.
fn tuple_element_type(
    env: &TyEnv,
    binding: edda_intern::Symbol,
    index: u32,
    ic: &InferCx<'_>,
) -> TyId {
    let Some(binding_ty) = env.lookup(binding) else {
        return ic.ty_error();
    };
    let crate::ty::TyKind::Tuple(elems) = ic.lower.ty_interner.kind(binding_ty) else {
        return ic.ty_error();
    };
    elems
        .get(index as usize)
        .copied()
        .unwrap_or_else(|| ic.ty_error())
}

/// Type-check a single [`HirStmt`], updating `env` with any new
/// bindings introduced by `let` / `var`.
///
/// - `Let` with an annotation `T`: check the initialiser against `T`
///   and bind every name in the pattern at `T`.
/// - `Let` without an annotation but with an initialiser: synthesise
///   the initialiser, bind every name in the pattern at the
///   synthesised type.
/// - `Let` with neither (`var x: T` is admitted only when `T` is
///   present per `ast::StmtKind::Let` invariants): bind at `T`.
/// - `Assign` / `Expr`: synthesise sub-expressions for their `ty`
///   side-effect; no environment update.
pub(crate) fn infer_stmt(env: &mut TyEnv, stmt: &mut HirStmt, ic: &mut InferCx<'_>) {
    match &mut stmt.kind {
        HirStmtKind::Let {
            ty,
            init,
            pat,
            mutability,
        } => {
            // A `let` binding is immutable — the mode checker rejects a
            // later `mutable`/`init` borrow of it (which the backend would
            // lower as a byval copy, silently dropping the write).
            // `var` and `uninit` are
            // mutable: `uninit x` is precisely the binding an `init x`
            // out-param call initialises in place.
            let binding_mutable =
                !matches!(*mutability, edda_syntax::ast::BindingMode::Immutable);
            // Pre-scan the initializer for a method-style capability call before
            // the match consumes `init`. `init.as_ref()` is a shared borrow that
            // ends before the match moves `init` into its scrutinee tuple.
            let cap_alias_root = init.as_ref()
                .and_then(|i| call::capability_source_of_call(i));
            let (binding_ty, init_state) = match (ty, init) {
                (Some(t), Some(i)) => {
                    let target = *t;
                    check_expr(env, i, target, ic);
                    // `let x = y` moves a bare `linear` `y` into `x` —
                    // consume the source so it is not swept as a leak.
                    mode::consume_moved_linear(env, i, ic);
                    (target, BindingState::Valid)
                }
                (None, Some(i)) => {
                    let synth = synth_expr(env, i, ic);
                    mode::consume_moved_linear(env, i, ic);
                    (synth, BindingState::Valid)
                }
                // `var x: T` with no initialiser is the only path that
                // produces an uninitialised binding per §4.
                (Some(t), None) => (*t, BindingState::Uninit),
                (None, None) => (ic.ty_error(), BindingState::Valid),
            };
            bind_pat_with_state_mut(env, pat, binding_ty, init_state, binding_mutable, ic);
            // Capability alias: record `let mono = clock.monotonic()` → mono aliases clock.
            // This lets `translate_callee_entry` resolve `Capability(mono)` back to
            // `Capability(clock)` when `mono` is passed as a capability argument.
            // Resolve the receiver root through the map FIRST so chained
            // derivations (`let grandchild = child.fork()` where `child`
            // already aliases `allocator`) store the row-resident root —
            // alias readers do a single-hop `get`, so a chained entry
            // would surface `performs effect `child``.
            if let (Some(root_sym), HirPatKind::Binding(ident)) = (cap_alias_root, &pat.kind) {
                let resolved_root = ic
                    .capability_aliases
                    .get(&root_sym)
                    .copied()
                    .unwrap_or(root_sym);
                ic.capability_aliases.insert(ident.name, resolved_root);
            }
        }
        HirStmtKind::Assign { target, op: _, rhs } => {
            // Per §4's `x = e` row, assignment requires `x` to be
            // `Uninit` or `Valid` and transitions it to `Valid`. The
            // §5 form `x.f = e` is the per-field analogue —
            // M-Field-Assign keeps `f` Valid and may promote `x` from
            // `PartialInit(F)` to `Valid` when `F ∪ {f} = fields(T)`.
            // Assignment-LHS use does not count as a *read* (the §4
            // pre-state requirements differ), so we look up the
            // target's type and state directly rather than routing
            // through [`synth_expr`] (which would treat a `Uninit`
            // LHS as a read and falsely diagnose).
            let field_target = mode::field_projection_binding(target);
            // Tuple staged-init: `out.(i) = e` is the
            // positional analogue of `x.f = e` — element `i` plays the
            // role a named field does for a record.
            let tuple_target = mode::tuple_index_binding(target);
            let assign_target = mode::path_binding(target);
            let target_ty = if let Some((binding, _)) = field_target {
                // For `x.f = e`, target type is the field's type.
                // We don't synth_expr the LHS (that would treat
                // the field as a *read* and trigger the §5 read
                // gate). Instead, look up the binding's TyId,
                // resolve to a product type, find the field.
                //
                // We must still populate the receiver sub-expression's
                // `ty`: MIR lowering's `resolve_assign_target` consults
                // `receiver.ty` (via `resolve_nominal_adt`) to find the
                // ADT for the field projection. If we leave it as the
                // error sentinel, the resolver silently bails and the
                // entire field-projected assignment statement is
                // dropped from MIR.
                if let HirExprKind::Field { receiver, .. } = &mut target.kind
                    && let Some(binding_ty) = env.lookup(binding)
                {
                    receiver.ty = binding_ty;
                }
                field_type(env, binding, target, ic)
            } else if let Some((binding, index)) = tuple_target {
                // For `out.(i) = e`, target type is the tuple's element
                // type. As with the field case, we bypass synth_expr
                // (an `Uninit` / `PartialInit` tuple binding must not be
                // read-gated on the assignment LHS) and stamp the
                // receiver's `ty` so MIR's `resolve_place` finds the
                // tuple type for the element projection.
                if let HirExprKind::TupleIndex { receiver, .. } = &mut target.kind
                    && let Some(binding_ty) = env.lookup(binding)
                {
                    receiver.ty = binding_ty;
                }
                tuple_element_type(env, binding, index, ic)
            } else if let Some(sym) = assign_target {
                env.lookup(sym).unwrap_or_else(|| {
                    // Path doesn't resolve to a binding — defer to
                    // synth_expr so it emits the standard
                    // "cannot find binding" diagnostic.
                    synth_expr(env, target, ic)
                })
            } else {
                // Non-path LHS (e.g. `xs[i] = ...`) — index assign is
                // handled in a future wave; for now run normal
                // synth_expr so the sub-expression types check.
                synth_expr(env, target, ic)
            };
            // Stamp the target's HIR `ty` even though we bypassed synth_expr.
            target.ty = target_ty;
            let pre_state = assign_target.and_then(|sym| env.lookup_state(sym));
            check_expr(env, rhs, target_ty, ic);
            // `x = y` moves a bare `linear` `y` into `x` — consume the
            // source so it is not swept as a leak.
            mode::consume_moved_linear(env, rhs, ic);
            if let Some((binding, field)) = field_target {
                // M-Field-Assign: field becomes Valid; binding promotes
                // to Valid if every field is now Valid.
                let _ = mode::apply_field_assign_transition(env, binding, field, ic);
            } else if let Some((binding, index)) = tuple_target {
                // Tuple element analogue of M-Field-Assign: element `i`
                // becomes Valid; the binding promotes to Valid once every
                // element is initialised. The element key is
                // the interned decimal index, matching `type_field_set`'s
                // tuple keying.
                let field_sym = ic.lower.interner.intern(&index.to_string());
                let _ = mode::apply_field_assign_transition(env, binding, field_sym, ic);
            } else if let Some(sym) = assign_target {
                match pre_state {
                    Some(BindingState::Consumed) => {
                        let name = ic.lower.interner.resolve(sym).to_string();
                        ic.emit_typecheck_error(
                            target.span,
                            format!("cannot assign to `{name}`: it has been moved out"),
                        );
                    }
                    Some(_) | None => {
                        env.transition(sym, BindingState::Valid);
                    }
                }
            }
        }
        HirStmtKind::Expr(e) => {
            synth_expr(env, e, ic);
        }
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "try_tests.rs"]
mod try_tests;

#[cfg(test)]
#[path = "field_tracker_tests.rs"]
mod field_tracker_tests;

#[cfg(test)]
#[path = "range_tests.rs"]
mod range_tests;

#[cfg(test)]
#[path = "comptime_purity_tests.rs"]
mod comptime_purity_tests;

#[cfg(test)]
#[path = "spawn_tests.rs"]
mod spawn_tests;
