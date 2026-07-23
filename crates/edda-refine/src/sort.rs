//! Sorts and the type-level metadata referenced by the predicate IR.
//!
//! Per `docs/types/refinement-decidability.md` §2, the required-decidable
//! fragment is EUF + LIA + boolean + extensional arrays. The sort system
//! mirrors that fragment: integer (sized + signed), boolean, slice (array
//! theory), tuple, record (EUF over user records), and sum (for payload-free
//! variant tag equality).
//!
//! Records, sums, and their fields are referenced by name. v0.1's seam to the
//! typechecker is: the typechecker hands the refine layer a `Predicate`
//! containing [`RecordRef`] / [`FieldRef`] / [`SumRef`] / [`VariantRef`]
//! values whose names match the typechecker's symbol table. The Z3
//! translator consumes those names to declare uninterpreted
//! functions per record-field and per length-slice-element-sort.

use smol_str::SmolStr;

//            (i8..i128, u8..u128, usize, isize) — adding a width is a spec edit
/// Integer-type width. Matches the locked primitive integer set from
/// `docs/syntax/types.md`.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum IntWidth {
    /// 8-bit.
    W8,
    /// 16-bit.
    W16,
    /// 32-bit.
    W32,
    /// 64-bit.
    W64,
    /// 128-bit.
    W128,
    /// Platform pointer-width unsigned (`usize`). Always paired with `signed: false`.
    Usize,
    /// Platform pointer-width signed (`isize`). Always paired with `signed: true`.
    Isize,
}

impl IntWidth {
    /// Lowercase name used in pretty-printed predicates and diagnostics.
    /// Pointer-width variants render as `usize` / `isize` without a numeric
    /// suffix.
    pub const fn name(self) -> &'static str {
        match self {
            IntWidth::W8 => "8",
            IntWidth::W16 => "16",
            IntWidth::W32 => "32",
            IntWidth::W64 => "64",
            IntWidth::W128 => "128",
            IntWidth::Usize => "size",
            IntWidth::Isize => "size",
        }
    }
}

/// Integer sort. Combines a width and a signedness.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct IntSort {
    /// Bit width.
    pub width: IntWidth,
    /// `true` for `i*` / `isize`, `false` for `u*` / `usize`.
    pub signed: bool,
}

impl IntSort {
    /// `usize` — the slice-length / index sort used throughout the array theory.
    pub const USIZE: IntSort = IntSort {
        width: IntWidth::Usize,
        signed: false,
    };
    /// `isize`.
    pub const ISIZE: IntSort = IntSort {
        width: IntWidth::Isize,
        signed: true,
    };

    /// Construct a sized integer sort, e.g. `IntSort::sized(IntWidth::W32, true)` for `i32`.
    pub const fn sized(width: IntWidth, signed: bool) -> IntSort {
        IntSort { width, signed }
    }

    /// Lowercase name used in pretty-printed predicates and diagnostics.
    pub fn type_name(self) -> SmolStr {
        let prefix = if self.signed { "i" } else { "u" };
        let mut name = String::with_capacity(5);
        name.push_str(prefix);
        name.push_str(self.width.name());
        SmolStr::new(name)
    }
}

/// Sort of a value reachable from the predicate IR.
///
/// Field projection in EUF produces sorts pulled from `Record` schemas;
/// slice indexing produces the element sort; `len` always produces
/// [`IntSort::USIZE`]; tag equality always produces [`Sort::Bool`].
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum Sort {
    /// Integer of the given width and signedness.
    Int(IntSort),
    /// Boolean.
    Bool,
    /// Slice `[T]` — the SMT-side array theory's domain. The boxed sort is the
    /// element sort.
    Slice(Box<Sort>),
    /// Tuple `(T1, T2, ...)`. Structurally equality-decomposable.
    Tuple(Vec<Sort>),
    /// User-declared record type. The schema (field names + sorts) is owned
    /// by the typechecker; refine carries only the name.
    Record(RecordRef),
    /// User-declared sum type. Equality at this sort is admitted only via
    /// [`Predicate::TagEq`](crate::Predicate::TagEq) for payload-free variants
    /// per refinement-decidability.md §5.
    Sum(SumRef),
}

impl Sort {
    /// `usize` shorthand.
    pub fn usize() -> Sort {
        Sort::Int(IntSort::USIZE)
    }

    /// `bool` shorthand.
    pub fn bool() -> Sort {
        Sort::Bool
    }

