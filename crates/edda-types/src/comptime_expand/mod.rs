//! Pass-2 comptime-`for` unrolling + `CompField` fold + `comptime if`
//! dead-branch elision (D-22).
//!
//! Runs after name resolution and before AST→HIR lowering, per
//! `corpus/edda-codex/language/04-specs-comptime.md` §3.1 / §4.5. It
//! eliminates the D-22 comptime forms so that HIR / inference / MIR
//! never see them:
//!
//! - **`comptime for <i> in 0..<field_count(T) { <body> }`** — the loop
//!   is fully unrolled at this pass; each iteration emits a concrete
//!   block with `<i>` bound to the iteration's literal index.
//! - **`<receiver>.(<index>)`** ([`ExprKind::CompField`]) — once the
//!   enclosing loop binds `<i>` to a literal, the index folds to a
//!   constant `k` and the node is rewritten to a plain
//!   [`ExprKind::Field`] naming `field_name_at(T, k)`.
//! - **`comptime if <cond> { … } else { … }`** — when `<cond>` folds to
//!   a comptime constant (the six `is_*` predicates and `==`/`!=` over
//!   `field_type_at(T, k)` / named types), only the live arm is spliced
//!   into the checked body. The dead arm is never typechecked and
//!   contributes nothing to the effect row.
//!   Undecidable conditions leave the `comptime if` in place (both arms
//!   checked).
//! - **A bare `field_count(T)` call in general expression position** —
//!   not only as a `comptime for` bound: any occurrence anywhere in the
//!   body folds to `T`'s literal member count once `T` resolves via the
//!   [`ShapeIndex`].
//!
//! Field introspection is structural: `field_count(T)` and the i-th
//! field name come straight from `T`'s product-[`TypeDecl`], so the pass
//! needs no `TyInterner` — only the [`FieldIndex`] built from the
//! resolved package's AST. The receiver of a `CompField` is assumed to
//! be the construction target of type `T` (the `field_count(T)` argument
//! of the enclosing loop); this is the record-construction shape locked
//! in §4.5. Field-type dispatch (`field_type_at` as a value) is handled
//! by the [`crate::mono`] pass binding inbound `comptime <name>: Type`
//! generics from the per-member declared-type leafs recorded here.
//!
//! # Module layout
//!
//! - This file ([`comptime_expand`](self)) — the [`Shape`] / [`ShapeIndex`]
//!   construction-shape types, [`build_shape_index`], and the per-body
//!   [`expand_fn_body`] entry point.
//! - [`prescan`] — the cheap "does this body contain a D-22 form?"
//!   pre-scan and the generic child-traversal helpers it walks.
//! - [`expander`] — the [`expander::Expander`] cloning walker that
//!   performs the unroll + fold, plus its [`expander::Env`] loop-binding
//!   environment and [`expander::UnrollPlan`].
//! - [`comptime_if`] — the comptime-`if` condition evaluator (type-valued
//!   `field_type_at` / `is_*` predicate folding) and live-arm selection.

use ahash::AHashMap;
use edda_diag::Diagnostics;
use edda_intern::{Interner, Symbol};
use edda_resolve::ResolvedPackage;
use edda_syntax::ast::{
    Block, Ident, ItemKind, Linearity, Type, TypeDeclKind, TypeKind, VariantPayload,
};

mod comptime_if;
mod expander;
mod prescan;
mod recurse;

use expander::{Env, Expander};
use prescan::block_has_comptime_construct;

/// One member's declared-type reference — `field_type_at(T, k)` ground
/// truth for the comptime-if evaluator and the mono pass.
///
/// Sum variants follow the native payload-composite convention: a
/// payload-less variant's payload
/// type is `()`, and every payload-bearing variant's is the payload
/// *tuple* — including single-payload (`case data(u32)` → the
/// one-element tuple `(u32)`), so `is_primitive(field_type_at(U, i))`
/// folds true exactly for unit arms.
pub(crate) enum MemberTy {
    /// Plain named type — a product field declared as a path
    /// (`i32`, `bool`, `Inner`).
    Named(Symbol, edda_span::Span),
    /// Payload-less sum variant: the payload composite is `()`.
    Unit,
    /// Sum-variant payload composite (tuple or struct payload): the
    /// per-element plain-path type references in declaration order.
    /// `None` for an element whose declared type is not a plain path.
    Tuple(Vec<Option<(Symbol, edda_span::Span)>>),
    /// Slice product field `[E]`: the element type's plain-path
    /// reference, or `None` when the element is not a plain path. Lets
    /// outbound-generic inference bind an enclosing `U := [E]` through
    /// `v.slice_field`.
    Slice(Option<(Symbol, edda_span::Span)>),
    /// Not representable (structural product-field type: function, nested
    /// tuple/slice element, …) — `field_type_at(T, k)` over it is
    /// comptime-undecidable.
    Opaque,
}

