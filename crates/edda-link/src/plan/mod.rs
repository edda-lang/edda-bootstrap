//! Tool-invocation plan and per-tool argv construction.
//!
//! [`LinkPlan`] captures the inputs the driver passes through to
//! `edda-link`: object files, output path, target config, library
//! dependencies, and an escape-hatch `extra_args` tail. `argv()` lowers
//! the plan to the tool-specific command line — linker dialect per
//! `build-system.md` §5b + `backend-choice.md` §6.7 for
//! executables / dynamic libraries, archiver dialect for static
//! libraries.

mod argv;

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use edda_target::TargetCfg;

use crate::archiver::Archiver;
use crate::error::LinkError;
use crate::linker::Linker;
use crate::output::{LibKind, LibSpec, OutputKind};
use crate::tool::Tool;

use self::argv::{
    argv_elf, argv_ld64_lld, argv_llvm_ar, argv_llvm_lib, argv_lld_link, argv_wasm_ld,
};

/// Inputs to one tool invocation.
///
/// The driver constructs a `LinkPlan` per output artifact and calls
/// [`Self::argv`] to materialise the command line, then
/// [`crate::run`] to spawn the tool.
///
/// All fields borrow — the plan does not allocate. Empty slices are
/// allowed for `libs`, `lib_search_paths`, and `extra_args`; an empty
/// `inputs` slice is rejected at [`Self::argv`].
#[derive(Clone, Copy, Debug)]
pub struct LinkPlan<'a> {
    /// Target the build is producing for.
    pub target: &'a TargetCfg,
    /// Object files to link or archive. Must be non-empty.
    pub inputs: &'a [PathBuf],
    /// Output path for the final artifact.
    pub output: &'a Path,
    /// Kind of artifact to emit.
    pub kind: OutputKind,
    /// Library dependencies to record. Ignored by archiver dialects.
    pub libs: &'a [LibSpec],
    /// Library search paths (`-L<dir>` / `/LIBPATH:<dir>`). Ignored by
    /// archiver dialects.
    pub lib_search_paths: &'a [PathBuf],
    /// Escape-hatch arguments appended verbatim. Reserved for tests
    /// and for driver-side flags the tool dialect doesn't yet model.
    pub extra_args: &'a [OsString],
}

impl LinkPlan<'_> {
    /// Select the tool for this plan: a linker for `Executable` and
    /// `DynamicLibrary`, an archiver for `StaticLibrary`.
    pub fn tool(&self) -> Result<Tool, LinkError> {
        let triple = self.target.triple();
        match self.kind {
            OutputKind::Executable | OutputKind::DynamicLibrary => {
                Linker::for_target(triple).map(Tool::Link)
            }
            OutputKind::StaticLibrary => Archiver::for_target(triple).map(Tool::Archive),
        }
    }

    /// Materialise the tool command line as a vector of OS-safe args.
    ///
    /// The vector is the tool's argv *without* argv\[0\] — callers
    /// pass it to [`std::process::Command::args`] after spawning the
    /// tool resolved by [`Tool::discover`].
    pub fn argv(&self) -> Result<Vec<OsString>, LinkError> {
        if self.inputs.is_empty() {
            return Err(LinkError::NoInputs);
        }
        let tool = self.tool()?;
        if let Tool::Link(linker) = tool {
            reject_framework_on_non_macos(linker, self.libs)?;
        }
        match tool {
            Tool::Link(Linker::Mold | Linker::LdLld) => Ok(argv_elf(self)),
            Tool::Link(Linker::Ld64Lld) => Ok(argv_ld64_lld(self)),
            Tool::Link(Linker::LldLink) => Ok(argv_lld_link(self)),
            Tool::Link(Linker::WasmLd) => Ok(argv_wasm_ld(self)),
            Tool::Archive(Archiver::LlvmAr) => Ok(argv_llvm_ar(self)),
            Tool::Archive(Archiver::LlvmLib) => Ok(argv_llvm_lib(self)),
        }
    }
}

