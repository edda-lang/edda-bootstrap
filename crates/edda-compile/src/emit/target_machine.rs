//! IR-module setup and `TargetMachine` configuration for the [`Emitter`].
//!
//! Holds the inkwell-binding glue that turns an Edda [`TargetCfg`] into
//! a tagged [`Module`] and a configured [`TargetMachine`]: triple
//! tagging ([`Emitter::module`]), target-machine construction with
//! platform defaults ([`Emitter::target_machine`]), and the
//! fully-tagged module suitable for IR emission ([`Emitter::tagged_module`]).

use inkwell::OptimizationLevel;
use inkwell::module::Module;
use inkwell::targets::{Target, TargetMachine, TargetTriple as LlvmTargetTriple};

use edda_target::TargetCfg;

use super::Emitter;
use crate::error::CompileError;
use crate::target_defaults;
use crate::target_info::{initialize_for_arch, llvm_triple};

impl Emitter {
    /// Construct an empty module tagged with the Edda triple only.
    ///
    /// Use this when the data-layout line is not required (e.g., tests
    /// that only inspect the triple header). For a fully-tagged module
    /// suitable for IR emission, call [`Emitter::tagged_module`].
    pub(crate) fn module<'ctx>(&'ctx self, name: &str, target: &TargetCfg) -> Module<'ctx> {
        let module = self.context().create_module(name);
        let triple_str = llvm_triple(target.triple());
        let llvm_triple = LlvmTargetTriple::create(&triple_str);
        module.set_triple(&llvm_triple);
        module
    }

    /// Build the inkwell [`TargetMachine`] for `target` using the
    /// platform defaults.
    ///
    /// Calls the per-arch `Target::initialize_*` (idempotent), looks up
    /// the LLVM [`Target`] by triple, and constructs the
    /// [`TargetMachine`] with:
    ///
    /// - CPU: `"generic"`
    /// - features: `""`
    /// - optimisation level: [`OptimizationLevel::Default`]
    /// - relocation model: [`target_defaults::reloc_model`]
    /// - code model: [`target_defaults::code_model`]
    ///
    /// Returns [`CompileError::LlvmInit`] if any stage fails.
    pub(crate) fn target_machine(&self, target: &TargetCfg) -> Result<TargetMachine, CompileError> {
        initialize_for_arch(target.triple().arch());

        let triple_str = llvm_triple(target.triple());
        let llvm_triple = LlvmTargetTriple::create(&triple_str);

        let llvm_target = Target::from_triple(&llvm_triple).map_err(|e| CompileError::LlvmInit {
            triple: *target.triple(),
            stage: "from_triple",
            reason: e.to_string(),
        })?;

        let reloc = target_defaults::reloc_model(target.triple()).to_inkwell();
        let code = target_defaults::code_model(target.triple()).to_inkwell();

        llvm_target
            .create_target_machine(
                &llvm_triple,
                "generic",
                "",
                OptimizationLevel::Default,
                reloc,
                code,
            )
            .ok_or_else(|| CompileError::LlvmInit {
                triple: *target.triple(),
                stage: "create_target_machine",
                reason: "LLVM declined to create a target machine for the requested target"
                    .to_string(),
            })
    }

    /// Build a module fully tagged for IR emission: triple set from
    /// [`llvm_triple`], data layout set from a freshly-built
    /// [`TargetMachine`].
    ///
    /// Each invocation constructs a new [`TargetMachine`]; callers that
    /// build many modules for the same target can call
    /// [`Emitter::target_machine`] once and copy the data layout
    /// themselves via inkwell's [`TargetMachine::get_target_data`].
    #[allow(dead_code)] // retained for in-crate test scaffolding; production path uses module + lower_program directly
    pub(crate) fn tagged_module<'ctx>(
        &'ctx self,
        name: &str,
        target: &TargetCfg,
    ) -> Result<Module<'ctx>, CompileError> {
        let module = self.module(name, target);
        let target_machine = self.target_machine(target)?;
        let data = target_machine.get_target_data();
        let data_layout = data.get_data_layout();
        module.set_data_layout(&data_layout);
        Ok(module)
    }
}
