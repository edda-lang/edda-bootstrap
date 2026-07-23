//! Inkwell-backed LLVM IR emitter (feature `llvm`).
//!
//! - [`Emitter`] owns an inkwell [`Context`]; [`Emitter::module`]
//!   produces an empty module tagged with the Edda triple's LLVM
//!   spelling.
//! - [`Emitter::target_machine`] builds a real inkwell
//!   [`TargetMachine`] using the [`CodeModel`] / [`RelocModel`]
//!   defaults, and [`Emitter::tagged_module`] returns a module with
//!   **both** the triple and the data-layout line set from that
//!   target machine.
//! - [`Emitter::declare_function`] projects a MIR
//!   [`edda_mir::Body`]'s signature to an LLVM function declaration.
//! - [`Emitter::lower_body`] walks the body's basic
//!   blocks, materialises an entry-block prologue (allocas + parameter
//!   stores), and lowers statements, rvalues, and terminators per
//!   the current lowering scope described in [`crate`].
//! - Object emission: [`Emitter::write_object`] writes a lowered
//!   [`Module`] to disk through a [`TargetMachine`] as a native
//!   object file ([`FileType::Object`]).
//!   [`Emitter::compile_program_to_object`] is the end-to-end
//!   convenience that lowers a [`MirProgram`] and writes the object
//!   in one call.
//!
//! # Module layout
//!
//! This module keeps the public [`Emitter`] API and the inkwell-binding
//! glue, split across focused submodules:
//!
//! - [`target_machine`] — IR-module setup and `TargetMachine` config
//!   ([`Emitter::module`], [`Emitter::target_machine`],
//!   [`Emitter::tagged_module`]).
//! - [`lower_api`] — the MIR-walker delegation wrappers
//!   ([`Emitter::declare_function`], [`Emitter::lower_body`],
//!   [`Emitter::lower_program`]).
//! - [`object`] — native object-file emission
//!   ([`Emitter::write_object`], [`Emitter::compile_program_to_object`]).
//!
//! The MIR walker proper lives under [`crate::lower`], split into one
//! concept module per axis (`fn_sig`, `local`, `operand`, `place`,
//! `rvalue`, `statement`, `terminator`, `body`).
//! [`Emitter::declare_function`] and [`Emitter::lower_body`] are thin
//! wrappers that delegate into that tree.
//!
//! # Feature gating
//!
//! Compiled only when `--features llvm` is enabled. Without the feature,
//! [`crate::emit`] is absent and the rest of `edda-compile` compiles
//! cleanly without an LLVM toolchain on PATH.
//!
//! # Lifetime model
//!
//! [`Emitter`] owns an inkwell [`Context`]. Every [`Module`] returned
//! borrows that context, so the borrow checker enforces "modules
//! outlive their context". Callers that want multiple modules in one
//! batch hold a single [`Emitter`] for the batch's lifetime.
//!
//! # Initialisation
//!
//! LLVM target backends must be initialised before
//! `Target::from_triple` will find them. [`Emitter::tagged_module`] and
//! [`Emitter::target_machine`] call the per-arch
//! `Target::initialize_*` family on every invocation; LLVM's internal
//! init functions are idempotent so the repeated calls cost only a
//! couple of branch checks once the first call has registered the
//! arch.

mod lower_api;
mod object;
mod target_machine;

use inkwell::context::Context;

/// Owner of the inkwell [`Context`].
///
/// Every [`Module`] produced by [`Emitter::module`] /
/// [`Emitter::tagged_module`] borrows from this owner.
pub struct Emitter {
    context: Context,
}

impl Default for Emitter {
    fn default() -> Self {
        Emitter::new()
    }
}

impl Emitter {
    /// Construct a fresh emitter with its own [`Context`].
    pub fn new() -> Self {
        Emitter {
            context: Context::create(),
        }
    }

    /// Borrow the underlying [`Context`]. Used by crate-internal
    /// code (the `lower/` walker) that constructs inkwell types
    /// directly and needs the same context the module lives in.
    pub(crate) fn context(&self) -> &Context {
        &self.context
    }
}

#[cfg(test)]
mod tests;
