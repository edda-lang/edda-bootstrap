//! Static-archive driver and `$PATH` discovery.
//!
//! `llvm-ar` produces System V / BSD-format `.a` archives for ELF,
//! Mach-O, and WebAssembly object files. `llvm-lib` produces
//! Microsoft-format `.lib` archives for COFF/MSVC object files.
//! Both ship with the LLVM toolchain that's already required for the
//! compiler backend (`backend-choice.md` §6.7).

use std::path::PathBuf;

use edda_target::{Os, TargetTriple};

use crate::error::LinkError;

/// Static-archive driver.
///
/// One variant per archive format. `LlvmAr` produces System V / BSD
/// `.a` archives (used for ELF, Mach-O, and wasm objects); `LlvmLib`
/// produces Microsoft `.lib` archives for COFF objects.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Archiver {
    /// `llvm-ar` — System V / BSD `.a` archives.
    LlvmAr,
    /// `llvm-lib` — Microsoft `.lib` archives.
    LlvmLib,
}

impl Archiver {
    /// Executable name searched on `$PATH`.
    pub const fn tool_name(self) -> &'static str {
        match self {
            Self::LlvmAr => "llvm-ar",
            Self::LlvmLib => "llvm-lib",
        }
    }

    /// Select the archiver for `triple`.
    ///
    /// Returns [`LinkError::UnsupportedTargetOs`] for OSes outside the
    /// v0.1 target matrix.
    pub fn for_target(triple: &TargetTriple) -> Result<Archiver, LinkError> {
        match triple.os() {
            Os::Linux | Os::Macos | Os::BareMetal | Os::Browser => Ok(Archiver::LlvmAr),
            Os::Windows => Ok(Archiver::LlvmLib),
            os @ (Os::Freebsd | Os::Wasi) => Err(LinkError::UnsupportedTargetOs(os)),
        }
    }

    /// Resolve [`Self::tool_name`] against `$PATH`.
    pub fn discover(self) -> Result<PathBuf, LinkError> {
        crate::tool::discover_on_path(self.tool_name()).map_err(|searched_path| {
            LinkError::ToolNotFound {
                tool: crate::Tool::Archive(self),
                searched_path,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edda_target::{AbiVariant, Arch, Os, TargetTriple};

    fn triple(arch: Arch, os: Os, abi: AbiVariant) -> TargetTriple {
        TargetTriple::new(arch, os, abi)
    }

    #[test]
    fn tool_names_are_stable() {
        assert_eq!(Archiver::LlvmAr.tool_name(), "llvm-ar");
        assert_eq!(Archiver::LlvmLib.tool_name(), "llvm-lib");
    }

    #[test]
    fn linux_target_picks_llvm_ar() {
        let t = triple(Arch::X86_64, Os::Linux, AbiVariant::Gnu);
        assert_eq!(Archiver::for_target(&t).unwrap(), Archiver::LlvmAr);
    }

    #[test]
    fn macos_target_picks_llvm_ar() {
        let t = triple(Arch::Aarch64, Os::Macos, AbiVariant::Darwin);
        assert_eq!(Archiver::for_target(&t).unwrap(), Archiver::LlvmAr);
    }

    #[test]
    fn windows_target_picks_llvm_lib() {
        let t = triple(Arch::X86_64, Os::Windows, AbiVariant::Msvc);
        assert_eq!(Archiver::for_target(&t).unwrap(), Archiver::LlvmLib);
    }

    #[test]
    fn wasm32_target_picks_llvm_ar() {
        let t = triple(Arch::Wasm32, Os::BareMetal, AbiVariant::None);
        assert_eq!(Archiver::for_target(&t).unwrap(), Archiver::LlvmAr);
    }

    #[test]
    fn wasm_browser_target_picks_llvm_ar() {
        // Browser wasm objects archive with
        // llvm-ar, like every other wasm/ELF/Mach-O object.
        let t = triple(Arch::Wasm32, Os::Browser, AbiVariant::None);
        assert_eq!(Archiver::for_target(&t).unwrap(), Archiver::LlvmAr);
    }

    #[test]
    fn unsupported_target_returns_error() {
        let t = triple(Arch::X86_64, Os::Freebsd, AbiVariant::Gnu);
        let err = Archiver::for_target(&t).unwrap_err();
        assert!(matches!(err, LinkError::UnsupportedTargetOs(Os::Freebsd)));
    }
}
