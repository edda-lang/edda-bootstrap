//! Comptime values.
//!
//! The comptime evaluator exchanges values with the rest of the
//! compiler through [`Value`]. Variants mirror the locked primitive
//! catalogue from `types.md` plus the `Type` meta-type from
//! `comptime.md`.
//!
//! Types referenced from a `Value::Type` use [`edda_types::TyId`], so
//! the comptime layer shares the type-system's view of "what type is
//! this" by handle equality rather than by re-encoding the type.

use std::fmt;

use edda_intern::Symbol;
use edda_types::TyId;

mod float;
mod int;

pub use float::FloatValue;
pub use int::IntValue;

/// Discriminant of [`Value`] for diagnostic messages and type-error
/// branches.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum ValueKind {
    /// [`Value::Unit`].
    Unit,
    /// [`Value::Bool`].
    Bool,
    /// [`Value::Int`].
    Int,
    /// [`Value::Float`].
    Float,
    /// [`Value::Str`].
    Str,
    /// [`Value::Type`].
    Type,
    /// [`Value::Array`].
    Array,
    /// [`Value::Record`].
    Record,
}

impl ValueKind {
    /// Human-readable name for diagnostics (`"int"`, `"Type"`, `"String"`, …).
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unit => "()",
            Self::Bool => "bool",
            Self::Int => "int",
            Self::Float => "float",
            Self::Str => "String",
            Self::Type => "Type",
            Self::Array => "array",
            Self::Record => "record",
        }
    }
}

impl fmt::Display for ValueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A compile-time value.
///
/// The variant set covers the locked primitive catalogue
/// (`types.md`) plus the comptime-only `Type` meta-type
/// (`comptime.md`) and the structural `[T]` array/slice aggregate
/// (`Array`) that bounded `for`-loop evaluation over ranges and array
/// literals produces and consumes, plus the nominal
/// product aggregate (`Record`) a comptime struct literal constructs.
/// User-defined sum-variant values are not in this enum
/// yet — they land alongside `edda-types` support when comptime
/// `match` over a nominal reaches the evaluator.
///
/// `Eq`/`PartialEq` are *not* derived: `FloatValue` cannot satisfy
/// reflexive equality (IEEE-754 NaN). Callers that need structural
/// comparison match on variants and compare payloads explicitly.
#[derive(Clone, Debug)]
pub enum Value {
    /// `()`. Sole value of the unit type.
    Unit,
    /// `bool` value.
    Bool(bool),
    /// Integer value with explicit width.
    Int(IntValue),
    /// IEEE-754 float value with explicit width.
    Float(FloatValue),
    /// Interned string literal.
    Str(Symbol),
    /// A `Type`-typed comptime constant. Type-identifier-as-argument
    /// occurrences (e.g. `i32` in `size_of(i32)`) lower to this
    /// variant; the payload is the type's interned handle.
    Type(TyId),
    /// A `[T]` array/slice value — an `[e1, ..., en]` literal, or the
    /// element sequence a bounded `for`-loop iterates.
    Array(Vec<Value>),
    /// A nominal product (`record`) value — a `Path { field: e, ... }`
    /// struct-literal constructed at comptime. Field
    /// entries are `(field-name, value)` pairs in struct-literal source
    /// order. Purely internal to the evaluator: a comptime block that
    /// reduces to a record has no runtime constant form, so
    /// [`crate::Value::Record`] never reaches the MIR materialisation
    /// seam as a final value — it only flows between a helper's `return`
    /// and the caller that consumes/discards it.
    Record(Vec<(Symbol, Value)>),
}

impl Value {
    /// Discriminant of this value for diagnostics.
    pub const fn kind(&self) -> ValueKind {
        match self {
            Self::Unit => ValueKind::Unit,
            Self::Bool(_) => ValueKind::Bool,
            Self::Int(_) => ValueKind::Int,
            Self::Float(_) => ValueKind::Float,
            Self::Str(_) => ValueKind::Str,
            Self::Type(_) => ValueKind::Type,
            Self::Array(_) => ValueKind::Array,
            Self::Record(_) => ValueKind::Record,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edda_types::{Primitive, TyInterner};

    #[test]
    fn value_kind_names() {
        let ty = TyInterner::new();
        assert_eq!(Value::Unit.kind().name(), "()");
        assert_eq!(Value::Bool(true).kind().name(), "bool");
        assert_eq!(
            Value::Int(IntValue::new_signed(Primitive::I32, 0))
                .kind()
                .name(),
            "int"
        );
        assert_eq!(Value::Float(FloatValue::F32(0.0)).kind().name(), "float");
        assert_eq!(
            Value::Type(ty.prim(Primitive::I32)).kind().name(),
            "Type"
        );
        assert_eq!(Value::Array(vec![]).kind().name(), "array");
        assert_eq!(Value::Record(vec![]).kind().name(), "record");
    }
}
