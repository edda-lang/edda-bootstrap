//! Spec-invocation monomorphization.
//!
//! Substitutes a spec's comptime parameters with the [`ArgumentTuple`]
//! supplied at one invocation site. The surface is split in
//! two:
//!
//! - [`SubstitutionMap`] — position-validated binding table.
//!   Construction rejects arity, kind, and currently-unsupported-kind
//!   mismatches before any walk runs.
//! - [`substitute_spec_body`] — clone-and-rewrite walker that produces
//!   a new [`edda_syntax::ast::Block`] with every reference to a bound
//!   generic parameter substituted.
//!
//! # Substitution surface
//!
//! - `TypeKind::Path` and `ExprKind::Path` whose head segment matches a
//!   bound `Type`-kind generic → rewrite path with the bound qualified
//!   name's segments followed by the original path's tail.
//! - `ExprKind::Path` single-segment matching a bound `Comptime`-kind
//!   generic → replace with the bound primitive's literal expression.
//!   Negative signed integers wrap in `ExprKind::Unary { Neg }` around
//!   a non-negative `Literal::Int`.
//! - `StructLit.path`, `Pat::Variant.path`, `Pat::Struct.path` — same
//!   Type-kind head rewrite (these positions are type references).
//!
//! All other AST nodes deep-clone with spans preserved.
//!
//! # Deferred
//!
//! - **EffectRow expansion** — `EffectMember::Spread` whose path names a
//!   comptime `EffectRow` parameter would splice the bound row's members
//!   in place. [`SubstitutionMap::bind`] rejects `Argument::EffectRow`
//!   so this position is never reached.
//! - **UserDefined constructor expansion** — `ExprKind::Path` matching a
//!   comptime `UserDefined`-typed parameter would expand to the
//!   appropriate struct-literal or sum-variant constructor expression.
//!   Likewise rejected at bind time.

mod map;
mod walk;
mod walk_items;

pub use map::SubstitutionMap;
pub use walk::substitute_spec_body;

#[cfg(test)]
mod tests;
