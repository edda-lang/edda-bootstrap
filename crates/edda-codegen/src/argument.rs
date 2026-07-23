//! Comptime argument tuple: the §3 representation of a spec invocation's
//! arguments, fed into the canonical-form hash.
//!
//! All five argument kinds are locked here: `Type` (`0x01`),
//! `EffectRow` (`0x02`), `Primitive` (`0x03`), `UserDefined` (`0x04`),
//! and `Function` (`0x05`). Per the ordering rule in `storage.md` §6,
//! [`EffectRowArgument`] entries are stored in canonical-row order
//! (err < panic < yield, then lexicographic by payload qualified
//! name); the constructor sorts and dedups on insertion so two
//! source forms that resolve to the same expanded row produce
//! byte-identical hash input.

use smol_str::SmolStr;

//   with the same arguments in different positions produce different hashes
/// Ordered sequence of comptime arguments passed to a spec invocation.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ArgumentTuple {
    args: Box<[Argument]>,
}

impl ArgumentTuple {
    /// The empty tuple — the argument list of a spec invocation that takes
    /// no comptime arguments. Hash-distinct from any non-empty tuple.
    pub fn empty() -> Self {
        ArgumentTuple { args: Box::new([]) }
    }

    /// Construct from an owned argument slice.
    pub fn new(args: impl Into<Box<[Argument]>>) -> Self {
        ArgumentTuple { args: args.into() }
    }

    /// Borrow the underlying argument slice in declaration order.
    pub fn args(&self) -> &[Argument] {
        &self.args
    }

    /// Number of arguments.
    pub fn len(&self) -> usize {
        self.args.len()
    }

    /// `true` if this is the [`empty`](Self::empty) tuple.
    pub fn is_empty(&self) -> bool {
        self.args.is_empty()
    }
}

//   0x01 Type, 0x02 EffectRow, 0x03 Primitive, 0x04 UserDefined, 0x05 Function
/// One element of an [`ArgumentTuple`].
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum Argument {
    /// `storage.md` §3 — Type argument. Serialised as the resolved fully
    /// qualified name of the type (UTF-8 bytes, length-prefixed). For a
    /// primitive type the qualified name is the primitive's name
    /// (`"i32"`, `"bool"`, …); for user-defined or generated types the
    /// qualified name is its declaring-module path joined with the leaf.
    Type(SmolStr),

    /// `storage.md` §3 + §6 — EffectRow argument. Stored in canonical
    /// order (err < panic < yield, then lexicographic by qualified
    /// name); aliases are expected to have been expanded by the caller
    /// per `spec-language.md` §6.
    EffectRow(EffectRowArgument),

    /// `storage.md` §3 — Primitive value argument. Serialised by the
    /// parameter's declared type's fixed-width encoding (little-endian).
    /// `usize`/`isize` normalise to 8 bytes per `PrimitiveValue`.
    Primitive(PrimitiveValue),

    /// `storage.md` §3 — User-defined value argument. The type's
    /// resolved qualified name plus a field-by-field or
    /// variant-tag-plus-payload serialisation, recursing on each
    /// component's kind.
    UserDefined(UserValue),

    /// Function-reference argument.
    /// Serialised as the resolved fully qualified name of the bound
    /// top-level function (UTF-8 bytes, length-prefixed) — the same
    /// shape as [`Argument::Type`] but a distinct kind tag, so a spec
    /// argument that is a function reference content-addresses
    /// distinctly from a same-named type. Bound to a spec parameter
    /// declared `comptime f: function(...) -> ...`; the substitution
    /// walker rewrites in-body calls to `f(..)` into a direct call on
    /// this qualified name.
    Function(SmolStr),
}

impl Argument {
    /// On-disk kind tag from `storage.md` §3 (`Function` = `0x05` was a
    /// later addition; appending a tag keeps every prior tag byte stable).
    pub(crate) const fn kind_tag(&self) -> u8 {
        match self {
            Argument::Type(_) => 0x01,
            Argument::EffectRow(_) => 0x02,
            Argument::Primitive(_) => 0x03,
            Argument::UserDefined(_) => 0x04,
            Argument::Function(_) => 0x05,
        }
    }
}

