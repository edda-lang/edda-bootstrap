//! LLVM relocation-model selection per target.
//!
//! The relocation model controls whether the backend assumes its code
//! and data live at a fixed address (Static) or must be patched at load
//! time (Pic / DynamicNoPic). The model interacts with the linker: a
//! `Pic` object file relocated into a `Static`-only binary errors out
//! at link, and vice-versa. The choice is also security-relevant — ASLR
//! requires position-independent executables.
//!
//! Edda v0.1 picks the platform-conventional default at every target:
//! Pic on user-space OSes (Linux, macOS, Windows, FreeBSD), Static on
//! bare-metal and WebAssembly. Future profiles can override per
//! invocation (e.g., `--reloc-model=static` for a self-contained
//! executable) — the override path is not yet wired here.

use edda_target::{Arch, Os, TargetTriple};

/// LLVM relocation model.
///
/// LangRef nomenclature; verified against LLVM 18's `LLVMRelocMode`
/// enum. When the inkwell binding lands, callers obtain the numeric
/// discriminant via inkwell's `RelocMode` rather than parsing the name.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum RelocModel {
    /// No relocation. Code and data live at the addresses the linker
    /// chose. Used by bare-metal images, kernel binaries, and
    /// self-contained executables that opt out of ASLR.
    Static,
    /// Position-independent code. Every external reference goes through
    /// a relocation that the loader fixes up. Required for shared
    /// libraries and for ASLR-enabled executables on Linux / macOS.
    Pic,
    /// Position-independent but with the legacy "dynamic-no-pic"
    /// constraint on macOS — symbol references are PIC but TLS goes
    /// through the dynamic loader. Rarely used outside legacy Darwin
    /// configurations.
    DynamicNoPic,
    /// Read-only data is position-independent, code is not. ARM-only,
    /// used for tightly-coupled flash + RAM images.
    Ropi,
    /// Writable data is position-independent. ARM-only counterpart to
    /// [`RelocModel::Ropi`].
    Rwpi,
    /// Both read-only and read-write data are position-independent.
    /// ARM-only combination of [`RelocModel::Ropi`] and
    /// [`RelocModel::Rwpi`].
    RopiRwpi,
}

impl RelocModel {
    /// LLVM `-relocation-model=...` spelling (also the LangRef name).
    pub const fn cli_name(self) -> &'static str {
        match self {
            RelocModel::Static => "static",
            RelocModel::Pic => "pic",
            RelocModel::DynamicNoPic => "dynamic-no-pic",
            RelocModel::Ropi => "ropi",
            RelocModel::Rwpi => "rwpi",
            RelocModel::RopiRwpi => "ropi-rwpi",
        }
    }
}

impl std::fmt::Display for RelocModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.cli_name())
    }
}

#[cfg(feature = "llvm")]
impl RelocModel {
    /// Map this [`RelocModel`] onto inkwell's `RelocMode`.
    ///
    /// The ROPI / RWPI / ROPI_RWPI family has no representation in
    /// inkwell 0.5's `RelocMode`; those Edda variants fall back to
    /// `Default`. A future change will surface a `simd_target_unsupported`-style
    /// rejection for callers that explicitly request an ROPI-family
    /// model outside the supported arches.
    pub(crate) const fn to_inkwell(self) -> inkwell::targets::RelocMode {
        use inkwell::targets::RelocMode;
        match self {
            RelocModel::Static => RelocMode::Static,
            RelocModel::Pic => RelocMode::PIC,
            RelocModel::DynamicNoPic => RelocMode::DynamicNoPic,
            RelocModel::Ropi | RelocModel::Rwpi | RelocModel::RopiRwpi => RelocMode::Default,
        }
    }
}

/// Platform-conventional relocation model for `triple`.
///
/// User-space OSes (Linux, macOS, Windows, FreeBSD, WASI) default to
/// [`RelocModel::Pic`] — the modern ASLR-friendly choice, also the
/// default for Rust's matching targets. Bare-metal and WebAssembly
/// archs default to [`RelocModel::Static`] — Wasm because the runtime
/// owns the address space layout, bare-metal because the firmware
/// image lives at a fixed address.
///
/// The override path (manifest `relocation_model = "static"`, CLI
/// `--reloc-model=...`) is reserved; once it lands, validation must
/// run through [`is_admissible`] before constructing the
/// `TargetMachine`.
pub const fn default_for_target(triple: &TargetTriple) -> RelocModel {
    match (triple.arch(), triple.os()) {
        // Wasm: runtime owns the address space.
        (Arch::Wasm32 | Arch::Wasm64, _) => RelocModel::Static,
        // Bare-metal: fixed-address firmware image.
        (_, Os::BareMetal) => RelocModel::Static,
        // User-space everything else: PIC is the modern default.
        _ => RelocModel::Pic,
    }
}

