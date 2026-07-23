//! Opaque `u32` handles for every arena-backed MIR entity.
//!
//! Each `*Id` is a `#[repr(transparent)]` newtype around `u32`. The inner
//! integer is private; the only ways to construct one are the crate-internal
//! `from_raw` constructor used by `IndexVec::push` (via the [`Idx`] impl) and
//! the `DUMMY` sentinels exposed where it makes sense.

/// Build a `*Id` newtype with the standard `Copy + Eq + Hash + Ord + Debug`
/// suite, a private inner `u32`, crate-internal `from_raw`/`as_index` helpers,
/// and an `Idx` impl that routes through them.
macro_rules! define_id {
    ($(#[$attr:meta])* $vis:vis $name:ident, $invariant:expr) => {
        #[doc = $invariant]
        $(#[$attr])*
        #[repr(transparent)]
        #[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
        $vis struct $name(u32);

        impl $name {
            /// Construct from a raw `u32`. Crate-internal: external callers
            /// obtain IDs only via `IndexVec::push`.
            #[allow(dead_code)]
            pub(crate) fn from_raw(index: u32) -> Self {
                $name(index)
            }

            /// Return the inner index as a `usize` for slicing.
            #[allow(dead_code)]
            pub(crate) fn as_index(self) -> usize {
                self.0 as usize
            }

            /// Raw `u32` representation; for pretty-printing and debug only.
            #[inline]
            pub fn as_u32(self) -> u32 {
                self.0
            }
        }

        impl crate::arena::Idx for $name {
            fn new(idx: usize) -> Self {
                let raw = u32::try_from(idx)
                    .expect(concat!(stringify!($name), ": index exceeds u32::MAX"));
                $name(raw)
            }

            fn index(self) -> usize {
                self.0 as usize
            }
        }

        impl crate::arena::sealed::Sealed for $name {}
    };
}

define_id!(
    pub LocalId,
    "Handle into a [`crate::Body`]'s `locals` arena.\n\n\
     **Per-body scope** — a `LocalId` is only meaningful within the `Body` \
     that produced it."
);

define_id!(
    pub BlockId,
    "Handle into a [`crate::Body`]'s `blocks` arena.\n\n\
     **Per-body scope** — a `BlockId` is only meaningful within the `Body` \
     that produced it."
);

define_id!(
    pub BodyId,
    "Handle into a [`crate::MirProgram`]'s `bodies` arena."
);

define_id!(
    pub AdtId,
    "Handle into a [`crate::MirProgram`]'s `adts` arena."
);

define_id!(
    pub ConstId,
    "Handle into a [`crate::MirProgram`]'s `consts` arena."
);

define_id!(
    pub VariantIdx,
    "Index of a variant inside an [`crate::AdtDef`].\n\n\
     **Per-ADT scope** — a `VariantIdx` is only meaningful within the \
     `AdtDef` that produced it."
);

define_id!(
    pub FieldIdx,
    "Index of a field inside a [`crate::VariantDef`].\n\n\
     **Per-variant scope** — a `FieldIdx` is only meaningful within the \
     `VariantDef` that produced it."
);

define_id!(
    pub EffectId,
    "Handle into a [`crate::Body`]'s capability list.\n\n\
     **Per-body scope** — an `EffectId` is only meaningful within the \
     `Body` whose `effect_row` produced it."
);

impl BodyId {
    /// Sentinel value used as a placeholder before a body's true `BodyId` is
    /// known (e.g. forward references during lowering). Indexing a program
    /// with `DUMMY` panics.
    pub const DUMMY: BodyId = BodyId(u32::MAX);
}

impl BlockId {
    /// Sentinel for an unconstructed block reference; debugging only. Indexing
    /// a body's blocks with `DUMMY` panics.
    pub const DUMMY: BlockId = BlockId(u32::MAX);
}

impl LocalId {
    /// The conventional `LocalId` for the return slot in every `Body`.
    pub const RETURN_SLOT: LocalId = LocalId(0);
}