//   discriminator (Err < Panic < Yield), then by payload qualified
//   name lexicographically. Derived [`Ord`] gives this for free since
//   variant declaration order matches canonical-row order
//   (two `err: T` entries for the same `T` collapse to one)
/// Canonical-ordered [`EffectRowEntry`] sequence.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct EffectRowArgument {
    entries: Box<[EffectRowEntry]>,
}

impl EffectRowArgument {
    /// Construct from any iterable; entries are sorted into canonical
    /// order and consecutive duplicates removed.
    pub fn new(entries: impl IntoIterator<Item = EffectRowEntry>) -> Self {
        let mut sorted: Vec<_> = entries.into_iter().collect();
        sorted.sort();
        sorted.dedup();
        Self {
            entries: sorted.into_boxed_slice(),
        }
    }

    /// The empty row — admissible for specs that take an `EffectRow`
    /// argument bound to an empty `with {}` clause.
    pub fn empty() -> Self {
        Self { entries: Box::new([]) }
    }

    /// Borrow the canonical-ordered entries.
    pub fn entries(&self) -> &[EffectRowEntry] {
        &self.entries
    }

    /// Number of entries (after dedup).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if no entries are present.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

//   order from `storage.md` §6 — derived [`Ord`] depends on this
/// One entry inside an [`EffectRowArgument`]. Capability entries are
/// never admitted (per `spec-language.md` §5: "Capability entries are
/// not admitted in `EffectRow` values").
#[derive(Clone, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum EffectRowEntry {
    /// `err: T` entry. Payload is `T`'s resolved qualified name.
    Err(SmolStr),
    /// `panic` entry — no payload.
    Panic,
    /// `yield: T` entry. Payload is `T`'s resolved qualified name.
    Yield(SmolStr),
}

impl EffectRowEntry {
    /// On-disk sub-tag emitted before the payload bytes.
    pub(crate) const fn sub_tag(&self) -> u8 {
        match self {
            EffectRowEntry::Err(_) => 0x00,
            EffectRowEntry::Panic => 0x01,
            EffectRowEntry::Yield(_) => 0x02,
        }
    }
}

//   depth of the user-defined value's source-level constructor
//   expression (which is bounded by source size)
/// Value of a user-defined comptime argument — either a product-type
/// record literal or a sum-type variant literal.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum UserValue {
    /// Record literal: a named-type plus its field-by-field bindings.
    Record {
        /// The record type's resolved fully qualified name.
        type_qualified: SmolStr,
        /// Field bindings in source-declaration order (which matches
        /// the type declaration's field order).
        fields: Box<[UserField]>,
    },
    /// Variant literal: a named sum type plus the active variant tag
    /// and its payload.
    Variant {
        /// The sum type's resolved fully qualified name.
        type_qualified: SmolStr,
        /// The active variant's leaf name (`red`, `rgb`, …).
        variant_tag: SmolStr,
        /// The variant's payload (unit / tuple / struct).
        payload: UserVariantPayload,
    },
}

impl UserValue {
    /// On-disk shape tag emitted after the kind byte: `0x00` Record,
    /// `0x01` Variant.
    pub(crate) const fn shape_tag(&self) -> u8 {
        match self {
            UserValue::Record { .. } => 0x00,
            UserValue::Variant { .. } => 0x01,
        }
    }
}

/// A single field binding inside a [`UserValue::Record`] or a
/// [`UserVariantPayload::Struct`].
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct UserField {
    /// The field's declared name.
    pub name: SmolStr,
    /// The bound value — recurses on its argument kind.
    pub value: Argument,
}

impl UserField {
    /// Construct from a name and a value.
    pub fn new(name: impl Into<SmolStr>, value: Argument) -> Self {
        Self {
            name: name.into(),
            value,
        }
    }
}

//   Tuple=1, Struct=2 — and overlap with neither the [`variant_payload`]
//   AST tag family nor the [`variant_pat_payload`] family (these are
//   ARGUMENT bytes; the AST bytes have separate tag spaces)
/// Payload of a [`UserValue::Variant`].
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum UserVariantPayload {
    /// `case red` — no payload.
    Unit,
    /// `case rgb(255, 0, 0)` — positional payload.
    Tuple(Box<[Argument]>),
    /// `case rgb { r: 255, g: 0, b: 0 }` — named payload.
    Struct(Box<[UserField]>),
}

