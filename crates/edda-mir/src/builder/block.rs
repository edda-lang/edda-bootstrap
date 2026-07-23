//! `BlockBuilder`: the consume-on-finish per-block builder.
//!
//! A `BlockBuilder` reserves a [`BlockId`] at construction (by appending a
//! placeholder [`BasicBlockData`] with `Terminator::Unreachable`) so callers
//! can target the not-yet-sealed block from another block's terminator. The
//! terminator sealer methods consume the builder, overwriting the placeholder
//! at the reserved id.

use edda_span::Span;

use crate::block::BasicBlockData;
use crate::body::Body;
use crate::ids::{AdtId, BlockId, EffectId, LocalId, VariantIdx};
use crate::operand::Operand;
use crate::place::Place;
use crate::rvalue::Rvalue;
use crate::statement::{Statement, StatementKind};
use crate::terminator::{CallArg, FuncRef, Terminator, TerminatorKind};

/// Builder for a single basic block.
///
/// Constructed via [`crate::builder::BodyBuilder::block`]. Drop without
/// sealing leaves an `Unreachable` placeholder in `body.blocks` at the
/// reserved id — fine for a forgiving builder, but the `#[must_use]`
/// attribute discourages it.
#[must_use = "BlockBuilder must be terminated; otherwise the block keeps its Unreachable placeholder"]
pub struct BlockBuilder<'a> {
    body: &'a mut Body,
    id: BlockId,
    stmts: Vec<Statement>,
}

impl<'a> BlockBuilder<'a> {
    /// Reserve a fresh `BlockId` by installing a placeholder
    /// `Terminator::Unreachable` block. Crate-internal: callers use
    /// `BodyBuilder::block`.
    pub(super) fn reserve(body: &'a mut Body) -> Self {
        let placeholder = BasicBlockData {
            stmts: Vec::new(),
            terminator: Terminator {
                span: Span::DUMMY,
                kind: TerminatorKind::Unreachable,
            },
        };
        let id = body.blocks.push(placeholder);
        BlockBuilder {
            body,
            id,
            stmts: Vec::new(),
        }
    }

    /// The reserved [`BlockId`]. Stable across the builder's lifetime.
    pub fn id(&self) -> BlockId {
        self.id
    }

    // -- statement helpers --------------------------------------------------

    /// Append an `Assign { place, rvalue }` statement.
    pub fn assign(&mut self, span: Span, place: Place, rvalue: Rvalue) {
        self.stmts.push(Statement {
            span,
            kind: StatementKind::Assign { place, rvalue },
        });
    }

    /// Append a `StorageLive(local)` statement.
    pub fn storage_live(&mut self, span: Span, local: LocalId) {
        self.stmts.push(Statement {
            span,
            kind: StatementKind::StorageLive(local),
        });
    }

    /// Append a `StorageDead(local)` statement.
    pub fn storage_dead(&mut self, span: Span, local: LocalId) {
        self.stmts.push(Statement {
            span,
            kind: StatementKind::StorageDead(local),
        });
    }

    /// Append a `SetInit(local)` statement (post-`init`-mode init).
    pub fn set_init(&mut self, span: Span, local: LocalId) {
        self.stmts.push(Statement {
            span,
            kind: StatementKind::SetInit(local),
        });
    }

    /// Append a `Drop(local)` statement. The `_local` suffix avoids the
    /// reserved-keyword collision with `drop`.
    pub fn drop_local(&mut self, span: Span, local: LocalId) {
        self.stmts.push(Statement {
            span,
            kind: StatementKind::Drop(local),
        });
    }

    /// Append a `Nop` statement.
    pub fn nop(&mut self, span: Span) {
        self.stmts.push(Statement {
            span,
            kind: StatementKind::Nop,
        });
    }

    /// Append a pre-built [`Statement`] without going through a helper.
    pub fn push(&mut self, stmt: Statement) {
        self.stmts.push(stmt);
    }

    // -- terminator sealers -------------------------------------------------

    /// Seal with `Return(value)`.
    pub fn return_(self, span: Span, value: Operand) -> BlockId {
        self.seal(Terminator {
            span,
            kind: TerminatorKind::Return(value),
        })
    }

    /// Seal with `Goto(target)`.
    pub fn goto(self, span: Span, target: BlockId) -> BlockId {
        self.seal(Terminator {
            span,
            kind: TerminatorKind::Goto(target),
        })
    }

    /// Seal with `SwitchBool { cond, true_bb, false_bb }`.
    pub fn switch_bool(
        self,
        span: Span,
        cond: Operand,
        true_bb: BlockId,
        false_bb: BlockId,
    ) -> BlockId {
        self.seal(Terminator {
            span,
            kind: TerminatorKind::SwitchBool {
                cond,
                true_bb,
                false_bb,
            },
        })
    }

    /// Seal with `SwitchTag { subject, adt, arms, otherwise }`.
    pub fn switch_tag(
        self,
        span: Span,
        subject: Operand,
        adt: AdtId,
        arms: Vec<(VariantIdx, BlockId)>,
        otherwise: BlockId,
    ) -> BlockId {
        self.seal(Terminator {
            span,
            kind: TerminatorKind::SwitchTag {
                subject,
                adt,
                arms,
                otherwise,
            },
        })
    }

    /// Seal with `Call { ... }`. Capabilities thread accounting-only
    /// (`ThreadedCapability::slot`); build the terminator by hand for
    /// positional `value_arg` pairing.
    #[allow(clippy::too_many_arguments)]
    pub fn call(
        self,
        span: Span,
        func: FuncRef,
        args: Vec<CallArg>,
        capabilities: Vec<EffectId>,
        destination: Place,
        target: BlockId,
        on_error: Option<BlockId>,
    ) -> BlockId {
        self.seal(Terminator {
            span,
            kind: TerminatorKind::Call {
                func,
                args,
                capabilities: capabilities
                    .into_iter()
                    .map(crate::terminator::ThreadedCapability::slot)
                    .collect(),
                destination,
                target,
                on_error,
            },
        })
    }

    /// Seal with `Raise { err_adt, value }`.
    pub fn raise(self, span: Span, err_adt: AdtId, value: Operand) -> BlockId {
        self.seal(Terminator {
            span,
            kind: TerminatorKind::Raise { err_adt, value },
        })
    }

    /// Seal with `Panic { msg }`.
    pub fn panic(self, span: Span, msg: Operand) -> BlockId {
        self.seal(Terminator {
            span,
            kind: TerminatorKind::Panic { msg },
        })
    }

    /// Seal with `Unreachable`.
    pub fn unreachable(self, span: Span) -> BlockId {
        self.seal(Terminator {
            span,
            kind: TerminatorKind::Unreachable,
        })
    }

    /// Seal with a pre-built [`Terminator`] without going through a helper.
    pub fn terminate(self, term: Terminator) -> BlockId {
        self.seal(term)
    }

    /// Install the accumulated statements + terminator at the reserved id.
    fn seal(self, terminator: Terminator) -> BlockId {
        let BlockBuilder { body, id, stmts } = self;
        body.blocks[id] = BasicBlockData { stmts, terminator };
        id
    }
}
