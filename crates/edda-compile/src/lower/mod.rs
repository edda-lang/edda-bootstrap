//! MIR -> LLVM IR walker, split into one concept module per axis.
//!
//! Each submodule owns a single MIR construct's lowering. The pure
//! tables at the crate root ([`crate::mir_prim`], [`crate::ops`],
//! [`crate::abi_attr`], [`crate::calling_conv`]) provide the per-
//! (prim, op) facts; this `lower/` tree wires them into an inkwell-
//! backed walker that produces an [`inkwell::module::Module`] from an
//! [`edda_mir::MirProgram`].
//!
//! # Module layout
//!
//! - [`fn_sig`] — [`edda_mir::Body`] signature -> inkwell `FunctionValue`
//!   declaration (no body yet).
//! - [`local`] — entry-block prologue: `alloca` every primitive-typed
//!   local, `store` each incoming inkwell argument into its parameter
//!   slot.
//! - [`operand`] — [`edda_mir::Operand`] -> inkwell `BasicValueEnum`;
//!   includes [`operand::lower_const`] for the const-table path.
//! - [`place`] — [`edda_mir::Place`] -> alloca pointer (read or write
//!   variant). Admitted projections are documented in [`place`]'s own
//!   module doc.
//! - [`rvalue`] — [`edda_mir::Rvalue`] -> inkwell value. Hosts the
//!   primitive [`BinOp`](edda_mir::BinOp) and [`UnOp`](edda_mir::UnOp)
//!   dispatch tables.
//! - [`statement`] — [`edda_mir::Statement`] -> instruction(s).
//! - [`terminator`] — [`edda_mir::Terminator`] -> branch / return /
//!   unreachable.
//! - [`body`] — per-body orchestration: pre-creates `BasicBlock`s,
//!   runs the entry-block prologue, walks every block.
//! - [`program`] — program-level orchestration: walks every
//!   [`edda_mir::Body`] in a [`edda_mir::MirProgram`] into a single
//!   inkwell `Module`.
//! - [`ty`] — shared MIR-type -> inkwell-type adapters consumed by
//!   every walker submodule. Lives here, in its own module, because
//!   no single per-construct submodule owns it.
//!
//! `emit.rs` keeps the public [`crate::Emitter`] API; this tree is
//! crate-internal implementation.

pub(crate) mod body;
pub(crate) mod fn_sig;
pub(crate) mod local;
pub(crate) mod operand;
pub(crate) mod place;
pub(crate) mod program;
pub(crate) mod rvalue;
pub(crate) mod statement;
pub(crate) mod terminator;
pub(crate) mod ty;

#[cfg(test)]
pub(crate) mod test_fixtures;

use edda_intern::Interner;
use edda_mir::{Body, MirProgram};
use edda_target::{Arch, Os};
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::types::BasicType;
use inkwell::values::PointerValue;

/// Shared context bundle threaded through every walker in this tree.
///
/// Before this struct, each walker function carried 7-9 positional
/// parameters that were identical at every layer (context, builder,
/// body, program, locals, arch, body_name). Bundling them collapses
/// signatures to two or three parameters and means that adding a new
/// field (e.g. an upcoming string pool) is a one-line edit instead
/// of a sweep across the walker tree.
///
/// The struct is `Copy`-free on purpose: callers pass `&LowerCtx` to
/// each callee so the bundle's borrows are checked once at the body
/// boundary rather than re-validated for every walker step.
pub(crate) struct LowerCtx<'ctx, 'a> {
    /// Inkwell context owning every IR-level type and value this
    /// walker produces.
    pub context: &'ctx Context,
    /// Inkwell IR builder. Its positioned block is set by the caller
    /// in `body::lower_body`.
    pub builder: &'a Builder<'ctx>,
    /// Inkwell module the walker emits into. Used by the Call walker
    /// to resolve callee `FunctionValue`s by name and to declare extern
    /// callees on first use.
    pub module: &'a Module<'ctx>,
    /// MIR body whose blocks the walker is lowering.
    pub body: &'a Body,
    /// The enclosing MIR program — used to resolve constants by
    /// `ConstId` and ADT shapes by `AdtDefId`.
    pub program: &'a MirProgram,
    /// Workspace interner — used by the Call walker to resolve callee
    /// names from `Symbol` to `&str`.
    pub interner: &'a Interner,
    /// Per-local alloca pointers indexed by `LocalId::index()`.
    /// `None` marks a non-primitive local that the walker
    /// rejects on read.
    pub locals: &'a [Option<PointerValue<'ctx>>],
    /// Active target arch. Drives pointer-width-sensitive type
    /// lowering for `Usize`/`Isize`.
    pub arch: Arch,
    /// Active target OS. Drives platform-specific ABI choices —
    /// notably the win64-vs-sysv split for how aggregates > 8 bytes
    /// cross the extern boundary.
    pub os: Os,
    /// Interner-resolved name of the enclosing body. Used in error
    /// messages and (eventually) for symbol-mangling.
    pub body_name: &'a str,
    /// Per-body collision-free LLVM symbol names indexed by
    /// `BodyId::index()`. Computed by `lower_program` before any
    /// `add_function` runs; the call walker uses this slot rather
    /// than the source `body.name` so the lookup matches the symbol
    /// the declare pass actually registered.
    pub symbol_names: &'a [String],
}

impl<'ctx> LowerCtx<'ctx, '_> {
    /// Build a fixed-size temporary `alloca` at the top of the function's
    /// entry block, then restore the builder to its prior position.
    ///
    /// LLVM only treats an `alloca` as a *static* frame slot — allocated
    /// once in the prologue and reclaimed at return — when it lives in the
    /// entry block. An `alloca` emitted in any later block (e.g. inside a
    /// loop body) is a *dynamic* alloca: on win64 it lowers to a
    /// `__chkstk` + `sub rsp, rax` sequence whose space is **not** freed
    /// until the function returns. Emitting one per loop iteration leaks
    /// the thread stack without bound, overflowing on deep solver loops.
    /// Temporaries (byval-arg / sret /
    /// aggregate-construction scratch) outlive only the statement that
    /// produces them, so a single reused entry-block slot per temp is
    /// correct as well as cheaper.
    pub(crate) fn build_entry_alloca<T: BasicType<'ctx>>(
        &self,
        ty: T,
        name: &str,
    ) -> PointerValue<'ctx> {
        let restore = self.builder.get_insert_block();
        let function = restore
            .and_then(|bb| bb.get_parent())
            .expect("insert block has a parent function");
        let entry = function
            .get_first_basic_block()
            .expect("function has an entry block");
        match entry.get_first_instruction() {
            Some(first) => self.builder.position_before(&first),
            None => self.builder.position_at_end(entry),
        }
        let ptr = self
            .builder
            .build_alloca(ty, name)
            .expect("entry-block alloca must succeed");
        if let Some(bb) = restore {
            self.builder.position_at_end(bb);
        }
        ptr
    }
}
