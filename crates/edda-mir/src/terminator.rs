//! Terminators: the single control-flow instruction at the end of every basic
//! block.

use edda_intern::Symbol;
use edda_span::Span;

use crate::ids::{AdtId, BlockId, BodyId, EffectId, LocalId, VariantIdx};
use crate::operand::Operand;
use crate::place::Place;
use crate::ty::{FnSig, ParamMode};

/// A terminator: source span plus variant.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Terminator {
    /// Source span of the originating control-flow construct.
    pub span: Span,
    /// Variant and payload.
    pub kind: TerminatorKind,
}

/// Every terminator form.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum TerminatorKind {
    /// `return operand` — leave the body with a value.
    Return(Operand),
    /// Unconditional branch to a successor block.
    Goto(BlockId),
    /// Two-way branch on a boolean operand.
    SwitchBool {
        /// Condition operand.
        cond: Operand,
        /// Target when `cond` is true.
        true_bb: BlockId,
        /// Target when `cond` is false.
        false_bb: BlockId,
    },
    /// N-way branch on a sum-type's discriminant tag.
    SwitchTag {
        /// Sum-typed subject.
        subject: Operand,
        /// The ADT (must be `AdtKind::Sum`).
        adt: AdtId,
        /// `(variant, target)` pairs in match order.
        arms: Vec<(VariantIdx, BlockId)>,
        /// Fallback branch for unmatched discriminants.
        otherwise: BlockId,
    },
    /// Call a function: thread capabilities, write the return value, branch
    /// on success / error.
    Call {
        /// Callee (an interned body or an extern symbol).
        func: FuncRef,
        /// Arguments in declaration order, paired with their call-site mode.
        args: Vec<CallArg>,
        /// Capabilities threaded into the call, one per callee capability
        /// slot in callee-row order.
        capabilities: Vec<ThreadedCapability>,
        /// Destination for the return value.
        destination: Place,
        /// Successor on normal return.
        target: BlockId,
        /// Successor on `?`-propagated error; `None` when the call does not
        /// participate in `?` propagation.
        on_error: Option<BlockId>,
    },
    /// Raise an error of `err_adt` carrying `value` to the nearest `?` site.
    Raise {
        /// Error ADT.
        err_adt: AdtId,
        /// Error payload operand.
        value: Operand,
    },
    /// Abort with a panic message.
    Panic {
        /// Panic message operand (string-typed).
        msg: Operand,
    },
    /// Statically-unreachable control flow.
    Unreachable,
    /// `<group>.spawn(take a = ..., ...) { body }` — lift `child` onto a new
    /// OS thread via `__edda_task_spawn`, threading `args` (the explicit
    /// `take` arguments plus any implicitly read-captured outer bindings, in
    /// `child.params` order) and the enclosing `scope(exec)`'s task group.
    Spawn {
        /// The lifted spawn body.
        child: BodyId,
        /// Arguments passed to `child`, in its declared parameter order.
        args: Vec<Operand>,
        /// The enclosing `scope(exec)`'s task-group handle.
        group_local: LocalId,
        /// Destination for the spawned task's handle.
        dest: LocalId,
        /// Successor block.
        target: BlockId,
    },
    /// `<task>.await` — block until `task` completes and write its result
    /// into `dest`.
    Await {
        /// The task handle operand.
        task: Operand,
        /// Destination for the task's result.
        dest: LocalId,
        /// Successor block.
        target: BlockId,
    },
}

/// What a [`TerminatorKind::Call`] is calling.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum FuncRef {
    /// Call a body defined inside this program.
    Body(BodyId),
    /// Call an extern symbol with the given signature.
    Extern {
        /// Symbol name as it appears in the linked binary.
        name: Symbol,
        /// Signature of the extern.
        sig: Box<FnSig>,
    },
    /// Indirect call through a fn-pointer operand. The operand's
    /// [`crate::ty::MirType`] is `MirTypeKind::FnPtr(sig)`; we duplicate
    /// `sig` here so the call lowering / pretty-printer / validator do
    /// not need to chase the operand's `Place` back to its declared
    /// type. The `sig`'s `params`/`ret`/`capabilities`/`may_raise`/
    /// `may_panic` must agree with the operand's type sig.
    Indirect {
        /// Callee operand — evaluates to a fn-pointer value.
        callee: Operand,
        /// Signature of the callee (copied from the operand's MirType).
        sig: Box<FnSig>,
    },
}

/// One capability threaded into a [`TerminatorKind::Call`].
///
/// `id` is the caller-side capability slot the callee's row entry was
/// accounted against. `value_arg` is the index into the call's `args`
/// whose operand carries the capability *value* the callee receives —
/// the positional dataflow. The two can disagree when the caller holds
/// two values of one capability type (its own parameter plus
/// `alloc.fork(allocator)`'s result): accounting resolves to the row
/// slot while the value is the forked handle. Codegen must materialise
/// the argument from `value_arg` when present, else `alloc.fork` is a
/// silent no-op and `alloc.close(fork)` destroys the parent heap.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct ThreadedCapability {
    /// Caller-side capability slot id (effect accounting + fallback
    /// value source).
    pub id: EffectId,
    /// Index into the call's `args` whose operand carries the
    /// capability value; `None` for synthesised sites with no
    /// positional capability operand.
    pub value_arg: Option<u32>,
}

impl ThreadedCapability {
    /// Accounting-only threading: the value loads from `id`'s slot.
    pub fn slot(id: EffectId) -> Self {
        Self {
            id,
            value_arg: None,
        }
    }

    /// Positional threading: the value is `args[arg_index]`'s operand.
    pub fn positional(id: EffectId, arg_index: u32) -> Self {
        Self {
            id,
            value_arg: Some(arg_index),
        }
    }
}

/// One argument to a [`TerminatorKind::Call`].
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct CallArg {
    /// Call-site mode.
    pub mode: CallMode,
    /// Argument operand.
    pub operand: Operand,
}

/// Call-site argument mode — the projection of [`ParamMode`] as seen at the
/// call.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum CallMode {
    /// Read-by-reference (corresponds to `ParamMode::Let`).
    Read,
    /// Mutable by-reference (corresponds to `ParamMode::Mutable`).
    Mutable,
    /// By-value move (corresponds to `ParamMode::Take`).
    Take,
    /// Out-parameter write (corresponds to `ParamMode::Init`).
    Init,
}

impl CallMode {
    /// Map a [`ParamMode`] to its call-site projection.
    pub fn from_param_mode(mode: ParamMode) -> CallMode {
        match mode {
            ParamMode::Let => CallMode::Read,
            ParamMode::Mutable => CallMode::Mutable,
            ParamMode::Take => CallMode::Take,
            ParamMode::Init => CallMode::Init,
        }
    }

    /// Lowercase keyword spelling for the pretty-printer.
    pub fn as_str(self) -> &'static str {
        match self {
            CallMode::Read => "read",
            CallMode::Mutable => "mutable",
            CallMode::Take => "take",
            CallMode::Init => "init",
        }
    }
}
