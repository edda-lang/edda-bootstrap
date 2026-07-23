//! Primitive type catalogue.
//!
//! Per `docs/syntax/types.md`, the locked set is: signed integers
//! `i8..i128` plus pointer-sized `isize`; unsigned integers `u8..u128`
//! plus pointer-sized `usize`; IEEE-754 floats `f32`/`f64`; `bool`; the
//! unit type `()`; the bottom type `never`; the comptime meta-type
//! `Type`; and the owned-string type `String`; the opaque heap pointer `HeapPtr`; and the Unicode scalar `Codepoint`. The catalogue is locked
//! at the spec level — new primitives require explicit revision.

use std::fmt;

/// A built-in primitive type, per the locked catalogue.
///
/// `Primitive` is the *kind* of a built-in type; the actual interned
/// [`TyId`](crate::TyId) for any primitive is obtained from
/// [`TyInterner::prim`](crate::TyInterner::prim). Variants are ordered
/// to match the catalogue tables in `docs/syntax/types.md` — signed ints
/// first, then unsigned, then pointer-sized, then floats, then `bool`,
/// `()`, `never`, `Type`, `String`, `HeapPtr`, `Codepoint`. Reordering would break the
/// pre-allocated handle table inside the interner.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
#[repr(u8)]
pub enum Primitive {
    /// 8-bit signed integer.
    I8 = 0,
    /// 16-bit signed integer.
    I16 = 1,
    /// 32-bit signed integer.
    I32 = 2,
    /// 64-bit signed integer (default for unannotated integer literals).
    I64 = 3,
    /// 128-bit signed integer.
    I128 = 4,
    /// Pointer-width signed integer.
    Isize = 5,
    /// 8-bit unsigned integer.
    U8 = 6,
    /// 16-bit unsigned integer.
    U16 = 7,
    /// 32-bit unsigned integer.
    U32 = 8,
    /// 64-bit unsigned integer.
    U64 = 9,
    /// 128-bit unsigned integer.
    U128 = 10,
    /// Pointer-width unsigned integer (canonical type for indices,
    /// lengths, capacities — `usize`).
    Usize = 11,
    /// IEEE-754 single-precision float.
    F32 = 12,
    /// IEEE-754 double-precision float (default for unannotated float literals).
    F64 = 13,
    /// Boolean — `true` or `false`. No implicit integer conversion.
    Bool = 14,
    /// Unit type `()` — sole inhabitant is `()`. Used for functions
    /// returning nothing, statement expressions, side-effecting results.
    Unit = 15,
    /// Bottom type `never` — uninhabited, the type of any expression
    /// that does not return to its surrounding context. Unifies with
    /// any other type at branch joins.
    Never = 16,
    /// Comptime meta-type `Type` — values are themselves types. No
    /// runtime representation; `Type`-typed bindings exist only at
    /// compile time. CamelCase, deliberately distinct from the
    /// lowercase keyword `type` used for declarations.
    Type = 17,
    /// Owned UTF-8 string `String`. Heap-allocated when materialised
    /// from `f"..."` interpolation; static-backed when from a plain
    /// `"..."` literal. CamelCase because it interacts with allocators
    /// — concealing that under a lowercase `string` would hide the
    /// capability story.
    String = 18,
    /// Opaque raw heap pointer — the implementation backing for
    /// `Box(T)` and the receiver of allocator intrinsics. Pointer-width
    /// on every target; carries no element type at the source level
    /// (size/align of the pointee are passed explicitly to the
    /// allocation intrinsics). CamelCase because it is spelled in source
    /// only by stdlib internals (`stdlib/mem/alloc/src/alloc.ea`); user code never
    /// names it directly.
    HeapPtr = 19,
    /// Unicode scalar value — one Unicode codepoint in `0..0xD800 ∪
    /// 0xE000..0x110000`. 4-byte unsigned representation (matches
    /// [`crate::mir_prim::MirPrim::Char`] downstream). CamelCase because
    /// the V1.0 surface spells it `Codepoint` per CLAUDE.md "Primitive
    /// types". Appended at the end of the enum so existing discriminants
    /// for `I8..HeapPtr` are unchanged — wire-load-bearing per RISKS.md.
    Codepoint = 20,
}

