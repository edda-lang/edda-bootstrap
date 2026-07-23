//! [`ValidationError`] — problems detected by [`crate::validate`].
//!
//! Each variant names the offending body, block, and any per-entity index so a
//! caller can resolve back to the originating `MirProgram` for span-aware
//! rendering. Validation is structural — no `Span` is carried directly.

use crate::adt::AdtKind;
use crate::ids::{AdtId, BlockId, BodyId, FieldIdx, LocalId, VariantIdx};

/// Problems detected by [`crate::validate`].
///
/// Each variant names the offending body, block, and any per-entity index so a
/// caller can resolve back to the originating `MirProgram` for span-aware
/// rendering. Validation is structural — no `Span` is carried directly.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum ValidationError {
    /// A body's `entry` block id is outside `body.blocks`.
    BodyEntryDangling {
        /// Offending body.
        body: BodyId,
        /// The dangling entry block id.
        entry: BlockId,
    },
    /// A terminator names a successor block that is outside `body.blocks`.
    BlockSuccessorDangling {
        /// Offending body.
        body: BodyId,
        /// Block whose terminator carries the dangling successor.
        block: BlockId,
        /// The dangling successor block id.
        successor: BlockId,
    },
    /// A statement at `stmt_index` references a local outside `body.locals`.
    StatementLocalDangling {
        /// Offending body.
        body: BodyId,
        /// Block containing the statement.
        block: BlockId,
        /// Statement index within the block.
        stmt_index: usize,
        /// The dangling local id.
        local: LocalId,
    },
    /// A terminator references a local outside `body.locals`.
    TerminatorLocalDangling {
        /// Offending body.
        body: BodyId,
        /// Block whose terminator carries the dangling local.
        block: BlockId,
        /// The dangling local id.
        local: LocalId,
    },
    /// `SwitchTag` named an ADT whose kind is not `Sum`.
    SwitchTagAdtKindMismatch {
        /// Offending body.
        body: BodyId,
        /// Offending block.
        block: BlockId,
        /// The ADT named.
        adt: AdtId,
        /// The ADT's actual kind.
        found: AdtKind,
    },
    /// `SwitchTag` arm references a variant outside the ADT's variant list.
    SwitchTagVariantDangling {
        /// Offending body.
        body: BodyId,
        /// Offending block.
        block: BlockId,
        /// The ADT named.
        adt: AdtId,
        /// The dangling variant index.
        variant: VariantIdx,
    },
    /// `SwitchTag` arms contain the same variant more than once.
    SwitchTagDuplicateArm {
        /// Offending body.
        body: BodyId,
        /// Offending block.
        block: BlockId,
        /// The duplicated variant index.
        variant: VariantIdx,
    },
    /// `MakeVariant` was used with an ADT whose kind is `Product`.
    MakeVariantOnProduct {
        /// Offending body.
        body: BodyId,
        /// Offending block.
        block: BlockId,
        /// The ADT named.
        adt: AdtId,
    },
    /// `MakeRecord` was used with an ADT whose kind is `Sum`.
    MakeRecordOnSum {
        /// Offending body.
        body: BodyId,
        /// Offending block.
        block: BlockId,
        /// The ADT named.
        adt: AdtId,
    },
    /// A field index or count is outside the target variant's `fields` range.
    FieldIndexOutOfRange {
        /// Offending body.
        body: BodyId,
        /// Offending block.
        block: BlockId,
        /// The ADT named.
        adt: AdtId,
        /// The variant index (None for products).
        variant: Option<VariantIdx>,
        /// The offending field index.
        field: FieldIdx,
    },
    /// `MakeVariant` / `MakeRecord` operand count does not match the variant's
    /// field count.
    FieldCountMismatch {
        /// Offending body.
        body: BodyId,
        /// Offending block.
        block: BlockId,
        /// The ADT named.
        adt: AdtId,
        /// The variant index (None for products).
        variant: Option<VariantIdx>,
        /// The expected number of fields.
        expected: usize,
        /// The provided number of operands.
        found: usize,
    },
    /// A `Call` terminator carries a `FuncRef::Body(BodyId::DUMMY)`.
    CallTargetIsDummy {
        /// Offending body.
        body: BodyId,
        /// Offending block.
        block: BlockId,
    },
    /// A `Call` capability's `value_arg` does not name a capability-typed
    /// positional argument of the same call.
    CallCapValueArgMalformed {
        /// Offending body.
        body: BodyId,
        /// Offending block.
        block: BlockId,
        /// The recorded arg index.
        value_arg: u32,
    },
    /// A `Call::func = FuncRef::Body(id)` references a body outside `program.bodies`.
    CallExternBodyIdNonsense {
        /// Offending body.
        body: BodyId,
        /// Offending block.
        block: BlockId,
    },
    /// A `Spawn` terminator carries `child == BodyId::DUMMY`.
    SpawnTargetIsDummy {
        /// Offending body.
        body: BodyId,
        /// Offending block.
        block: BlockId,
    },
    /// A `Spawn::child` references a body outside `program.bodies`.
    SpawnTargetBodyIdNonsense {
        /// Offending body.
        body: BodyId,
        /// Offending block.
        block: BlockId,
    },
    /// A body has no `LocalDecl` with `source == LocalSource::ReturnSlot`.
    ReturnSlotMissing {
        /// Offending body.
        body: BodyId,
    },
    /// A body has more than one `LocalDecl` with `source == LocalSource::ReturnSlot`.
    DuplicateReturnSlot {
        /// Offending body.
        body: BodyId,
        /// Observed `ReturnSlot` count (always ≥ 2 when this variant is raised).
        count: usize,
    },
    /// A `ParamInfo` references a local whose `source != Param(i)` or whose
    /// `i` does not match its position in `params`.
    ParamLocalMismatch {
        /// Offending body.
        body: BodyId,
        /// Index into `body.params`.
        param_index: u32,
        /// The local referenced by the param.
        local: LocalId,
    },
    /// `AlignBytes::new` was given a value that is zero or not a power of two.
    AlignNotPowerOfTwo {
        /// The offending value.
        value: u32,
    },
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::BodyEntryDangling { body, entry } => write!(
                f,
                "body{} has entry block{} outside its blocks arena",
                body.as_u32(),
                entry.as_u32(),
            ),
            ValidationError::BlockSuccessorDangling {
                body,
                block,
                successor,
            } => write!(
                f,
                "body{} block{} terminator references successor block{} outside its blocks arena",
                body.as_u32(),
                block.as_u32(),
                successor.as_u32(),
            ),
            ValidationError::StatementLocalDangling {
                body,
                block,
                stmt_index,
                local,
            } => write!(
                f,
                "body{} block{} stmt[{}] references local{} outside its locals arena",
                body.as_u32(),
                block.as_u32(),
                stmt_index,
                local.as_u32(),
            ),
            ValidationError::TerminatorLocalDangling {
                body,
                block,
                local,
            } => write!(
                f,
                "body{} block{} terminator references local{} outside its locals arena",
                body.as_u32(),
                block.as_u32(),
                local.as_u32(),
            ),
            ValidationError::SwitchTagAdtKindMismatch {
                body,
                block,
                adt,
                found,
            } => write!(
                f,
                "body{} block{} SwitchTag on adt{} expected Sum but found {:?}",
                body.as_u32(),
                block.as_u32(),
                adt.as_u32(),
                found,
            ),
            ValidationError::SwitchTagVariantDangling {
                body,
                block,
                adt,
                variant,
            } => write!(
                f,
                "body{} block{} SwitchTag arm names variant{} outside adt{}",
                body.as_u32(),
                block.as_u32(),
                variant.as_u32(),
                adt.as_u32(),
            ),
            ValidationError::SwitchTagDuplicateArm {
                body,
                block,
                variant,
            } => write!(
                f,
                "body{} block{} SwitchTag has duplicate arm for variant{}",
                body.as_u32(),
                block.as_u32(),
                variant.as_u32(),
            ),
            ValidationError::MakeVariantOnProduct { body, block, adt } => write!(
                f,
                "body{} block{} MakeVariant used on Product adt{}",
                body.as_u32(),
                block.as_u32(),
                adt.as_u32(),
            ),
            ValidationError::MakeRecordOnSum { body, block, adt } => write!(
                f,
                "body{} block{} MakeRecord used on Sum adt{}",
                body.as_u32(),
                block.as_u32(),
                adt.as_u32(),
            ),
            ValidationError::FieldIndexOutOfRange {
                body,
                block,
                adt,
                variant,
                field,
            } => match variant {
                Some(v) => write!(
                    f,
                    "body{} block{} field{} out of range for adt{} variant{}",
                    body.as_u32(),
                    block.as_u32(),
                    field.as_u32(),
                    adt.as_u32(),
                    v.as_u32(),
                ),
                None => write!(
                    f,
                    "body{} block{} field{} out of range for adt{}",
                    body.as_u32(),
                    block.as_u32(),
                    field.as_u32(),
                    adt.as_u32(),
                ),
            },
            ValidationError::FieldCountMismatch {
                body,
                block,
                adt,
                variant,
                expected,
                found,
            } => match variant {
                Some(v) => write!(
                    f,
                    "body{} block{} adt{} variant{} expects {} fields but got {}",
                    body.as_u32(),
                    block.as_u32(),
                    adt.as_u32(),
                    v.as_u32(),
                    expected,
                    found,
                ),
                None => write!(
                    f,
                    "body{} block{} adt{} expects {} fields but got {}",
                    body.as_u32(),
                    block.as_u32(),
                    adt.as_u32(),
                    expected,
                    found,
                ),
            },
            ValidationError::CallTargetIsDummy { body, block } => write!(
                f,
                "body{} block{} Call targets BodyId::DUMMY (sentinel must never appear in committed MIR)",
                body.as_u32(),
                block.as_u32(),
            ),
            ValidationError::CallCapValueArgMalformed {
                body,
                block,
                value_arg,
            } => write!(
                f,
                "body{} block{} Call capability value_arg={} does not name a capability-typed positional argument",
                body.as_u32(),
                block.as_u32(),
                value_arg,
            ),
            ValidationError::CallExternBodyIdNonsense { body, block } => write!(
                f,
                "body{} block{} Call references a BodyId outside the program's bodies arena",
                body.as_u32(),
                block.as_u32(),
            ),
            ValidationError::SpawnTargetIsDummy { body, block } => write!(
                f,
                "body{} block{} Spawn targets BodyId::DUMMY (sentinel must never appear in committed MIR)",
                body.as_u32(),
                block.as_u32(),
            ),
            ValidationError::SpawnTargetBodyIdNonsense { body, block } => write!(
                f,
                "body{} block{} Spawn::child references a BodyId outside the program's bodies arena",
                body.as_u32(),
                block.as_u32(),
            ),
            ValidationError::ReturnSlotMissing { body } => write!(
                f,
                "body{} has no ReturnSlot local",
                body.as_u32(),
            ),
            ValidationError::DuplicateReturnSlot { body, count } => write!(
                f,
                "body{} has {} ReturnSlot locals (expected exactly 1)",
                body.as_u32(),
                count,
            ),
            ValidationError::ParamLocalMismatch {
                body,
                param_index,
                local,
            } => write!(
                f,
                "body{} param[{}] backs local{} whose source does not match Param({})",
                body.as_u32(),
                param_index,
                local.as_u32(),
                param_index,
            ),
            ValidationError::AlignNotPowerOfTwo { value } => write!(
                f,
                "AlignBytes value {} is zero or not a power of two",
                value,
            ),
        }
    }
}

impl std::error::Error for ValidationError {}
