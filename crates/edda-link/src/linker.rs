//! Linker selection and `$PATH` discovery.
//!
//! mold for Linux ELF when host == target; lld variants everywhere else
//! (`build-system.md` §5b). Cross-link is supported via the lld family
//! (`ld.lld`, `ld64.lld`, `lld-link`, `wasm-ld`): lld runs on any host
//! and can produce binaries for any of its supported object formats.
//! mold is Linux-only as a host tool but accepts both x86_64 and
//! aarch64 ELF inputs, so it's the preferred Linux→Linux path.

use std::path::PathBuf;

use edda_target::{Os, TargetTriple};

use crate::error::LinkError;

/// Locked v0.1 linker set.
///
/// One variant per linker toolchain. `tool_name()` is the executable
/// name searched on `$PATH` by [`Linker::discover`]. Linker selection
/// is target-OS-driven; the host enters only to disambiguate Linux
/// targets (mold preferred on a Linux host, `ld.lld` everywhere else).
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Linker {
    /// `mold` — Linux ELF linker. Linux host only.
    Mold,
    /// `ld.lld` — lld's ELF port. Cross-platform host; used as the
    /// Linux-target fallback when the host is not Linux.
    LdLld,
    /// `ld64.lld` — lld's Mach-O port. Cross-platform host.
    Ld64Lld,
    /// `lld-link` — lld's COFF / link.exe-compatible port.
    /// Cross-platform host.
    LldLink,
    /// `wasm-ld` — lld's WebAssembly port. Cross-platform host.
    WasmLd,
}

impl Linker {
    /// Executable name searched on `$PATH`.
    ///
    /// On Windows, [`Linker::discover`] appends `.exe` automatically;
    /// the value returned here is the unsuffixed name.
    pub const fn tool_name(self) -> &'static str {
        match self {
            Self::Mold => "mold",
            Self::LdLld => "ld.lld",
            Self::Ld64Lld => "ld64.lld",
            Self::LldLink => "lld-link",
            Self::WasmLd => "wasm-ld",
        }
    }

    /// Select the linker for `triple`.
    ///
    /// Returns [`LinkError::UnsupportedTargetOs`] if the triple's OS
    /// is not in the v0.1 linker matrix (`Linux`, `Macos`, `Windows`,
    /// and `BareMetal` / `Browser` for wasm → `wasm-ld`). Cross-link
    /// from any host to any supported target is accepted: lld's variants
    /// run on any host.
    pub fn for_target(triple: &TargetTriple) -> Result<Linker, LinkError> {
        match triple.os() {
            Os::Linux if host_is_linux() => Ok(Linker::Mold),
            Os::Linux => Ok(Linker::LdLld),
            Os::Macos => Ok(Linker::Ld64Lld),
            Os::Windows => Ok(Linker::LldLink),
            Os::BareMetal | Os::Browser => Ok(Linker::WasmLd),
            os @ (Os::Freebsd | Os::Wasi) => Err(LinkError::UnsupportedTargetOs(os)),
        }
    }

    /// Resolve [`Self::tool_name`] against `$PATH`.
    ///
    /// On Windows, both the bare name and the `.exe`-suffixed form are
    /// searched. Returns [`LinkError::ToolNotFound`] if no matching
    /// executable exists.
    pub fn discover(self) -> Result<PathBuf, LinkError> {
        crate::tool::discover_on_path(self.tool_name()).map_err(|searched_path| {
            LinkError::ToolNotFound {
                tool: crate::Tool::Link(self),
                searched_path,
            }
        })
    }
}

/// Compile-time host-OS classification.
const fn host_is_linux() -> bool {
    cfg!(target_os = "linux")
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
        assert_eq!(Linker::Mold.tool_name(), "mold");
        assert_eq!(Linker::LdLld.tool_name(), "ld.lld");
        assert_eq!(Linker::Ld64Lld.tool_name(), "ld64.lld");
        assert_eq!(Linker::LldLink.tool_name(), "lld-link");
        assert_eq!(Linker::WasmLd.tool_name(), "wasm-ld");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_target_on_linux_host_picks_mold() {
        let t = triple(Arch::X86_64, Os::Linux, AbiVariant::Gnu);
        assert_eq!(Linker::for_target(&t).unwrap(), Linker::Mold);
        let t = triple(Arch::Aarch64, Os::Linux, AbiVariant::Gnu);
        assert_eq!(Linker::for_target(&t).unwrap(), Linker::Mold);
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn linux_target_on_non_linux_host_picks_ld_lld() {
        let t = triple(Arch::X86_64, Os::Linux, AbiVariant::Gnu);
        assert_eq!(Linker::for_target(&t).unwrap(), Linker::LdLld);
    }

    #[test]
    fn macos_target_picks_ld64_lld_on_any_host() {
        let t = triple(Arch::Aarch64, Os::Macos, AbiVariant::Darwin);
        assert_eq!(Linker::for_target(&t).unwrap(), Linker::Ld64Lld);
    }

    #[test]
    fn windows_target_picks_lld_link_on_any_host() {
        let t = triple(Arch::X86_64, Os::Windows, AbiVariant::Msvc);
        assert_eq!(Linker::for_target(&t).unwrap(), Linker::LldLink);
    }

    #[test]
    fn wasm32_baremetal_picks_wasm_ld_on_any_host() {
        let t = triple(Arch::Wasm32, Os::BareMetal, AbiVariant::None);
        assert_eq!(Linker::for_target(&t).unwrap(), Linker::WasmLd);
    }

    #[test]
    fn wasm_browser_picks_wasm_ld_on_any_host() {
        // Browser wasm links through wasm-ld,
        // matching the native link matrix (`Os.browser -> Linker.wasm_ld`).
        let t = triple(Arch::Wasm32, Os::Browser, AbiVariant::None);
        assert_eq!(Linker::for_target(&t).unwrap(), Linker::WasmLd);
        let t = triple(Arch::Wasm64, Os::Browser, AbiVariant::None);
        assert_eq!(Linker::for_target(&t).unwrap(), Linker::WasmLd);
    }

    #[test]
    fn unsupported_target_os_returns_error() {
        let t = triple(Arch::X86_64, Os::Freebsd, AbiVariant::Gnu);
        let err = Linker::for_target(&t).unwrap_err();
        assert!(matches!(err, LinkError::UnsupportedTargetOs(Os::Freebsd)));
        let t = triple(Arch::Wasm32, Os::Wasi, AbiVariant::WasiPreview1);
        let err = Linker::for_target(&t).unwrap_err();
        assert!(matches!(err, LinkError::UnsupportedTargetOs(Os::Wasi)));
    }
}
