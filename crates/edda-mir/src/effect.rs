//! Effect rows and capability descriptors.
//!
//! Surface effect rows (`with { allocator, err: IoError, ... }`) lower to a
//! body-scoped capability list (each slot bound to a synthetic [`LocalId`]),
//! a list of error ADTs, and a panic bit.

use edda_intern::Symbol;

use crate::ids::{AdtId, EffectId, LocalId};

/// The effect row attached to a [`crate::Body`]: capability slots, possible
/// errors, and a panic bit.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct EffectRow {
    /// Capability slots in calling-convention order.
    pub capabilities: Vec<CapabilitySlot>,
    /// Error ADTs the body may raise via `?` propagation.
    pub errors: Vec<AdtId>,
    /// Whether the body may panic.
    pub has_panic: bool,
}

impl EffectRow {
    /// Construct a pure (capability-free, error-free, no-panic) effect row.
    pub fn pure() -> Self {
        EffectRow {
            capabilities: Vec::new(),
            errors: Vec::new(),
            has_panic: false,
        }
    }
}

impl Default for EffectRow {
    fn default() -> Self {
        EffectRow::pure()
    }
}

/// One capability slot in an [`EffectRow`].
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct CapabilitySlot {
    /// Per-body capability identifier.
    pub id: EffectId,
    /// Synthetic leading-parameter local that holds this capability handle.
    pub param_local: LocalId,
    /// Capability kind (`allocator`, `clock`, ...).
    pub ty: CapabilityKind,
}

/// Kind of capability handle a slot carries.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum CapabilityKind {
    /// Heap allocator (`allocator`).
    Allocator,
    /// Wall-clock / monotonic clock (`clock`).
    Clock,
    /// Filesystem access (`fs`).
    Fs,
    /// Network access (`net`).
    Net,
    /// Environment variables (`env`).
    Env,
    /// Process arguments (`args`).
    Args,
    /// Random number source (`random`).
    Random,
    /// User-defined capability identified by name.
    Named(Symbol),
    /// Value-position capability handle whose type is from the source-level
    /// [`edda_types::CapabilityType`] catalogue. Used by [`lower_ty`] when
    /// projecting a `TyKind::Capability` reached through a field, a
    /// non-effect-row parameter, or a local binding — sites that have a
    /// concrete capability type but no parameter-name `Symbol` to classify
    /// through [`classify_capability`]. The ABI is identical to any other
    /// capability slot (one opaque `ptr`); the discriminator only carries
    /// the source-level type so renderers / debug output can still name it.
    Typed(edda_types::CapabilityType),
}

impl CapabilityKind {
    /// Lowercase capability name. For `Named` slots the result is borrowed
    /// from the supplied interner via the printer; the bare-kind helpers all
    /// return `'static` strings.
    pub fn well_known_str(&self) -> Option<&'static str> {
        match self {
            CapabilityKind::Allocator => Some("allocator"),
            CapabilityKind::Clock => Some("clock"),
            CapabilityKind::Fs => Some("fs"),
            CapabilityKind::Net => Some("net"),
            CapabilityKind::Env => Some("env"),
            CapabilityKind::Args => Some("args"),
            CapabilityKind::Random => Some("random"),
            CapabilityKind::Named(_) => None,
            CapabilityKind::Typed(c) => Some(c.name()),
        }
    }
}

