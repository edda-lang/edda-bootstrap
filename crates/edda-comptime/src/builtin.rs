//! Locked comptime built-in catalogue.
//!
//! `comptime.md` (*Comptime built-ins*) locks `size_of` and
//! `align_of`. `abi-and-layout.md` §8 and `build-system.md` §9
//! lock `offset_of` and `target_has`. The reflective-introspection
//! group — `field_count`, `field_name_at`, `field_type_at`, and the
//! eight `is_*` predicates (`is_signed`, `is_unsigned`, `is_integer`,
//! `is_floating`, `is_numeric`, `is_primitive`, `is_sum`, `is_copy`) —
//! locks the remaining eleven names against the self-hosted reference
//! in `lib/cteval/src/builtin/{layout,category}.ea` (`is_sum` landed
//! with the D-22 sum-type READ seam;
//! `is_copy` broadens the `comptime if is_primitive(T)` bulk-copy
//! fast-path gate to freely-copyable user records).
//!
//! The catalogue serves two callers:
//!
//! - **`edda-resolve`** consults it during name resolution so a user
//!   `function size_of(...)` declaration is rejected before any call
//!   site sees it.
//! - **`edda-comptime`'s evaluator** ([`crate::eval::eval_builtin`])
//!   dispatches an already-evaluated argument tuple to the right
//!   built-in.

use std::fmt;

/// Locked comptime built-in. Each variant maps to one name in the
/// spec-locked catalogue.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
#[repr(u8)]
pub enum Builtin {
    /// `size_of(T: Type) -> usize`. `comptime.md` *Comptime built-ins*.
    SizeOf,
    /// `align_of(T: Type) -> usize`. `comptime.md` *Comptime built-ins*.
    AlignOf,
    /// `offset_of(T: Type, field: String) -> usize`. `abi-and-layout.md` §8.
    OffsetOf,
    /// `target_has(feature: String) -> bool`. `build-system.md` §9.
    TargetHas,
    /// `field_count(T: Type) -> usize`. Record/sum/tuple member count.
    FieldCount,
    /// `field_name_at(T: Type, i: usize) -> String`. i-th field/variant name.
    FieldNameAt,
    /// `field_type_at(T: Type, i: usize) -> Type`. i-th element/field/payload type.
    FieldTypeAt,
    /// `is_signed(T: Type) -> bool`. Signed-integer primitive predicate.
    IsSigned,
    /// `is_unsigned(T: Type) -> bool`. Unsigned-integer primitive predicate.
    IsUnsigned,
    /// `is_integer(T: Type) -> bool`. Any-integer primitive predicate.
    IsInteger,
    /// `is_floating(T: Type) -> bool`. Float (`f32`/`f64`) predicate.
    IsFloating,
    /// `is_numeric(T: Type) -> bool`. Integer-or-float predicate.
    IsNumeric,
    /// `is_primitive(T: Type) -> bool`. Any `TyKind::Primitive` predicate.
    IsPrimitive,
    /// `is_sum(T: Type) -> bool`. Sum-type (variant-carrying nominal)
    /// predicate — the D-22 READ-surface dispatch gate for the derive
    /// / serde sum-type fan-out.
    IsSum,
    /// `is_copy(T: Type) -> bool`. `true` iff `T` is non-`linear`,
    /// non-`affine`, and transitively contains no `linear`/`affine`
    /// field — broadens the `comptime if is_primitive(T)` bulk-copy
    /// fast-path gate to freely-copyable user records.
    IsCopy,
    /// `is_container(T: Type) -> bool`. Spec-instance container
    /// predicate (`Box_*`, `Vec_*`, `Option_*`, map instances). The
    /// decidable answer is minted by `comptime_expand`'s
    /// `comptime_if` folding, where a spec-instantiation-generated type
    /// is a mangled leaf absent from the `ShapeIndex`; by the time this
    /// post-monomorphisation evaluator runs, such a type has collapsed
    /// to its concrete `TyKind::Nominal` shape and is no longer a
    /// distinguishable spec instance, so this path answers `false`
    /// uniformly. A given `comptime if is_container(…)` is folded by
    /// exactly one path, so the two never disagree.
    IsContainer,
}

/// Number of locked built-ins. Bumps require a spec change.
pub const BUILTIN_COUNT: usize = 16;

impl Builtin {
    /// Every locked built-in in declaration order.
    pub const ALL: [Builtin; BUILTIN_COUNT] = [
        Self::SizeOf,
        Self::AlignOf,
        Self::OffsetOf,
        Self::TargetHas,
        Self::FieldCount,
        Self::FieldNameAt,
        Self::FieldTypeAt,
        Self::IsSigned,
        Self::IsUnsigned,
        Self::IsInteger,
        Self::IsFloating,
        Self::IsNumeric,
        Self::IsPrimitive,
        Self::IsSum,
        Self::IsCopy,
        Self::IsContainer,
    ];

