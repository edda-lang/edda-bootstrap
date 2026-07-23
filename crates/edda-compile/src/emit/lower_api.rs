//! MIR-walker delegation wrappers on the [`Emitter`].
//!
//! Thin [`Emitter`] methods that delegate into the [`crate::lower`]
//! tree: per-signature declaration ([`Emitter::declare_function`]),
//! per-body lowering ([`Emitter::lower_body`]), and the whole-program
//! driver ([`Emitter::lower_program`]). The walker logic lives under
//! [`crate::lower`]; these wrappers thread the owned [`Context`] and
//! resolve body names.

use inkwell::module::Module;
use inkwell::values::FunctionValue;

use edda_intern::Interner;
use edda_mir::{Body, MirProgram};
use edda_target::{Arch, TargetCfg};

use super::Emitter;
use crate::error::CompileError;
use crate::lower::body::lower_body;
use crate::lower::fn_sig::lower_fn_sig;
use crate::lower::program::lower_program;

impl Emitter {
    /// Emit an LLVM function declaration for `body`'s signature into `module`.
    ///
    /// Thin wrapper over [`crate::lower::fn_sig::lower_fn_sig`]. See that
    /// function for the full contract.
    #[allow(dead_code)] // retained for in-crate test scaffolding
    pub(crate) fn declare_function<'ctx>(
        &'ctx self,
        module: &Module<'ctx>,
        body: &Body,
        program: &MirProgram,
        interner: &Interner,
        arch: Arch,
    ) -> Result<FunctionValue<'ctx>, CompileError> {
        let symbol = interner.resolve(body.name).to_string();
        lower_fn_sig(self.context(), module, body, program, interner, arch, &symbol)
    }

    /// Walk a MIR [`Body`] and emit its basic blocks into `function`.
    ///
    /// Thin wrapper over [`crate::lower::body::lower_body`]. See that
    /// function for the full contract.
    #[allow(dead_code)] // retained for in-crate test scaffolding
    pub(crate) fn lower_body<'ctx>(
        &'ctx self,
        module: &Module<'ctx>,
        function: FunctionValue<'ctx>,
        body: &Body,
        program: &MirProgram,
        interner: &Interner,
        arch: Arch,
    ) -> Result<(), CompileError> {
        // Single-body callers (test scaffolding) supply an empty
        // symbol-names slice; `resolve_callee` falls back to the
        // source-level name via `interner.resolve(body.name)`. The
        // production path goes through `lower_program`, which
        // populates the slice with collision-free symbols.
        lower_body(
            self.context(),
            module,
            function,
            body,
            program,
            interner,
            arch,
            &[],
        )
    }

    /// Lower an entire [`MirProgram`] into a fresh inkwell [`Module`].
    ///
    /// Convenience over the per-body [`Emitter::declare_function`] +
    /// [`Emitter::lower_body`] pair: builds a fully-tagged module, then
    /// walks every body through [`crate::lower::program::lower_program`].
    /// See that function for the full contract;
    /// product and sum ADTs, slices, and capability threading are all
    /// admitted.
    #[allow(dead_code)] // retained for in-crate test scaffolding; production uses crate::lower::program::lower_program directly
    pub(crate) fn lower_program<'ctx>(
        &'ctx self,
        name: &str,
        target: &TargetCfg,
        program: &MirProgram,
        interner: &Interner,
    ) -> Result<Module<'ctx>, CompileError> {
        let module = self.tagged_module(name, target)?;
        lower_program(
            self.context(),
            &module,
            program,
            interner,
            target.triple().arch(),
            name,
            false,
        )?;
        Ok(module)
    }
}
