//! Object-file format selection per (arch, os).
//!
//! Per build-system.md §5b the output object-file format is determined by
//! the OS (and by the arch in the WebAssembly case). The linker driver in
//! `edda-link` consumes this enum to pick the right linker — mold for
//! ELF, lld for Mach-O / COFF, wasm-ld for Wasm.

use edda_target::{Arch, Os};

/// On-disk format of the object files emitted by this backend.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum ObjectFormat {
    /// Executable and Linkable Format (Linux, FreeBSD, bare-metal).
    Elf,
    /// Mach-O (macOS).
    MachO,
    /// Common Object File Format (Windows).
    Coff,
    /// WebAssembly module (wasm32, wasm64).
    Wasm,
}

impl ObjectFormat {
    /// Conventional object-file extension for this format, without a
    /// leading `.`.
    pub const fn extension(self) -> &'static str {
        match self {
            ObjectFormat::Elf => "o",
            ObjectFormat::MachO => "o",
            ObjectFormat::Coff => "obj",
            ObjectFormat::Wasm => "wasm",
        }
    }
}

impl std::fmt::Display for ObjectFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            ObjectFormat::Elf => "ELF",
            ObjectFormat::MachO => "Mach-O",
            ObjectFormat::Coff => "COFF",
            ObjectFormat::Wasm => "Wasm",
        };
        f.write_str(name)
    }
}

/// Object-file format for the given (arch, os) pair.
///
/// WebAssembly archs (`wasm32`, `wasm64`) always emit `.wasm` regardless
/// of the OS component — the OS in a Wasm triple records the *host
/// environment* (e.g., `wasi`), not a binary format. For non-Wasm archs
/// the OS chooses: ELF for Linux / FreeBSD / bare-metal, Mach-O for
/// macOS, COFF for Windows.
///
/// The pairing `(non-Wasm arch, Os::Wasi)` is not in any v0.1 target;
/// the function returns [`ObjectFormat::Elf`] for that case so it stays
/// total, but driver-level validation should reject the triple before it
/// reaches this code.
pub const fn object_format(arch: Arch, os: Os) -> ObjectFormat {
    match arch {
        Arch::Wasm32 | Arch::Wasm64 => ObjectFormat::Wasm,
        Arch::X86_64 | Arch::Aarch64 | Arch::Riscv64 => match os {
            Os::Linux | Os::Freebsd | Os::BareMetal | Os::Wasi | Os::Browser => {
                ObjectFormat::Elf
            }
            Os::Macos => ObjectFormat::MachO,
            Os::Windows => ObjectFormat::Coff,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x86_64_linux_is_elf() {
        assert_eq!(object_format(Arch::X86_64, Os::Linux), ObjectFormat::Elf);
    }

    #[test]
    fn x86_64_windows_is_coff() {
        assert_eq!(object_format(Arch::X86_64, Os::Windows), ObjectFormat::Coff);
    }

    #[test]
    fn aarch64_macos_is_macho() {
        assert_eq!(
            object_format(Arch::Aarch64, Os::Macos),
            ObjectFormat::MachO
        );
    }

    #[test]
    fn wasm32_wasi_is_wasm() {
        assert_eq!(object_format(Arch::Wasm32, Os::Wasi), ObjectFormat::Wasm);
    }

    #[test]
    fn wasm_archs_are_wasm_regardless_of_os() {
        for arch in [Arch::Wasm32, Arch::Wasm64] {
            for os in Os::ALL {
                assert_eq!(object_format(arch, os), ObjectFormat::Wasm);
            }
        }
    }

    #[test]
    fn riscv64_bare_metal_is_elf() {
        assert_eq!(
            object_format(Arch::Riscv64, Os::BareMetal),
            ObjectFormat::Elf
        );
    }

    #[test]
    fn freebsd_is_elf() {
        assert_eq!(object_format(Arch::X86_64, Os::Freebsd), ObjectFormat::Elf);
    }

    #[test]
    fn extensions_have_no_leading_dot() {
        for fmt in [
            ObjectFormat::Elf,
            ObjectFormat::MachO,
            ObjectFormat::Coff,
            ObjectFormat::Wasm,
        ] {
            assert!(!fmt.extension().starts_with('.'), "{fmt:?}");
        }
    }

    #[test]
    fn coff_extension_is_obj() {
        assert_eq!(ObjectFormat::Coff.extension(), "obj");
    }

    #[test]
    fn wasm_extension_is_wasm() {
        assert_eq!(ObjectFormat::Wasm.extension(), "wasm");
    }

    #[test]
    fn display_renders_canonical_names() {
        assert_eq!(format!("{}", ObjectFormat::Elf), "ELF");
        assert_eq!(format!("{}", ObjectFormat::MachO), "Mach-O");
        assert_eq!(format!("{}", ObjectFormat::Coff), "COFF");
        assert_eq!(format!("{}", ObjectFormat::Wasm), "Wasm");
    }
}
