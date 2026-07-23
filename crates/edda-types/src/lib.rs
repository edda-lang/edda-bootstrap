//! HIR, type inference, mode checking, and effect-row checking.
//!
//! Owns the typed intermediate form. Bidirectional inference,
//! effect-row checking, and parameter-mode (let/mutable/take/set)
//! linearity checking all live in this crate.
//!
//! Implements the corpus surface across:
//!   - `corpus/edda-codex/language/01-syntax.md` (types, function
//!     signatures, expressions, patterns)
//!   - `corpus/edda-codex/language/02-modes-effects-refinements.md`
//!     (modes, effect rows, refinement handoff)
//!   - `corpus/edda-codex/language/04-specs-comptime.md`
//!     (comptime-purity verification)
//!
//! # Current state
//!
//! As of 2026-05-18 the crate is feature-complete against the locked
//! corpus surface:
//!
//! - Type representation — interned [`TyId`] handles, [`Primitive`]
//!   catalogue, [`TyInterner`]. Structural type identity reduces to
//!   handle equality.
//! - AST → HIR structural lowering — every `ast::*` variant maps
//!   mechanically to its HIR counterpart.
//! - Bidirectional inference — literals + bindings, operators +
//!   control flow + composites, multi-segment paths, calls / fields /
//!   struct literals, pattern destructuring (tuple / variant /
//!   struct / guard).
//! - Effect rows — closed-row machinery, originators (`Raise` /
//!   `Panic`), call-site row union, `?` propagation, function-exit
//!   containment, call-site capability substitution per the
//!   "rows name parameters held, not derived bindings" rule.
//! - Mode tracker — `Uninit` / `Valid` / `PartialInit(F)` /
//!   `Consumed` lattice with statement and call-site transitions,
//!   branch GLB merging, loop re-entry, function-exit checks.
//! - Implicit spec invocation registration ([`ImplicitSpecRequest`])
//!   for `Range` and `Option` patterns.
//! - Comptime-purity verification at call sites.
//! - [`CapabilityType`] catalogue (Clock / MonotonicClock / Stdout /
//!   Stderr / Stdin / Allocator / Filesystem / Network / Random /
//!   Executor / ReadOnlyFilesystem / SandboxedFilesystem /
//!   LocalhostNetwork / RestrictedNetwork / BoundedAllocator /
//!   DeterministicRandom / Subprocess / Debugger) — pre-allocated in
//!   the interner.
//! - Refinement discharge handoff to `edda-refine` via the `refine`
//!   Cargo feature. `where` clauses (type-level invariants) still
//!   owed.
//!
//! The running history for any specific item lives in
//! `git log --oneline crates/edda-types/`.
//!
//! # Type identity
//!
//! [`TyId`] is a 32-bit handle issued by a [`TyInterner`]. Equal
//! `TyId`s denote the same type by construction — the interner
//! deduplicates structurally equal [`TyKind`]s on insertion. Callers
//! compare types by `TyId == TyId` rather than walking [`TyKind`]s.
//! Primitives are pre-allocated at construction time so primitive
//! lookups (`interner.prim(Primitive::I32)`) are O(1) handle accesses.

mod attr;
#[cfg(test)]
mod attr_tests;
mod capability;
mod check;
mod coherence;
mod comptime_builtin;
mod comptime_expand;
mod cx;
mod effect;
mod graded;
mod hir;
mod implicit_spec;
mod infer;
mod intrinsic;
mod lower;
mod mono;
mod prim;
#[cfg(feature = "refine")]
mod refine;
#[cfg(feature = "refine")]
mod graded_refine;
mod return_mode;
mod sig;
mod stability;
mod ty;

#[cfg(test)]
mod test_support;

pub use attr::{AttrAbi, AttrLayout, AttrRepr, AttrSet};
pub use check::{
    TypedExternFunction, TypedFunction, TypedPackage, TypedSpecInvocation, TypedTypeDecl,
    check_capability_availability, check_package,
};
pub use comptime_builtin::{ComptimeBuiltin, comptime_builtin_for_name};
pub use intrinsic::{
    CapabilityMethod, IntrinsicKind, PrimitiveStaticMethod, resolve_capability_method,
    resolve_primitive_static_method,
};
pub use cx::{
    ConstInit, FieldInfo, TyCx, TypeDeclInfo, TypeDeclShape, VariantInfo, VariantPayloadInfo,
};
pub use effect::{EffectEntry, EffectRow, GradedBound, PureEffect};
pub use implicit_spec::{ImplicitSpec, ImplicitSpecRequest};
pub use hir::{
    HirBlock, HirCallArg, HirCallMode, HirClosure, HirExpr, HirExprKind, HirFStringPart,
    HirMatchArm, HirPat, HirPatKind, HirPath, HirSpawn, HirSpawnArg, HirStmt, HirStmtKind,
    HirStructLitField, HirStructPatField, HirVariantPatPayload,
};
pub use capability::CapabilityType;
pub use prim::Primitive;
pub use sig::{FnPtrParam, FnPtrSig, FnSig, Param, ParamMode, ReturnMode};
pub use ty::{TyDisplay, TyId, TyInterner, TyKind};