impl UserVariantPayload {
    /// On-disk sub-tag emitted before payload bytes.
    pub(crate) const fn sub_tag(&self) -> u8 {
        match self {
            UserVariantPayload::Unit => 0x00,
            UserVariantPayload::Tuple(_) => 0x01,
            UserVariantPayload::Struct(_) => 0x02,
        }
    }
}

//   `usize`/`isize` always serialise as 8 bytes, independent of the host
/// Concrete value of a primitive comptime argument.
///
/// Each variant pairs a primitive type tag with its value. The encoder
/// uses the variant to pick the on-disk width:
///
/// | Variant      | Sub-tag | Width (LE)  |
/// |--------------|---------|-------------|
/// | `U8`         | `0x01`  | 1 byte      |
/// | `U16`        | `0x02`  | 2 bytes     |
/// | `U32`        | `0x03`  | 4 bytes     |
/// | `U64`        | `0x04`  | 8 bytes     |
/// | `USize`      | `0x05`  | 8 bytes     |
/// | `I8`         | `0x06`  | 1 byte      |
/// | `I16`        | `0x07`  | 2 bytes     |
/// | `I32`        | `0x08`  | 4 bytes     |
/// | `I64`        | `0x09`  | 8 bytes     |
/// | `ISize`      | `0x0a`  | 8 bytes     |
/// | `Bool`       | `0x0b`  | 1 byte      |
/// | `String`     | `0x0c`  | u32 length + UTF-8 |
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum PrimitiveValue {
    /// `u8` parameter.
    U8(u8),
    /// `u16` parameter.
    U16(u16),
    /// `u32` parameter.
    U32(u32),
    /// `u64` parameter.
    U64(u64),
    /// `usize` parameter; width normalised to 8 bytes regardless of host.
    USize(u64),
    /// `i8` parameter.
    I8(i8),
    /// `i16` parameter.
    I16(i16),
    /// `i32` parameter.
    I32(i32),
    /// `i64` parameter.
    I64(i64),
    /// `isize` parameter; width normalised to 8 bytes regardless of host.
    ISize(i64),
    /// `bool` parameter.
    Bool(bool),
    /// `String` parameter — static-backed UTF-8 only, per `spec-language.md` §3.
    String(SmolStr),
}

