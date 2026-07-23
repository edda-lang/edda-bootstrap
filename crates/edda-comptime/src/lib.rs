//! Comptime evaluator and comptime built-ins.
//!
//! Hosts the locked comptime built-in catalogue: `size_of`, `align_of`,
//! `offset_of`, `target_has`, plus the reflective-introspection names
//! `field_count`, `field_name_at`, `field_type_at`, and the seven `is_*`
//! predicates (`is_signed`, `is_unsigned`, `is_integer`, `is_floating`,
//! `is_numeric`, `is_primitive`, `is_sum`).
//!
//! # Current state
//!
//! As of 2026-05-18 the crate provides the value model, the built-in
//! catalogue + dispatch, type layout queries over the currently-locked
//! types, and a full HIR-walking expression evaluator over
//! [`edda_types::HirExpr`] (literals → arithmetic → control flow →
//! comptime built-in calls). Recent commits have added the
//! `TypeDeclLookup` trait for type-system lookups (resolving
//! `TyKind::Nominal` handles to their field tables) and integrated the
//! evaluator with the codegen cascade.
//!
//! The running history for any specific item lives in
//! `git log --oneline crates/edda-comptime/`.
//!
//! Still owed: sum-typed user-record layout (products already lay out
//! through `TypeDeclLookup`; sums route through
//! `LayoutUnsupported::NominalLayoutDeferred` until variant-tag codegen
//! lands); the `@layout`-attribute path for `offset_of` against user
//! types; and the comptime-purity inference at the `edda-types` seam.
//!
//! Implements the corpus comptime surface:
//!   - `corpus/edda-codex/language/04-specs-comptime.md`
//!   - `corpus/edda-codex/language/06-tooling.md` (size_of / align_of /
//!     offset_of / target_has, target gating)

mod builtin;
mod error;
mod eval;
mod fndecl;
mod layout;
mod value;

pub use builtin::{Builtin, BuiltinParamKind, BuiltinSignature, builtin_for_name};
pub use error::ComptimeError;
pub use eval::{ComptimeEnv, EvalCx, MAX_DEPTH, eval_builtin, eval_builtin_with_decls, eval_expr};
pub use fndecl::{FnDeclInfo, FnDeclLookup};
pub use layout::{Layout, LayoutUnsupported, TypeDeclLookup};
pub use value::{FloatValue, IntValue, Value, ValueKind};
