//! Typed pattern nodes (`HirPat`, `HirPatKind`, payload-shape helpers).
//!
//! Mirrors `ast::PatKind` one-for-one with a [`TyId`] carrier on
//! [`HirPat`] for the value type the pattern matches. The pattern's
//! `ty` is the scrutinee type the inference pass fixes.

use edda_span::Span;
use edda_syntax::ast::{Ident, Literal, RangeKind};

use crate::ty::TyId;

use super::{HirExpr, HirPath};

/// A typed HIR pattern node.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct HirPat {
    /// Source range.
    pub span: Span,
    /// Value type the pattern matches.
    pub ty: TyId,
    /// Variant and payload.
    pub kind: HirPatKind,
}

/// Every pattern form admitted by `match`, `let`, and `for`.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum HirPatKind {
    /// `_` — discards the value.
    Wildcard,
    /// `name` — binds the value to a fresh local. The bound binding's
    /// type is the enclosing [`HirPat::ty`].
    Binding(Ident),
    /// `42`, `"hello"`, `true` — matches by equality. The literal's
    /// type must agree with the pattern's `ty`.
    Literal(Literal),
    /// `(p1, p2, ...)` — tuple destructuring.
    Tuple(Box<[HirPat]>),
    /// `Path.variant [payload]` — sum-variant pattern.
    Variant {
        /// Qualified variant name.
        path: HirPath,
        /// Payload shape (none / positional / named).
        payload: HirVariantPatPayload,
    },
    /// `Path { field, field: pat, .. }` — struct destructuring.
    Struct {
        /// Type path being destructured.
        path: HirPath,
        /// Named-field patterns.
        fields: Box<[HirStructPatField]>,
        /// `true` if the pattern ended with `..` to ignore extra fields.
        rest: bool,
    },
    /// `pat where cond` — pattern with refinement guard. The guard's
    /// value type must be `bool`.
    Guard {
        /// Inner pattern.
        pat: Box<HirPat>,
        /// Boolean guard expression.
        cond: HirExpr,
    },
    /// `lo..<hi` / `lo..=hi` — literal range pattern; binds no names.
    Range {
        /// Inclusive lower bound literal.
        lo: Literal,
        /// Upper bound literal (exclusive for `HalfOpen`).
        hi: Literal,
        /// `..<` (half-open) vs `..=` (closed) discriminator.
        kind: RangeKind,
    },
    /// `name @ subpattern` — binds the whole matched value to `name`
    /// and matches its shape against `inner`. The bound binding's type
    /// is the enclosing [`HirPat::ty`].
    AtBinding {
        /// The name bound to the whole matched value.
        name: Ident,
        /// Sub-pattern the value's shape is matched against.
        inner: Box<HirPat>,
    },
    /// `[p, ..]` / `[head, ..tail]` / `[..init, last]` / `[]` — slice
    /// destructuring with at most one rest binding.
    Slice {
        /// Patterns before the rest binding (all elements if no rest).
        prefix: Box<[HirPat]>,
        /// `None` = no rest; `Some(None)` = bare `..`;
        /// `Some(Some(name))` = `..name` binding the remaining slice.
        rest: Option<Option<Ident>>,
        /// Patterns after the rest binding (empty if no rest).
        suffix: Box<[HirPat]>,
    },
    /// Lowering-recovery sentinel. A diagnostic has already been emitted.
    Error,
}

/// Payload of a [`HirPatKind::Variant`]: unit / positional / named.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum HirVariantPatPayload {
    /// No payload — `Color.red`.
    None,
    /// Tuple payload — `Json.array(items)` (positional).
    Tuple(Box<[HirPat]>),
    /// Struct payload — `Event.click { x, y }` (named).
    Struct(Box<[HirStructPatField]>),
}

/// A field pattern inside a struct or struct-variant pattern.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct HirStructPatField {
    /// Source range covering the field entry.
    pub span: Span,
    /// Field name being matched.
    pub name: Ident,
    /// Sub-pattern. The shorthand `name` is desugared to
    /// `name: Binding(name)` by AST → HIR lowering.
    pub pat: HirPat,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prim::Primitive;
    use crate::ty::TyInterner;
    use edda_intern::Interner;
    use edda_syntax::ast::{Ident, Literal};

    fn ident(interner: &Interner, name: &str) -> Ident {
        Ident {
            name: interner.intern(name),
            span: Span::DUMMY,
        }
    }

    #[test]
    fn wildcard_carries_ty() {
        let ty = TyInterner::new();
        let p = HirPat {
            span: Span::DUMMY,
            ty: ty.prim(Primitive::I32),
            kind: HirPatKind::Wildcard,
        };
        assert_eq!(p.ty, ty.prim(Primitive::I32));
        assert!(matches!(p.kind, HirPatKind::Wildcard));
    }

    #[test]
    fn binding_carries_ident_and_ty() {
        let interner = Interner::new();
        let ty = TyInterner::new();
        let p = HirPat {
            span: Span::DUMMY,
            ty: ty.prim(Primitive::Bool),
            kind: HirPatKind::Binding(ident(&interner, "flag")),
        };
        match &p.kind {
            HirPatKind::Binding(id) => assert_eq!(id.name, interner.intern("flag")),
            _ => panic!("expected Binding"),
        }
    }

    #[test]
    fn tuple_pattern_round_trips() {
        let ty = TyInterner::new();
        let inner = HirPat {
            span: Span::DUMMY,
            ty: ty.prim(Primitive::I32),
            kind: HirPatKind::Wildcard,
        };
        let outer = HirPat {
            span: Span::DUMMY,
            ty: ty.prim(Primitive::I32),
            kind: HirPatKind::Tuple(Box::from([inner.clone(), inner.clone()])),
        };
        match &outer.kind {
            HirPatKind::Tuple(elems) => assert_eq!(elems.len(), 2),
            _ => panic!("expected Tuple"),
        }
    }

    #[test]
    fn variant_payload_variants_round_trip() {
        let ty = TyInterner::new();
        let interner = Interner::new();
        let inner = HirPat {
            span: Span::DUMMY,
            ty: ty.prim(Primitive::I32),
            kind: HirPatKind::Wildcard,
        };
        let none_payload = HirVariantPatPayload::None;
        let tuple_payload =
            HirVariantPatPayload::Tuple(Box::from([inner.clone()]));
        let struct_payload = HirVariantPatPayload::Struct(Box::from([HirStructPatField {
            span: Span::DUMMY,
            name: ident(&interner, "x"),
            pat: inner.clone(),
        }]));
        assert!(matches!(none_payload, HirVariantPatPayload::None));
        assert!(matches!(tuple_payload, HirVariantPatPayload::Tuple(_)));
        assert!(matches!(struct_payload, HirVariantPatPayload::Struct(_)));
    }

    #[test]
    fn literal_pattern_holds_value() {
        let ty = TyInterner::new();
        let p = HirPat {
            span: Span::DUMMY,
            ty: ty.prim(Primitive::Bool),
            kind: HirPatKind::Literal(Literal::Bool(true)),
        };
        assert!(matches!(
            p.kind,
            HirPatKind::Literal(Literal::Bool(true))
        ));
    }

    #[test]
    fn error_sentinel_round_trips() {
        let ty = TyInterner::new();
        let p = HirPat {
            span: Span::DUMMY,
            ty: ty.error(),
            kind: HirPatKind::Error,
        };
        let cloned = p.clone();
        assert_eq!(p, cloned);
    }
}
