//! Typechecker-side view of the locked comptime built-in
//! catalogue (§C10).
//!
//! Mirrors `edda_comptime::Builtin`'s name list without dragging the
//! evaluator crate in as a dependency — edda-types cannot depend on
//! edda-comptime because the comptime evaluator depends on edda-types
//! (a cycle). Both crates point at the same canonical names from
//! `corpus/edda-codex/language/04-specs-comptime.md` *Comptime built-ins*
//! and `06-tooling.md` §9 `target_has`. Both lists carry the same
//! fifteen spellings — the layout / target group (`size_of`,
//! `align_of`, `offset_of`, `target_has`) plus the reflective-
//! introspection group (`field_count`, `field_name_at`,
//! `field_type_at`, and the eight `is_*` predicates — `is_sum` landed
//! with the D-22 sum-type READ seam;
//! `is_copy` broadens the bulk-copy fast-path gate to
//! freely-copyable user records).

/// Locked comptime built-in seen by the typechecker.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
#[repr(u8)]
pub enum ComptimeBuiltin {
    /// `size_of(comptime T: Type) -> usize`.
    SizeOf,
    /// `align_of(comptime T: Type) -> usize`.
    AlignOf,
    /// `offset_of(comptime T: Type, comptime field: ident) -> usize`.
    /// Always errors on dispatch until the `@layout` path
    /// lands; the typechecker still types-checks the arg shape so
    /// diagnostics surface uniformly.
    OffsetOf,
    /// `target_has(comptime feature: String) -> bool`.
    TargetHas,
    /// `field_count(comptime T: Type) -> usize`.
    FieldCount,
    /// `field_name_at(comptime T: Type, comptime i: usize) -> String`.
    FieldNameAt,
    /// `field_type_at(comptime T: Type, comptime i: usize) -> Type`.
    FieldTypeAt,
    /// `is_signed(comptime T: Type) -> bool`.
    IsSigned,
    /// `is_unsigned(comptime T: Type) -> bool`.
    IsUnsigned,
    /// `is_integer(comptime T: Type) -> bool`.
    IsInteger,
    /// `is_floating(comptime T: Type) -> bool`.
    IsFloating,
    /// `is_numeric(comptime T: Type) -> bool`.
    IsNumeric,
    /// `is_primitive(comptime T: Type) -> bool`.
    IsPrimitive,
    /// `is_sum(comptime T: Type) -> bool` — sum-type predicate (D-22
    /// READ surface).
    IsSum,
    /// `is_copy(comptime T: Type) -> bool` — freely-copyable predicate
    /// (non-`linear`, non-`affine`, transitively no `linear`/`affine`
    /// field).
    IsCopy,
    /// `is_container(comptime T: Type) -> bool` — spec-instance
    /// container predicate (`Box_*`, `Vec_*`, `Option_*`, map
    /// instances). `true` iff `T` is a type minted by a `spec`
    /// instantiation rather than a source `type` declaration; the
    /// decidable phase-2 signal is a mangled leaf name absent from the
    /// [`super::comptime_expand::ShapeIndex`]. Lets `std` guard the
    /// derived `hash_value`/`compare_value` field-walk away from
    /// container fields whose introspection (`is_sum`/`field_count`/
    /// `discriminant_of`/`v.(i)`) does not fold.
    IsContainer,
}

impl ComptimeBuiltin {
    /// Lowercase spec spelling.
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

    /// Number of arguments this built-in admits at the call site.
    pub const fn arity(self) -> usize {
        match self {
            Self::SizeOf
            | Self::AlignOf
            | Self::TargetHas
            | Self::FieldCount
            | Self::IsSigned
            | Self::IsUnsigned
            | Self::IsInteger
            | Self::IsFloating
            | Self::IsNumeric
            | Self::IsPrimitive
            | Self::IsSum
            | Self::IsCopy
            | Self::IsContainer => 1,
            Self::OffsetOf | Self::FieldNameAt | Self::FieldTypeAt => 2,
        }
    }
}