/// Number of [`Primitive`] variants. Used by [`TyInterner`] to size its
/// pre-allocated handle table; must stay in sync with the variant set.
pub(crate) const PRIM_COUNT: usize = 21;

impl Primitive {
    /// All [`Primitive`] variants, in discriminant order.
    ///
    /// Order matches the interner's pre-allocated handle table — the
    /// `i`th element here has discriminant `i` and occupies slot `i` in
    /// [`TyInterner::prims`](crate::TyInterner).
    pub const ALL: [Primitive; PRIM_COUNT] = [
        Primitive::I8,
        Primitive::I16,
        Primitive::I32,
        Primitive::I64,
        Primitive::I128,
        Primitive::Isize,
        Primitive::U8,
        Primitive::U16,
        Primitive::U32,
        Primitive::U64,
        Primitive::U128,
        Primitive::Usize,
        Primitive::F32,
        Primitive::F64,
        Primitive::Bool,
        Primitive::Unit,
        Primitive::Never,
        Primitive::Type,
        Primitive::String,
        Primitive::HeapPtr,
        Primitive::Codepoint,
    ];

    /// The source spelling of this primitive (`"i32"`, `"()"`, `"never"`,
    /// `"Type"`, `"String"`, …).
    ///
    /// Used by diagnostic rendering and by the AST-lowering pass.
    /// The strings are spec-locked — changing one is a user-visible break.
    pub const fn name(self) -> &'static str {
        match self {
            Primitive::I8 => "i8",
            Primitive::I16 => "i16",
            Primitive::I32 => "i32",
            Primitive::I64 => "i64",
            Primitive::I128 => "i128",
            Primitive::Isize => "isize",
            Primitive::U8 => "u8",
            Primitive::U16 => "u16",
            Primitive::U32 => "u32",
            Primitive::U64 => "u64",
            Primitive::U128 => "u128",
            Primitive::Usize => "usize",
            Primitive::F32 => "f32",
            Primitive::F64 => "f64",
            Primitive::Bool => "bool",
            Primitive::Unit => "()",
            Primitive::Never => "never",
            Primitive::Type => "Type",
            Primitive::String => "String",
            Primitive::HeapPtr => "HeapPtr",
            Primitive::Codepoint => "Codepoint",
        }
    }

    /// Reverse of [`Primitive::name`]: parse a primitive from its source
    /// spelling. Returns `None` for any input outside the locked catalogue.
    pub fn from_name(s: &str) -> Option<Primitive> {
        // Linear scan — the catalogue is small; a switch compiles to the
        // same shape and a HashMap costs more than it saves.
        Primitive::ALL.into_iter().find(|p| p.name() == s)
    }

    /// True for the signed integer types `i8..i128` and `isize`.
    pub const fn is_signed_integer(self) -> bool {
        matches!(
            self,
            Primitive::I8
                | Primitive::I16
                | Primitive::I32
                | Primitive::I64
                | Primitive::I128
                | Primitive::Isize
        )
    }

    /// True for the unsigned integer types `u8..u128` and `usize`.
    pub const fn is_unsigned_integer(self) -> bool {
        matches!(
            self,
            Primitive::U8
                | Primitive::U16
                | Primitive::U32
                | Primitive::U64
                | Primitive::U128
                | Primitive::Usize
        )
    }

    /// True for any integer primitive, signed or unsigned.
    ///
    /// Equivalent to [`Primitive::is_signed_integer`] OR
    /// [`Primitive::is_unsigned_integer`]. Used by `T-IntLit-Check` and
    /// `T-BinaryArith-Int` from `docs/types/inference-rules.md §1a`.
    pub const fn is_integer(self) -> bool {
        self.is_signed_integer() || self.is_unsigned_integer()
    }

    /// True for `f32` and `f64`.
    pub const fn is_float(self) -> bool {
        matches!(self, Primitive::F32 | Primitive::F64)
    }

    /// True for any numeric primitive (integer or float).
    pub const fn is_numeric(self) -> bool {
        self.is_integer() || self.is_float()
    }

    /// True when this primitive's drop is a no-op — every integer
    /// width, both floats, `bool`, `()`, `never`, the comptime
    /// `Type` meta-primitive, and `Codepoint`. False for the heap-owned
    /// `String` and the opaque `HeapPtr` (silently dropping a `HeapPtr`
    /// would leak its allocation).
    ///
    /// Consulted by the typecheck mode tracker at function exit to
    /// admit the canonical "consume some fields, drop the rest"
    /// destructor pattern on `take`-mode parameters whose remaining-
    /// valid fields are stack-only scalars.
    pub const fn is_trivially_droppable(self) -> bool {
        match self {
            Primitive::I8
            | Primitive::I16
            | Primitive::I32
            | Primitive::I64
            | Primitive::I128
            | Primitive::Isize
            | Primitive::U8
            | Primitive::U16
            | Primitive::U32
            | Primitive::U64
            | Primitive::U128
            | Primitive::Usize
            | Primitive::F32
            | Primitive::F64
            | Primitive::Bool
            | Primitive::Unit
            | Primitive::Never
            | Primitive::Type
            | Primitive::Codepoint => true,
            Primitive::String | Primitive::HeapPtr => false,
        }
    }

    /// Discriminant as a `usize`, suitable for indexing the interner's
    /// pre-allocated `prims` table.
    #[inline]
    pub(crate) const fn as_index(self) -> usize {
        self as usize
    }
}