impl PrimitiveValue {
    /// On-disk sub-tag from the table above.
    pub(crate) const fn sub_tag(&self) -> u8 {
        match self {
            PrimitiveValue::U8(_) => 0x01,
            PrimitiveValue::U16(_) => 0x02,
            PrimitiveValue::U32(_) => 0x03,
            PrimitiveValue::U64(_) => 0x04,
            PrimitiveValue::USize(_) => 0x05,
            PrimitiveValue::I8(_) => 0x06,
            PrimitiveValue::I16(_) => 0x07,
            PrimitiveValue::I32(_) => 0x08,
            PrimitiveValue::I64(_) => 0x09,
            PrimitiveValue::ISize(_) => 0x0a,
            PrimitiveValue::Bool(_) => 0x0b,
            PrimitiveValue::String(_) => 0x0c,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_tuple_is_empty() {
        assert!(ArgumentTuple::empty().is_empty());
        assert_eq!(ArgumentTuple::empty().len(), 0);
        assert_eq!(ArgumentTuple::empty().args(), &[]);
    }

    #[test]
    fn kind_tags_match_storage_spec() {
        assert_eq!(Argument::Type(SmolStr::new("i32")).kind_tag(), 0x01);
        assert_eq!(
            Argument::EffectRow(EffectRowArgument::empty()).kind_tag(),
            0x02,
        );
        assert_eq!(
            Argument::Primitive(PrimitiveValue::U32(0)).kind_tag(),
            0x03,
        );
        assert_eq!(
            Argument::UserDefined(UserValue::Record {
                type_qualified: SmolStr::new("foo.Bar"),
                fields: Box::new([]),
            })
            .kind_tag(),
            0x04,
        );
    }

    #[test]
    fn effect_row_entry_sub_tags() {
        assert_eq!(EffectRowEntry::Err(SmolStr::new("E")).sub_tag(), 0x00);
        assert_eq!(EffectRowEntry::Panic.sub_tag(), 0x01);
        assert_eq!(EffectRowEntry::Yield(SmolStr::new("T")).sub_tag(), 0x02);
    }

    #[test]
    fn effect_row_sorts_by_kind_then_qname() {
        // Insertion order: Yield(z), Err(b), Panic, Err(a), Yield(a)
        let row = EffectRowArgument::new([
            EffectRowEntry::Yield(SmolStr::new("z")),
            EffectRowEntry::Err(SmolStr::new("b")),
            EffectRowEntry::Panic,
            EffectRowEntry::Err(SmolStr::new("a")),
            EffectRowEntry::Yield(SmolStr::new("a")),
        ]);
        // Expected canonical order:
        //   Err(a), Err(b), Panic, Yield(a), Yield(z)
        let expected = vec![
            EffectRowEntry::Err(SmolStr::new("a")),
            EffectRowEntry::Err(SmolStr::new("b")),
            EffectRowEntry::Panic,
            EffectRowEntry::Yield(SmolStr::new("a")),
            EffectRowEntry::Yield(SmolStr::new("z")),
        ];
        assert_eq!(row.entries(), expected.as_slice());
    }

    #[test]
    fn effect_row_dedups_duplicates() {
        let row = EffectRowArgument::new([
            EffectRowEntry::Err(SmolStr::new("E")),
            EffectRowEntry::Err(SmolStr::new("E")),
            EffectRowEntry::Panic,
            EffectRowEntry::Panic,
        ]);
        assert_eq!(row.len(), 2);
    }

    #[test]
    fn effect_row_input_order_does_not_change_equality() {
        let a = EffectRowArgument::new([
            EffectRowEntry::Err(SmolStr::new("X")),
            EffectRowEntry::Yield(SmolStr::new("Y")),
            EffectRowEntry::Panic,
        ]);
        let b = EffectRowArgument::new([
            EffectRowEntry::Yield(SmolStr::new("Y")),
            EffectRowEntry::Panic,
            EffectRowEntry::Err(SmolStr::new("X")),
        ]);
        assert_eq!(a, b);
    }

    #[test]
    fn user_value_shape_tags() {
        let rec = UserValue::Record {
            type_qualified: SmolStr::new("foo.Point"),
            fields: Box::new([]),
        };
        let var = UserValue::Variant {
            type_qualified: SmolStr::new("foo.Color"),
            variant_tag: SmolStr::new("red"),
            payload: UserVariantPayload::Unit,
        };
        assert_eq!(rec.shape_tag(), 0x00);
        assert_eq!(var.shape_tag(), 0x01);
    }

    #[test]
    fn user_variant_payload_sub_tags() {
        assert_eq!(UserVariantPayload::Unit.sub_tag(), 0x00);
        assert_eq!(UserVariantPayload::Tuple(Box::new([])).sub_tag(), 0x01);
        assert_eq!(UserVariantPayload::Struct(Box::new([])).sub_tag(), 0x02);
    }

    #[test]
    fn primitive_sub_tags_are_contiguous_and_locked() {
        // Lock the wire format — these are part of BodyVersion(0x01).
        assert_eq!(PrimitiveValue::U8(0).sub_tag(), 0x01);
        assert_eq!(PrimitiveValue::U16(0).sub_tag(), 0x02);
        assert_eq!(PrimitiveValue::U32(0).sub_tag(), 0x03);
        assert_eq!(PrimitiveValue::U64(0).sub_tag(), 0x04);
        assert_eq!(PrimitiveValue::USize(0).sub_tag(), 0x05);
        assert_eq!(PrimitiveValue::I8(0).sub_tag(), 0x06);
        assert_eq!(PrimitiveValue::I16(0).sub_tag(), 0x07);
        assert_eq!(PrimitiveValue::I32(0).sub_tag(), 0x08);
        assert_eq!(PrimitiveValue::I64(0).sub_tag(), 0x09);
        assert_eq!(PrimitiveValue::ISize(0).sub_tag(), 0x0a);
        assert_eq!(PrimitiveValue::Bool(false).sub_tag(), 0x0b);
        assert_eq!(PrimitiveValue::String(SmolStr::new("")).sub_tag(), 0x0c);
    }
}
