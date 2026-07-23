//! Statements: the non-terminator entries inside a basic block.
//!
//! Statements never alter control flow — that is the [`crate::Terminator`]'s
//! job. Every basic block is a flat sequence of statements followed by exactly
//! one terminator.

use edda_span::Span;

use crate::ids::LocalId;
use crate::place::Place;
use crate::rvalue::Rvalue;

/// A statement: source span plus variant.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Statement {
    /// Source span of the originating expression / declaration.
    pub span: Span,
    /// Variant and payload.
    pub kind: StatementKind,
}

/// Every statement form.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum StatementKind {
    /// Write `rvalue` into `place`.
    Assign {
        /// Destination.
        place: Place,
        /// Right-hand side.
        rvalue: Rvalue,
    },
    /// Begin the live range of a local.
    StorageLive(LocalId),
    /// End the live range of a local.
    StorageDead(LocalId),
    /// Mark an `init`-mode destination as initialised after a write.
    SetInit(LocalId),
    /// Drop the value at `local` (runs its destructor if any).
    Drop(LocalId),
    /// No-op statement used by lowering passes to preserve source position.
    Nop,
}