impl fmt::Display for Primitive {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_has_prim_count_entries() {
        assert_eq!(Primitive::ALL.len(), PRIM_COUNT);
    }

    #[test]
    fn all_is_in_discriminant_order() {
        for (i, p) in Primitive::ALL.iter().enumerate() {
            assert_eq!(p.as_index(), i, "Primitive::ALL[{i}] has wrong discriminant");
        }
    }

    #[test]
    fn name_round_trips() {
        for p in Primitive::ALL {
            assert_eq!(Primitive::from_name(p.name()), Some(p));
        }
    }

    #[test]
    fn from_name_rejects_outsiders() {
        assert_eq!(Primitive::from_name(""), None);
        assert_eq!(Primitive::from_name("int"), None);
        assert_eq!(Primitive::from_name("i31"), None);
        assert_eq!(Primitive::from_name("I32"), None); // case-sensitive
        assert_eq!(Primitive::from_name("()"), Some(Primitive::Unit));
    }

    #[test]
    fn integer_predicates_partition_correctly() {
        let signed = [
            Primitive::I8,
            Primitive::I16,
            Primitive::I32,
            Primitive::I64,
            Primitive::I128,
            Primitive::Isize,
        ];
        let unsigned = [
            Primitive::U8,
            Primitive::U16,
            Primitive::U32,
            Primitive::U64,
            Primitive::U128,
            Primitive::Usize,
        ];
        for p in signed {
            assert!(p.is_signed_integer(), "{p:?} should be signed");
            assert!(!p.is_unsigned_integer(), "{p:?} should not be unsigned");
            assert!(p.is_integer(), "{p:?} should be integer");
            assert!(p.is_numeric(), "{p:?} should be numeric");
            assert!(!p.is_float(), "{p:?} should not be float");
        }
        for p in unsigned {
            assert!(!p.is_signed_integer(), "{p:?} should not be signed");
            assert!(p.is_unsigned_integer(), "{p:?} should be unsigned");
            assert!(p.is_integer(), "{p:?} should be integer");
            assert!(p.is_numeric(), "{p:?} should be numeric");
            assert!(!p.is_float(), "{p:?} should not be float");
        }
    }

    #[test]
    fn float_predicates_partition_correctly() {
        for p in [Primitive::F32, Primitive::F64] {
            assert!(p.is_float());
            assert!(p.is_numeric());
            assert!(!p.is_integer());
        }
    }

    #[test]
    fn non_numeric_predicates() {
        for p in [
            Primitive::Bool,
            Primitive::Unit,
            Primitive::Never,
            Primitive::Type,
            Primitive::String,
        ] {
            assert!(!p.is_integer());
            assert!(!p.is_float());
            assert!(!p.is_numeric());
        }
    }

    #[test]
    fn display_matches_name() {
        assert_eq!(Primitive::I32.to_string(), "i32");
        assert_eq!(Primitive::Unit.to_string(), "()");
        assert_eq!(Primitive::Never.to_string(), "never");
        assert_eq!(Primitive::Type.to_string(), "Type");
        assert_eq!(Primitive::String.to_string(), "String");
    }
}