/// Look up the built-in named `name`. Returns `None` for every other
/// identifier. The reflective-introspection names (`field_count`,
/// `field_name_at`, `field_type_at`, and the seven `is_*` predicates)
/// are callable comptime built-ins alongside the layout / target group.
pub fn comptime_builtin_for_name(name: &str) -> Option<ComptimeBuiltin> {
    Some(match name {
        "size_of" => ComptimeBuiltin::SizeOf,
        "align_of" => ComptimeBuiltin::AlignOf,
        "offset_of" => ComptimeBuiltin::OffsetOf,
        "target_has" => ComptimeBuiltin::TargetHas,
        "field_count" => ComptimeBuiltin::FieldCount,
        "field_name_at" => ComptimeBuiltin::FieldNameAt,
        "field_type_at" => ComptimeBuiltin::FieldTypeAt,
        "is_signed" => ComptimeBuiltin::IsSigned,
        "is_unsigned" => ComptimeBuiltin::IsUnsigned,
        "is_integer" => ComptimeBuiltin::IsInteger,
        "is_floating" => ComptimeBuiltin::IsFloating,
        "is_numeric" => ComptimeBuiltin::IsNumeric,
        "is_primitive" => ComptimeBuiltin::IsPrimitive,
        "is_sum" => ComptimeBuiltin::IsSum,
        "is_copy" => ComptimeBuiltin::IsCopy,
        "is_container" => ComptimeBuiltin::IsContainer,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locked_name_round_trip() {
        for b in [
            ComptimeBuiltin::SizeOf,
            ComptimeBuiltin::AlignOf,
            ComptimeBuiltin::OffsetOf,
            ComptimeBuiltin::TargetHas,
            ComptimeBuiltin::FieldCount,
            ComptimeBuiltin::FieldNameAt,
            ComptimeBuiltin::FieldTypeAt,
            ComptimeBuiltin::IsSigned,
            ComptimeBuiltin::IsUnsigned,
            ComptimeBuiltin::IsInteger,
            ComptimeBuiltin::IsFloating,
            ComptimeBuiltin::IsNumeric,
            ComptimeBuiltin::IsPrimitive,
            ComptimeBuiltin::IsSum,
            ComptimeBuiltin::IsCopy,
            ComptimeBuiltin::IsContainer,
        ] {
            assert_eq!(comptime_builtin_for_name(b.name()), Some(b));
        }
    }

    #[test]
    fn unknown_name_yields_none() {
        assert_eq!(comptime_builtin_for_name("size_off"), None);
        assert_eq!(comptime_builtin_for_name(""), None);
    }

    #[test]
    fn arity_table_matches_signature() {
        assert_eq!(ComptimeBuiltin::SizeOf.arity(), 1);
        assert_eq!(ComptimeBuiltin::AlignOf.arity(), 1);
        assert_eq!(ComptimeBuiltin::OffsetOf.arity(), 2);
        assert_eq!(ComptimeBuiltin::TargetHas.arity(), 1);
        assert_eq!(ComptimeBuiltin::FieldCount.arity(), 1);
        assert_eq!(ComptimeBuiltin::FieldNameAt.arity(), 2);
        assert_eq!(ComptimeBuiltin::FieldTypeAt.arity(), 2);
        assert_eq!(ComptimeBuiltin::IsSigned.arity(), 1);
        assert_eq!(ComptimeBuiltin::IsUnsigned.arity(), 1);
        assert_eq!(ComptimeBuiltin::IsInteger.arity(), 1);
        assert_eq!(ComptimeBuiltin::IsFloating.arity(), 1);
        assert_eq!(ComptimeBuiltin::IsNumeric.arity(), 1);
        assert_eq!(ComptimeBuiltin::IsPrimitive.arity(), 1);
        assert_eq!(ComptimeBuiltin::IsSum.arity(), 1);
        assert_eq!(ComptimeBuiltin::IsCopy.arity(), 1);
        assert_eq!(ComptimeBuiltin::IsContainer.arity(), 1);
    }
}
