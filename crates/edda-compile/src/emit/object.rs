//! Native object-file emission for the [`Emitter`].
//!
//! Writes a lowered [`Module`] to disk as a native object file
//! ([`Emitter::write_object`]) and the end-to-end driver that lowers a
//! [`MirProgram`] and emits its object in one call
//! ([`Emitter::compile_program_to_object`]).

use std::path::Path;

use inkwell::module::Module;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{FileType, TargetMachine};

use edda_intern::Interner;
use edda_mir::MirProgram;
use edda_target::TargetCfg;

use super::Emitter;
use crate::error::CompileError;
use crate::lower::program::lower_program;

impl Emitter {
    /// Write a lowered [`Module`] to disk as a native object file.
    ///
    /// Thin wrapper over inkwell's
    /// [`TargetMachine::write_to_file`] with
    /// [`FileType::Object`]. The caller owns the [`TargetMachine`]'s
    /// construction (typically through [`Emitter::target_machine`])
    /// and the destination path; this method does not create parent
    /// directories — callers stage through `edda-cache`'s atomic
    /// stage-rename when they need durability.
    ///
    /// Returns [`CompileError::ObjectEmit`] when LLVM rejects the
    /// write (unwritable destination, unsupported file type for the
    /// target, internal codegen error).
    pub(crate) fn write_object<'ctx>(
        &'ctx self,
        target_machine: &TargetMachine,
        module: &Module<'ctx>,
        path: &Path,
    ) -> Result<(), CompileError> {
        target_machine
            .write_to_file(module, FileType::Object, path)
            .map_err(|e| CompileError::ObjectEmit {
                path: path.to_path_buf(),
                reason: e.to_string(),
            })
    }

    /// Lower a [`MirProgram`] and write the resulting native object
    /// file to `path` in one call.
    ///
    /// End-to-end convenience over [`Emitter::lower_program`] +
    /// [`Emitter::write_object`]. Constructs the inkwell
    /// [`TargetMachine`] once, tags a fresh module with the triple
    /// and data-layout line from that machine, runs the
    /// MIR → LLVM IR walker, then asks the same machine to emit the
    /// object file.
    ///
    /// Errors short-circuit at the first failing stage: target
    /// initialisation as [`CompileError::LlvmInit`], unsupported MIR
    /// shape as [`CompileError::UnsupportedMirShape`], write failure
    /// as [`CompileError::ObjectEmit`].
    ///
    /// When `freestanding` is set, the emitted object carries the OS
    /// entry stub (`_edda_start`) so the binary can link with no C
    /// runtime and no Rust `edda-rt`.
    pub fn compile_program_to_object<'ctx>(
        &'ctx self,
        name: &str,
        target: &TargetCfg,
        program: &MirProgram,
        interner: &Interner,
        path: &Path,
        freestanding: bool,
    ) -> Result<(), CompileError> {
        let target_machine = self.target_machine(target)?;
        let module = self.module(name, target);
        let data_layout = target_machine.get_target_data().get_data_layout();
        module.set_data_layout(&data_layout);
        lower_program(
            self.context(),
            &module,
            program,
            interner,
            target.triple().arch(),
            name,
            freestanding,
        )?;
        if let Some(pipeline) = opt_pass_pipeline() {
            module
                .run_passes(&pipeline, &target_machine, PassBuilderOptions::create())
                .map_err(|e| CompileError::ObjectEmit {
                    path: path.to_path_buf(),
                    reason: format!("LLVM pass pipeline `{pipeline}`: {e}"),
                })?;
        }
        if std::env::var("EDDA_DUMP_IR").is_ok() {
            eprintln!("{}", module.print_to_string().to_string());
        }
        self.write_object(&target_machine, &module, path)
    }
}

//   emit path stays byte-identical to the original behaviour (lowered IR goes
//   straight to instruction selection with no mid-level optimisation)
//   any other non-empty value is passed to `LLVMRunPasses` verbatim as a
//   textual pipeline (e.g. `function(mem2reg,instcombine)`), so targeted
//   pipelines are expressible without a rebuild
/// Resolve the `EDDA_LLVM_OPT` env knob to an LLVM pass-pipeline string.
///
/// The generated IR is alloca-heavy by construction (every MIR local is
/// stack-materialised), so without a mid-level pipeline the emitted
/// machine code keeps all locals in memory. Self-hosted binaries built
/// with `EDDA_LLVM_OPT=2` run the standard `default<O2>` module pipeline
/// (mem2reg, SROA, inlining, GVN, ...) before instruction selection
/// (the T1 throughput gap).
fn opt_pass_pipeline() -> Option<String> {
    let value = std::env::var("EDDA_LLVM_OPT").ok()?;
    match value.as_str() {
        "" | "0" => None,
        "1" | "2" | "3" => Some(format!("default<O{value}>")),
        _ => Some(value),
    }
}
