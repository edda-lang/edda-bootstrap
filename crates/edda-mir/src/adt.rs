//! Algebraic data type definitions: products (records / tuples) and sums.

use edda_intern::Symbol;
use edda_span::Span;

use crate::layout::LayoutInfo;
use crate::ty::{MirPrim, MirType};

/// A program-level ADT definition.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct AdtDef {
    /// Source-declared ADT name.
    pub name: Symbol,
    /// Defining span.
    pub span: Span,
    /// Product or sum.
    pub kind: AdtKind,
    /// Variants (length 1 for products, ≥ 1 for sums).
    pub variants: Vec<VariantDef>,
    /// Resolved layout descriptor.
    pub layout: LayoutInfo,
    /// Discriminant integer width for sums; `None` for products.
    pub tag_width: Option<MirPrim>,
}

/// ADT family — does this type have a discriminant?
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum AdtKind {
    /// Product type (record, tuple struct, single-variant ADT).
    Product,
    /// Sum type (tagged union) with one discriminant per variant.
    Sum,
}

/// One variant of an [`AdtDef`].
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct VariantDef {
    /// Variant name (same as the ADT name for single-variant products).
    pub name: Symbol,
    /// Defining span.
    pub span: Span,
    /// Fields, in declaration order.
    pub fields: Vec<FieldDef>,
    /// Resolved discriminant integer for sum variants; `None` for products.
    pub discriminant: Option<u64>,
}

/// One field of a [`VariantDef`].
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct FieldDef {
    /// Field name.
    pub name: Symbol,
    /// Defining span.
    pub span: Span,
    /// Field type.
    pub ty: MirType,
}
