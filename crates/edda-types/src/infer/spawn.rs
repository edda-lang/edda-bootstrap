//! `<scope>.spawn (take a [: T] = init, ...)? { body }` inference.
//!
//! Unlike [`crate::hir::HirExprKind::Closure`]'s mandatory-capture
//! discipline, a spawn body type-checks against the *ambient* env —
//! implicit read-only capture of enclosing bindings is admitted, per
//! `corpus/edda-codex/language/05-concurrency-coherence.md` §2.2. The
//! codex's one restriction on that implicit capture — "a `mutable`
//! reference does not cross the spawn boundary" — is enforced by
//! [`super::TyEnv::restrict_mutability`]: every already-open
//! (parent-scope) binding is forced immutable before the body walks,
//! so a `mutable` / `init` call-arg referencing an enclosing binding
//! trips the same [`super::mode::transitions::reject_immutable_borrow`]
//! gate a `let` local would; the fresh
//! `take`-arg locals bound below live in the body's own child scope and
//! keep their normal mutability. Only the explicit `take`-mode argument
//! list transfers ownership out of the parent scope.
//!
//! `T` (the `Task(T)` type parameter) is the body's return type, and
//! it is also the spawn expression's own value type:
//! the frontend types the task
//! transparently as `T`, mirroring the native compiler's
//! `infer_spawn_ty` (`compiler/lib/types/src/check/body/body.ea`).
//! The linear `Task(T)` handle is a MIR-level notion — the `Spawn`
//! terminator's `dest` is `HeapPtr`-typed regardless of this type
//! ([`edda_mir` locks that invariant on `TerminatorKind::Spawn`]),
//! and `.await` recovers `T` by synthesising its operand. Unlike the
//! `Range` / `Option` implicit specs, `.spawn` registers no
//! [`crate::ImplicitSpecRequest`] — the native compiler's
//! `infer_spawn_ty` registers nothing either, and since transparent
//! typing no HIR node ever binds a
//! `Task_<T>` nominal, so a materialised `Task_<T>` module would be
//! source-unreachable. Registering it anyway forced every package
//! using `.spawn` to resolve `std.task` even without importing it,
//! breaking codegen-root collection when the import closure didn't
//! reach `std.task`.

use crate::hir::{HirCallMode, HirSpawn};
use crate::ty::TyId;

use super::mode::apply_mode_transition;
use super::{InferCx, TyEnv, check_expr, synth_block, synth_expr};

/// Synthesise a `<scope>.spawn { body }` structured-concurrency spawn.
pub(super) fn synth_spawn(env: &mut TyEnv, spawn: &mut HirSpawn, ic: &mut InferCx<'_>) -> TyId {
    for arg in spawn.args.iter_mut() {
        let arg_ty = match arg.ty {
            Some(ty) => {
                check_expr(env, &mut arg.init, ty, ic);
                ty
            }
            None => synth_expr(env, &mut arg.init, ic),
        };
        // A bare-path initialiser (`take x = outer`) consumes `outer` in
        // the parent scope, exactly like an ordinary `f(take outer)`
        // call argument; a temporary (`take x = clone(shared)`) has no
        // binding to transition and silently skips, per
        // `apply_mode_transition`'s own scoping rule.
        apply_mode_transition(env, HirCallMode::Take, &arg.init, arg.span, ic);
        arg.ty = Some(arg_ty);
    }

    let outer_mutability = env.restrict_mutability();
    env.enter_scope();
    for arg in spawn.args.iter() {
        let ty: TyId = arg.ty.expect("populated in the loop above");
        env.bind(arg.name.name, ty);
    }
    let body_ty = synth_block(env, &mut spawn.body, ic);
    env.exit_scope();
    env.restore_mutability(outer_mutability);

    body_ty
}