    /// Lowercase spec spelling (`"size_of"`, `"align_of"`, …).
    pub const fn name(self) -> &'static str {
        match self {
            Self::SizeOf => "size_of",
            Self::AlignOf => "align_of",
            Self::OffsetOf => "offset_of",
            Self::TargetHas => "target_has",
            Self::FieldCount => "field_count",
            Self::FieldNameAt => "field_name_at",
            Self::FieldTypeAt => "field_type_at",
            Self::IsSigned => "is_signed",
            Self::IsUnsigned => "is_unsigned",
            Self::IsInteger => "is_integer",
            Self::IsFloating => "is_floating",
            Self::IsNumeric => "is_numeric",
            Self::IsPrimitive => "is_primitive",
            Self::IsSum => "is_sum",
            Self::IsCopy => "is_copy",
            Self::IsContainer => "is_container",
        }
    }

    /// Parameter shape declared in the spec.
    pub const fn signature(self) -> BuiltinSignature {
        match self {
            Self::SizeOf => BuiltinSignature {
                params: &[BuiltinParamKind::Type],
                returns: BuiltinReturnKind::Usize,
            },
            Self::AlignOf => BuiltinSignature {
                params: &[BuiltinParamKind::Type],
                returns: BuiltinReturnKind::Usize,
            },
            Self::OffsetOf => BuiltinSignature {
                params: &[BuiltinParamKind::Type, BuiltinParamKind::Str],
                returns: BuiltinReturnKind::Usize,
            },
            Self::TargetHas => BuiltinSignature {
                params: &[BuiltinParamKind::Str],
                returns: BuiltinReturnKind::Bool,
            },
            Self::FieldCount => BuiltinSignature {
                params: &[BuiltinParamKind::Type],
                returns: BuiltinReturnKind::Usize,
            },
            Self::FieldNameAt => BuiltinSignature {
                params: &[BuiltinParamKind::Type, BuiltinParamKind::Usize],
                returns: BuiltinReturnKind::Str,
            },
            Self::FieldTypeAt => BuiltinSignature {
                params: &[BuiltinParamKind::Type, BuiltinParamKind::Usize],
                returns: BuiltinReturnKind::Type,
            },
            Self::IsSigned
            | Self::IsUnsigned
            | Self::IsInteger
            | Self::IsFloating
            | Self::IsNumeric
            | Self::IsPrimitive
            | Self::IsSum
            | Self::IsCopy
            | Self::IsContainer => BuiltinSignature {
                params: &[BuiltinParamKind::Type],
                returns: BuiltinReturnKind::Bool,
            },
        }
    }
}

impl fmt::Display for Builtin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Look up the built-in matching `name`. Returns `None` for names
/// not in the locked catalogue.
pub fn builtin_for_name(name: &str) -> Option<Builtin> {
    Builtin::ALL.iter().copied().find(|b| b.name() == name)
}

/// Surface-form kind of a built-in parameter.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum BuiltinParamKind {
    /// A `Type`-typed argument (`comptime T: Type`). Bound to
    /// [`crate::Value::Type`].
    Type,
    /// A `String`-typed argument (`comptime <name>: String`). Bound
    /// to [`crate::Value::Str`].
    Str,
    /// A `usize`-typed argument (`comptime i: usize`). Bound to
    /// [`crate::Value::Int`] — the index parameter of `field_name_at`
    /// / `field_type_at`.
    Usize,
}

impl BuiltinParamKind {
    /// Surface spelling for diagnostics.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Type => "Type",
            Self::Str => "String",
            Self::Usize => "usize",
        }
    }
}

impl fmt::Display for BuiltinParamKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Return-type tag for a built-in.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum BuiltinReturnKind {
    /// `usize`.
    Usize,
    /// `bool`.
    Bool,
    /// `String` — `field_name_at`.
    Str,
    /// `Type` — `field_type_at`.
    Type,
}

impl BuiltinReturnKind {
    /// Surface spelling for diagnostics.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Usize => "usize",
            Self::Bool => "bool",
            Self::Str => "String",
            Self::Type => "Type",
        }
    }
}

impl fmt::Display for BuiltinReturnKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A built-in's parameter shape and return-type tag.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct BuiltinSignature {
    /// Parameters in declaration order.
    pub params: &'static [BuiltinParamKind],
    /// Return type.
    pub returns: BuiltinReturnKind,
}

impl BuiltinSignature {
    /// Number of parameters this built-in takes.
    pub const fn arity(&self) -> usize {
        self.params.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locked_name_round_trip() {
        for b in Builtin::ALL {
            assert_eq!(builtin_for_name(b.name()), Some(b));
        }
    }

    #[test]
    fn unknown_name_yields_none() {
        assert_eq!(builtin_for_name("size_off"), None);
        assert_eq!(builtin_for_name(""), None);
    }

    #[test]
    fn signature_arities_match_spec() {
        assert_eq!(Builtin::SizeOf.signature().arity(), 1);
        assert_eq!(Builtin::AlignOf.signature().arity(), 1);
        assert_eq!(Builtin::OffsetOf.signature().arity(), 2);
        assert_eq!(Builtin::TargetHas.signature().arity(), 1);
        assert_eq!(Builtin::FieldCount.signature().arity(), 1);
        assert_eq!(Builtin::FieldNameAt.signature().arity(), 2);
        assert_eq!(Builtin::FieldTypeAt.signature().arity(), 2);
        assert_eq!(Builtin::IsSigned.signature().arity(), 1);
        assert_eq!(Builtin::IsUnsigned.signature().arity(), 1);
        assert_eq!(Builtin::IsInteger.signature().arity(), 1);
        assert_eq!(Builtin::IsFloating.signature().arity(), 1);
        assert_eq!(Builtin::IsNumeric.signature().arity(), 1);
        assert_eq!(Builtin::IsPrimitive.signature().arity(), 1);
        assert_eq!(Builtin::IsSum.signature().arity(), 1);
        assert_eq!(Builtin::IsCopy.signature().arity(), 1);
        assert_eq!(Builtin::IsContainer.signature().arity(), 1);
    }
}