    /// Slice of the given element sort.
    pub fn slice(element: Sort) -> Sort {
        Sort::Slice(Box::new(element))
    }

    /// `true` if this sort admits LIA arithmetic (any [`Sort::Int`]).
    pub fn is_int(&self) -> bool {
        matches!(self, Sort::Int(_))
    }

    /// `true` if equality (`==`, `!=`) is structurally decidable at this sort
    /// per refinement-decidability.md §4. Payload-bearing sums are excluded —
    /// callers route through [`Predicate::TagEq`](crate::Predicate::TagEq).
    pub fn is_equality_bearing(&self) -> bool {
        match self {
            Sort::Int(_) | Sort::Bool => true,
            Sort::Slice(_) => false,
            Sort::Tuple(fields) => fields.iter().all(Self::is_equality_bearing),
            Sort::Record(_) => true,
            Sort::Sum(_) => false,
        }
    }
}

//            table; refine itself stores no field metadata
/// Reference to a record type. The schema (fields + sorts) lives in the
/// typechecker; refine carries only the name so the Z3 translator can declare
/// per-record uninterpreted functions for field projection.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct RecordRef(pub SmolStr);

impl RecordRef {
    /// Construct from any string-like value.
    pub fn new(name: impl Into<SmolStr>) -> RecordRef {
        RecordRef(name.into())
    }

    /// The record's fully-qualified name as known to the typechecker.
    pub fn name(&self) -> &str {
        self.0.as_str()
    }
}

//            function in the Z3 translation: `<record>.<field>: Record(<record>) -> sort`
/// Reference to a record field, used by
/// [`Predicate::FieldProj`](crate::Predicate::FieldProj). Carries the field's
/// projected sort so the Z3 translator can declare the right uninterpreted
/// function without re-querying the typechecker.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct FieldRef {
    /// Record type this field belongs to.
    pub record: RecordRef,
    /// Field name.
    pub field: SmolStr,
    /// Sort of the projected value.
    pub sort: Sort,
}

impl FieldRef {
    /// Convenience constructor.
    pub fn new(record: RecordRef, field: impl Into<SmolStr>, sort: Sort) -> FieldRef {
        FieldRef {
            record,
            field: field.into(),
            sort,
        }
    }
}

/// Reference to a sum type. Like [`RecordRef`], opaque to refine.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct SumRef(pub SmolStr);

impl SumRef {
    /// Construct from any string-like value.
    pub fn new(name: impl Into<SmolStr>) -> SumRef {
        SumRef(name.into())
    }

    /// The sum's fully-qualified name as known to the typechecker.
    pub fn name(&self) -> &str {
        self.0.as_str()
    }
}

//            construction does not reject payload-bearing references, but the
//            Z3 translator rejects them at translation time
/// Reference to a sum variant, used by
/// [`Predicate::TagEq`](crate::Predicate::TagEq). The spec admits tag equality
/// only on payload-free variants; carrying a payload-bearing variant here is a
/// translation-time error, not a construction-time error.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct VariantRef {
    /// Sum type the variant belongs to.
    pub sum: SumRef,
    /// Variant name.
    pub variant: SmolStr,
}

impl VariantRef {
    /// Convenience constructor.
    pub fn new(sum: SumRef, variant: impl Into<SmolStr>) -> VariantRef {
        VariantRef {
            sum,
            variant: variant.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn int_sort_type_name_matches_source_form() {
        assert_eq!(IntSort::sized(IntWidth::W32, true).type_name(), "i32");
        assert_eq!(IntSort::sized(IntWidth::W64, false).type_name(), "u64");
        assert_eq!(IntSort::USIZE.type_name(), "usize");
        assert_eq!(IntSort::ISIZE.type_name(), "isize");
    }

    #[test]
    fn equality_bearing_table_matches_spec() {
        assert!(Sort::Int(IntSort::USIZE).is_equality_bearing());
        assert!(Sort::Bool.is_equality_bearing());
        assert!(!Sort::slice(Sort::Bool).is_equality_bearing());
        assert!(Sort::Tuple(vec![Sort::Bool, Sort::usize()]).is_equality_bearing());
        assert!(!Sort::Tuple(vec![Sort::slice(Sort::Bool)]).is_equality_bearing());
        assert!(Sort::Record(RecordRef::new("Point")).is_equality_bearing());
        assert!(!Sort::Sum(SumRef::new("Connection")).is_equality_bearing());
    }
}