/// Whether `model` is admissible on `arch`. Used by manifest /
/// CLI validation when a future profile asks for a non-default model.
///
/// The ROPI / RWPI / ROPI_RWPI family is ARM-only; LLVM 18 emits an
/// error for those models on non-ARM backends. Static, Pic, and
/// DynamicNoPic are accepted on every v0.1 arch — LLVM accepts them,
/// even where the choice is a no-op (Wasm, bare-metal Static).
#[allow(dead_code)] // manifest / CLI override path not yet wired; retained for the locked table
pub(crate) const fn is_admissible(arch: Arch, model: RelocModel) -> bool {
    match model {
        RelocModel::Static | RelocModel::Pic | RelocModel::DynamicNoPic => true,
        RelocModel::Ropi | RelocModel::Rwpi | RelocModel::RopiRwpi => matches!(arch, Arch::Aarch64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edda_target::{AbiVariant, TargetTriple};

    fn t(arch: Arch, os: Os, abi: AbiVariant) -> TargetTriple {
        TargetTriple::new(arch, os, abi)
    }

    #[test]
    fn linux_targets_default_to_pic() {
        assert_eq!(
            default_for_target(&t(Arch::X86_64, Os::Linux, AbiVariant::Gnu)),
            RelocModel::Pic,
        );
        assert_eq!(
            default_for_target(&t(Arch::Aarch64, Os::Linux, AbiVariant::Gnu)),
            RelocModel::Pic,
        );
    }

    #[test]
    fn macos_windows_freebsd_wasi_default_to_pic() {
        assert_eq!(
            default_for_target(&t(Arch::Aarch64, Os::Macos, AbiVariant::Darwin)),
            RelocModel::Pic,
        );
        assert_eq!(
            default_for_target(&t(Arch::X86_64, Os::Windows, AbiVariant::Msvc)),
            RelocModel::Pic,
        );
        assert_eq!(
            default_for_target(&t(Arch::X86_64, Os::Freebsd, AbiVariant::Gnu)),
            RelocModel::Pic,
        );
        // Non-Wasm arch paired with Wasi (not a v0.1 target but the
        // function is total): user-space convention applies → Pic.
        assert_eq!(
            default_for_target(&t(Arch::X86_64, Os::Wasi, AbiVariant::WasiPreview1)),
            RelocModel::Pic,
        );
    }

    #[test]
    fn bare_metal_defaults_to_static() {
        assert_eq!(
            default_for_target(&t(Arch::Riscv64, Os::BareMetal, AbiVariant::None)),
            RelocModel::Static,
        );
        assert_eq!(
            default_for_target(&t(Arch::Aarch64, Os::BareMetal, AbiVariant::None)),
            RelocModel::Static,
        );
    }

    #[test]
    fn wasm_archs_default_to_static_regardless_of_os() {
        for os in [Os::Wasi, Os::Linux, Os::BareMetal] {
            for arch in [Arch::Wasm32, Arch::Wasm64] {
                let triple = t(arch, os, AbiVariant::WasiPreview1);
                assert_eq!(
                    default_for_target(&triple),
                    RelocModel::Static,
                    "non-Static default for {triple}",
                );
            }
        }
    }

    #[test]
    fn cli_names_match_llvm() {
        assert_eq!(RelocModel::Static.cli_name(), "static");
        assert_eq!(RelocModel::Pic.cli_name(), "pic");
        assert_eq!(RelocModel::DynamicNoPic.cli_name(), "dynamic-no-pic");
        assert_eq!(RelocModel::Ropi.cli_name(), "ropi");
        assert_eq!(RelocModel::Rwpi.cli_name(), "rwpi");
        assert_eq!(RelocModel::RopiRwpi.cli_name(), "ropi-rwpi");
    }

    #[test]
    fn static_pic_dynamicnopic_admissible_on_every_arch() {
        for arch in Arch::ALL {
            for m in [RelocModel::Static, RelocModel::Pic, RelocModel::DynamicNoPic] {
                assert!(is_admissible(arch, m), "{arch:?} / {m:?}");
            }
        }
    }

    #[test]
    fn ropi_family_aarch64_only() {
        for m in [RelocModel::Ropi, RelocModel::Rwpi, RelocModel::RopiRwpi] {
            assert!(is_admissible(Arch::Aarch64, m), "{m:?}");
            for other in [Arch::X86_64, Arch::Riscv64, Arch::Wasm32, Arch::Wasm64] {
                assert!(!is_admissible(other, m), "{other:?} / {m:?}");
            }
        }
    }

    #[test]
    fn display_matches_cli_name() {
        assert_eq!(format!("{}", RelocModel::Pic), "pic");
        assert_eq!(format!("{}", RelocModel::RopiRwpi), "ropi-rwpi");
    }
}
