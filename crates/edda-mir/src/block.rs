//! Basic block: a flat list of statements followed by exactly one terminator.

use crate::statement::Statement;
use crate::terminator::Terminator;

/// A basic block: zero-or-more statements then exactly one terminator.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct BasicBlockData {
    /// Statements in execution order.
    pub stmts: Vec<Statement>,
    /// Mandatory terminator.
    pub terminator: Terminator,
}
