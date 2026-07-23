//! Operand: an immediate value that feeds into an rvalue, statement, or
//! terminator. Operands are either reads from a [`Place`] or references to
//! interned constants.

use crate::ids::ConstId;
use crate::place::Place;

/// An operand: one of the four atomic value-producers in MIR.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum Operand {
    /// Read the place by-value without consuming it (type must be `Copy`).
    Copy(Place),
    /// Move the place by-value, consuming it.
    Move(Place),
    /// Reference an interned constant by its [`ConstId`].
    Const(ConstId),
    /// The `()` literal.
    Unit,
}
