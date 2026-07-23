//! Place: a description of a memory location reachable by indexing into a
//! local through a sequence of projections.

use crate::ids::{FieldIdx, LocalId, VariantIdx};
use crate::ty::MirType;

/// A place: a root [`LocalId`] plus zero-or-more projections walking into it.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Place {
    /// Root local the place starts from.
    pub local: LocalId,
    /// Projections applied in order to reach the leaf location.
    pub projection: Vec<Projection>,
}

impl Place {
    /// Construct a place that references `local` with no projections.
    pub fn local(local: LocalId) -> Self {
        Place {
            local,
            projection: Vec::new(),
        }
    }
}

/// One step in a [`Place`] projection chain.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum Projection {
    /// `.field` — select a field by index inside the current variant.
    Field(FieldIdx),
    /// `[i]` — slice or array element index by another local.
    Index(LocalId),
    /// Tagged-union downcast: assert and reinterpret as the given variant.
    VariantDowncast(VariantIdx),
    /// `*` — read through a pointer. The current leaf must be a
    /// `HeapPtr`; the projection advances the place to the pointed-to
    /// value of the carried [`MirType`]. Because `HeapPtr` carries no
    /// element type, the leaf type travels with the projection so a
    /// whole aggregate (record / sum / slice) can be loaded through the
    /// pointer, not only a scalar word.
    Deref(MirType),
}
