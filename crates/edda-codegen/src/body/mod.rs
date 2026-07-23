//! AST → canonical body bytes — the §4 deterministic encoding that
//! fills [`crate::CanonicalForm::canonical_body`].
//!
//! Per `docs/codegen/storage.md` §4 the encoding is a tree walk over the
//! AST with one-byte kind tags and length-prefixed children. Identifiers
//! resolve to fully qualified names before serialisation so that two
//! source forms that reference the same item through different import
//! aliases hash identically (`spec-language.md` §9).
//!
//! # Encoding surface
//!
//! - **Kind-tag taxonomy** ([`tags`]) — locked at [`BodyVersion(0x01)`]
//!   for the signature-level AST enums: `TypeKind`, `ParamMode`, and
//!   `EffectMember`.
//! - **Path resolver boundary** ([`QualifiedNameResolver`]) — the seam
//!   where the caller (`edda-driver`) hands in resolved qualified names
//!   for AST `Path` nodes.
//! - **`Encoder`** — `write_type`, `write_effect_row`,
//!   `write_effect_member`, `write_param_mode`.
//! - **Expression / statement / pattern encoding** — `write_expr`,
//!   `write_stmt`, `write_pat`, `write_block`, `write_match_arm`,
//!   `write_struct_lit_field`, `write_struct_pat_field`,
//!   `write_variant_pat_payload`. Covers all 28 `ExprKind`, 3
//!   `StmtKind`, and 8 `PatKind` variants.
//! - **Operator / literal / mode encoders** — `write_literal`,
//!   `write_bin_op`, `write_un_op`, `write_range_kind`,
//!   `write_assign_op`, `write_binding_mode`.
//! - **`TypeKind::Refined` encodes correctly.**
//! - **Item-level encoders** — `write_visibility`, `write_generic_kind`,
//!   `write_generic_param`, `write_param`, `write_type_field`,
//!   `write_variant`, `write_variant_payload`, `write_refinement_kind`,
//!   `write_refinement_clause`, `write_type_decl`, `write_fn_decl`.
//! - **`write_item`** — dispatches on [`edda_syntax::ast::ItemKind`]
//!   using the `item_kind` tag table, calling into `write_fn_decl`,
//!   `write_type_decl`, `write_spec`, `write_import`, or
//!   `write_module_decl` as appropriate.
//! - **Spec / import / module encoders** — `write_spec` (name +
//!   generics + body), `write_import` (resolved path), and
//!   `write_module_decl` (resolved override path).
//! - **`write_spec_body`** — top-level entry that fills
//!   [`crate::CanonicalForm::canonical_body`] for one spec invocation.
//!   Skips the spec's own name (already in
//!   [`crate::CanonicalForm::spec_qualified`]); encodes generics + the
//!   body block.
//!
//! # Deferred
//!
//! - **Spec-body item walk** — once `edda-syntax` admits item
//!   declarations inside `Spec.body`, `write_spec_body` swaps its
//!   block walk for a per-item walk via `write_item`. The dispatcher
//!   is already wired up so the swap is a localized edit.
//! - **Body admission check** (`spec-language.md` §2).
//! - **`where` clause discharge.**

pub mod tags;

mod encoder;
mod exprs;
mod items;
mod literals;
mod resolver;
mod spec;

#[cfg(test)]
mod test_support;

pub use encoder::Encoder;
pub use resolver::QualifiedNameResolver;