/// One user type's construction shape: product (field assignment via
/// `out.(i)`) or sum (variant construction via `T.(d)`), plus its member
/// names in declaration order. `field_count(T)` is `members.len()` for
/// both kinds.
pub(crate) struct Shape {
    /// `true` for a sum type (variant construction), `false` for a product.
    pub is_sum: bool,
    /// Field names (product) or variant names (sum), declaration order.
    pub members: Vec<Ident>,
    /// Per-member declared-type reference (field type for a product,
    /// payload composite for a sum).
    pub member_tys: Vec<MemberTy>,
    /// `linear` / `affine` modifier on this type's own declaration.
    pub linearity: Option<Linearity>,
}

/// Map a user type's leaf name → its [`Shape`]. Drives the unroll count
/// (`field_count(T)`), the `out.(i)` → `out.<field_name>` record rewrite,
/// and the `T.(d)` → `T.<variant_name>` variant rewrite.
pub(crate) type ShapeIndex = AHashMap<Symbol, Shape>;

/// Build the package-wide [`ShapeIndex`] from every type declaration's
/// AST (user modules + generated spec artifacts alike).
pub(crate) fn build_shape_index(package: &ResolvedPackage) -> ShapeIndex {
    let mut index = ShapeIndex::default();
    for module_entry in package.graph().modules() {
        for item in &module_entry.ast.items {
            if let ItemKind::TypeDecl(decl) = &item.kind {
                let shape = match &decl.kind {
                    TypeDeclKind::Product { fields } => Shape {
                        is_sum: false,
                        members: fields.iter().map(|f| f.name).collect(),
                        member_tys: fields
                            .iter()
                            .map(|f| product_field_member_ty(&f.ty))
                            .collect(),
                        linearity: decl.linearity,
                    },
                    TypeDeclKind::Sum { variants } => Shape {
                        is_sum: true,
                        members: variants.iter().map(|v| v.name).collect(),
                        member_tys: variants
                            .iter()
                            .map(|v| payload_member_ty(&v.payload))
                            .collect(),
                        linearity: decl.linearity,
                    },
                };
                index.insert(decl.name.name, shape);
            }
        }
    }
    index
}

/// A product field's [`MemberTy`]: a plain path projects `Named`; a
/// tuple field projects the positional composite of its element
/// references — the same convention a sum variant's tuple payload uses —
/// so outbound-generic
/// inference through `v.field` reaches a tuple field's element types
/// instead of stalling on `Opaque`; the unit type projects `Unit`; a
/// slice field `[E]` projects `Slice` with its element reference so an
/// enclosing `U := [E]` binds through `v.slice_field`. A function / other
/// structural field
/// stays `Opaque`.
fn product_field_member_ty(ty: &Type) -> MemberTy {
    match &ty.kind {
        TypeKind::Path(_) => match path_ref(ty) {
            Some((leaf, span)) => MemberTy::Named(leaf, span),
            None => MemberTy::Opaque,
        },
        TypeKind::Unit => MemberTy::Unit,
        TypeKind::Tuple(tys) => MemberTy::Tuple(tys.iter().map(path_ref).collect()),
        TypeKind::Slice(inner) => MemberTy::Slice(path_ref(inner)),
        _ => MemberTy::Opaque,
    }
}

/// The `(leaf, span)` reference of a plain-path type annotation.
fn path_ref(ty: &Type) -> Option<(Symbol, edda_span::Span)> {
    match &ty.kind {
        TypeKind::Path(p) => p.segments.last().map(|s| (s.name, p.span)),
        _ => None,
    }
}

/// A variant payload's [`MemberTy`] under the native payload-composite
/// convention: unit variants project
/// `()`; tuple and struct payloads project the positional composite of
/// their element types (struct payloads use field declaration order,
/// matching the native's `lower_payload_struct` → `tuple_` lowering).
fn payload_member_ty(payload: &VariantPayload) -> MemberTy {
    match payload {
        VariantPayload::Unit => MemberTy::Unit,
        VariantPayload::Tuple(tys) => {
            MemberTy::Tuple(tys.iter().map(path_ref).collect())
        }
        VariantPayload::Struct(fields) => {
            MemberTy::Tuple(fields.iter().map(|f| path_ref(&f.ty)).collect())
        }
    }
}

//   contains a `comptime for` / `CompField` / bare `field_count(T)` call
//   — callers pass the original borrowed block to typecheck unchanged
//   when this returns `None`, avoiding a clone of every non-comptime
//   function body
/// Expand the D-22 comptime forms inside one function body. Returns the
/// rewritten block, or `None` when the body contains nothing to expand.
pub(crate) fn expand_fn_body(
    block: &Block,
    shapes: &ShapeIndex,
    interner: &Interner,
    target: &edda_target::TargetCfg,
    diags: &mut Diagnostics,
) -> Option<Block> {
    if !block_has_comptime_construct(block, interner) {
        return None;
    }
    let exp = Expander { shapes, interner, target };
    Some(exp.block(block, &Env::None, diags))
}