/// `-framework` is macOS-only. Reject framework linkage on any other
/// linker before dialect dispatch — keeps the per-linker helpers
/// small. Archiver dispatch never sees `LibSpec` entries.
fn reject_framework_on_non_macos(linker: Linker, libs: &[LibSpec]) -> Result<(), LinkError> {
    if matches!(linker, Linker::Ld64Lld) {
        return Ok(());
    }
    if libs.iter().any(|l| l.kind == LibKind::Framework) {
        return Err(LinkError::UnsupportedKindForTool {
            tool: Tool::Link(linker),
            kind: OutputKind::DynamicLibrary,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::argv::WIN64_STACK_RESERVE;
    use edda_target::{AbiVariant, Arch, Os, TargetTriple};

    fn cfg(arch: Arch, os: Os, abi: AbiVariant) -> TargetCfg {
        TargetCfg::new(TargetTriple::new(arch, os, abi))
    }

    fn paths(items: &[&str]) -> Vec<PathBuf> {
        items.iter().map(PathBuf::from).collect()
    }

    fn contains(argv: &[OsString], needle: &str) -> bool {
        argv.iter().any(|s| s == needle)
    }

    #[test]
    fn empty_inputs_rejected() {
        let target = cfg(Arch::X86_64, Os::Linux, AbiVariant::Gnu);
        let output = PathBuf::from("out");
        let plan = LinkPlan {
            target: &target,
            inputs: &[],
            output: &output,
            kind: OutputKind::Executable,
            libs: &[],
            lib_search_paths: &[],
            extra_args: &[],
        };
        let err = plan.argv().unwrap_err();
        assert!(matches!(err, LinkError::NoInputs));
    }

    #[test]
    fn elf_executable_emits_gc_sections() {
        let target = cfg(Arch::X86_64, Os::Linux, AbiVariant::Gnu);
        let inputs = paths(&["a.o", "b.o"]);
        let output = PathBuf::from("out");
        let plan = LinkPlan {
            target: &target,
            inputs: &inputs,
            output: &output,
            kind: OutputKind::Executable,
            libs: &[],
            lib_search_paths: &[],
            extra_args: &[],
        };
        let argv = plan.argv().unwrap();
        assert!(contains(&argv, "-o"));
        assert!(contains(&argv, "out"));
        assert!(contains(&argv, "--gc-sections"));
        assert!(contains(&argv, "a.o"));
        assert!(contains(&argv, "b.o"));
    }

    #[test]
    fn elf_dynamic_library_adds_shared() {
        let target = cfg(Arch::X86_64, Os::Linux, AbiVariant::Gnu);
        let inputs = paths(&["a.o"]);
        let output = PathBuf::from("libout.so");
        let plan = LinkPlan {
            target: &target,
            inputs: &inputs,
            output: &output,
            kind: OutputKind::DynamicLibrary,
            libs: &[],
            lib_search_paths: &[],
            extra_args: &[],
        };
        let argv = plan.argv().unwrap();
        assert!(contains(&argv, "-shared"));
    }

    #[test]
    fn elf_lib_dialect_picks_static_and_dynamic_forms() {
        let target = cfg(Arch::X86_64, Os::Linux, AbiVariant::Gnu);
        let inputs = paths(&["a.o"]);
        let output = PathBuf::from("out");
        let libs = vec![
            LibSpec::new("z", LibKind::Dynamic),
            LibSpec::new("static_blob", LibKind::Static),
        ];
        let plan = LinkPlan {
            target: &target,
            inputs: &inputs,
            output: &output,
            kind: OutputKind::Executable,
            libs: &libs,
            lib_search_paths: &[],
            extra_args: &[],
        };
        let argv = plan.argv().unwrap();
        assert!(contains(&argv, "-lz"));
        assert!(contains(&argv, "-l:libstatic_blob.a"));
    }

    #[test]
    fn framework_lib_rejected_on_linux() {
        let target = cfg(Arch::X86_64, Os::Linux, AbiVariant::Gnu);
        let inputs = paths(&["a.o"]);
        let output = PathBuf::from("out");
        let libs = vec![LibSpec::new("Foundation", LibKind::Framework)];
        let plan = LinkPlan {
            target: &target,
            inputs: &inputs,
            output: &output,
            kind: OutputKind::Executable,
            libs: &libs,
            lib_search_paths: &[],
            extra_args: &[],
        };
        let err = plan.argv().unwrap_err();
        assert!(matches!(err, LinkError::UnsupportedKindForTool { .. }));
    }

    #[test]
    fn ld64_lld_argv_uses_framework_pair() {
        let target = cfg(Arch::Aarch64, Os::Macos, AbiVariant::Darwin);
        let inputs = paths(&["a.o"]);
        let output = PathBuf::from("out");
        let libs = vec![LibSpec::new("Foundation", LibKind::Framework)];
        let plan = LinkPlan {
            target: &target,
            inputs: &inputs,
            output: &output,
            kind: OutputKind::Executable,
            libs: &libs,
            lib_search_paths: &[],
            extra_args: &[],
        };
        let argv = plan.argv().unwrap();
        let idx = argv.iter().position(|a| a == "-framework").unwrap();
        assert_eq!(argv[idx + 1], "Foundation");
        assert!(contains(&argv, "-dead_strip"));
    }

    #[test]
    fn lld_link_argv_uses_slash_out_and_opt_ref() {
        let target = cfg(Arch::X86_64, Os::Windows, AbiVariant::Msvc);
        let inputs = paths(&["a.obj"]);
        let output = PathBuf::from("out.exe");
        let plan = LinkPlan {
            target: &target,
            inputs: &inputs,
            output: &output,
            kind: OutputKind::Executable,
            libs: &[LibSpec::new("kernel32", LibKind::Dynamic)],
            lib_search_paths: &[PathBuf::from("C:/libs")],
            extra_args: &[],
        };
        let argv = plan.argv().unwrap();
        assert!(contains(&argv, "/OUT:out.exe"));
        assert!(contains(&argv, "/OPT:REF"));
        assert!(contains(&argv, "/OPT:ICF"));
        // Executables carry a large win64 stack reserve.
        assert!(contains(&argv, &format!("/STACK:{WIN64_STACK_RESERVE}")));
        assert!(contains(&argv, "/LIBPATH:C:/libs"));
        assert!(contains(&argv, "kernel32.lib"));
    }

    #[test]
    fn wasm_ld_argv_has_gc_sections() {
        let target = cfg(Arch::Wasm32, Os::BareMetal, AbiVariant::None);
        let inputs = paths(&["a.o"]);
        let output = PathBuf::from("out.wasm");
        let plan = LinkPlan {
            target: &target,
            inputs: &inputs,
            output: &output,
            kind: OutputKind::Executable,
            libs: &[],
            lib_search_paths: &[],
            extra_args: &[],
        };
        let argv = plan.argv().unwrap();
        assert!(contains(&argv, "-o"));
        assert!(contains(&argv, "out.wasm"));
        assert!(contains(&argv, "--gc-sections"));
    }

    #[test]
    fn wasm_ld_dynamic_adds_no_entry() {
        let target = cfg(Arch::Wasm32, Os::BareMetal, AbiVariant::None);
        let inputs = paths(&["a.o"]);
        let output = PathBuf::from("out.wasm");
        let plan = LinkPlan {
            target: &target,
            inputs: &inputs,
            output: &output,
            kind: OutputKind::DynamicLibrary,
            libs: &[],
            lib_search_paths: &[],
            extra_args: &[],
        };
        let argv = plan.argv().unwrap();
        assert!(contains(&argv, "--no-entry"));
    }

    #[test]
    fn llvm_ar_argv_uses_crs_and_positional_output() {
        let target = cfg(Arch::X86_64, Os::Linux, AbiVariant::Gnu);
        let inputs = paths(&["a.o", "b.o"]);
        let output = PathBuf::from("libfoo.a");
        let plan = LinkPlan {
            target: &target,
            inputs: &inputs,
            output: &output,
            kind: OutputKind::StaticLibrary,
            libs: &[],
            lib_search_paths: &[],
            extra_args: &[],
        };
        let argv = plan.argv().unwrap();
        assert_eq!(argv[0], "crs");
        assert_eq!(argv[1], "libfoo.a");
        assert!(contains(&argv, "a.o"));
        assert!(contains(&argv, "b.o"));
    }

    #[test]
    fn llvm_ar_picked_for_macos_static_lib() {
        let target = cfg(Arch::Aarch64, Os::Macos, AbiVariant::Darwin);
        let inputs = paths(&["a.o"]);
        let output = PathBuf::from("libfoo.a");
        let plan = LinkPlan {
            target: &target,
            inputs: &inputs,
            output: &output,
            kind: OutputKind::StaticLibrary,
            libs: &[],
            lib_search_paths: &[],
            extra_args: &[],
        };
        assert!(matches!(
            plan.tool().unwrap(),
            Tool::Archive(Archiver::LlvmAr),
        ));
    }

    #[test]
    fn llvm_lib_argv_uses_slash_out() {
        let target = cfg(Arch::X86_64, Os::Windows, AbiVariant::Msvc);
        let inputs = paths(&["a.obj", "b.obj"]);
        let output = PathBuf::from("foo.lib");
        let plan = LinkPlan {
            target: &target,
            inputs: &inputs,
            output: &output,
            kind: OutputKind::StaticLibrary,
            libs: &[],
            lib_search_paths: &[],
            extra_args: &[],
        };
        let argv = plan.argv().unwrap();
        assert_eq!(argv[0], "/OUT:foo.lib");
        assert!(contains(&argv, "a.obj"));
        assert!(contains(&argv, "b.obj"));
    }

    #[test]
    fn wasm_static_library_picks_llvm_ar() {
        let target = cfg(Arch::Wasm32, Os::BareMetal, AbiVariant::None);
        let inputs = paths(&["a.o"]);
        let output = PathBuf::from("libwasm.a");
        let plan = LinkPlan {
            target: &target,
            inputs: &inputs,
            output: &output,
            kind: OutputKind::StaticLibrary,
            libs: &[],
            lib_search_paths: &[],
            extra_args: &[],
        };
        assert!(matches!(
            plan.tool().unwrap(),
            Tool::Archive(Archiver::LlvmAr),
        ));
    }
}
