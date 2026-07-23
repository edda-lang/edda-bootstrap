//! Static target metadata: LLVM triple, pointer width, endianness.
//!
//! These are the (arch, os, abi)-derived facts the LLVM binding will need
//! before it can build a `TargetMachine`. They are pure functions of the
//! locked triple grammar and require no LLVM dependency.
//!
//! # What is intentionally *not* here
//!
//! Data-layout strings are not hardcoded. Once the inkwell binding lands,
//! the canonical source is `TargetMachine::get_target_data().get_data_layout()`,
//! anchored to the actual LLVM version in use. Hardcoding the strings here
//! would drift the moment LLVM is bumped — see the crate-level doc in
//! `lib.rs`.

use edda_target::{AbiVariant, Arch, Os, TargetTriple};

/// Byte order. All v0.1 architectures are little-endian; the enum exists
/// so downstream code (object emission, layout printing) can name byte
/// order explicitly without ad-hoc booleans.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Endianness {
    /// Little-endian.
    Little,
    /// Big-endian.
    Big,
}

/// Pointer width in bits for the given architecture.
pub const fn pointer_width(arch: Arch) -> u32 {
    match arch {
        Arch::X86_64 | Arch::Aarch64 | Arch::Riscv64 | Arch::Wasm64 => 64,
        Arch::Wasm32 => 32,
    }
}

/// Native byte order for the given architecture.
pub const fn endianness(arch: Arch) -> Endianness {
    match arch {
        Arch::X86_64 | Arch::Aarch64 | Arch::Riscv64 | Arch::Wasm32 | Arch::Wasm64 => {
            Endianness::Little
        }
    }
}

/// LLVM target-triple string for an Edda triple.
///
/// Edda's triple grammar (`<arch>-<os>-<abi>`, build-system.md §9) is
/// three components; LLVM's triple is conventionally four
/// (`<arch>-<vendor>-<sys>-<env>`). Apple targets use the historical
/// 3-part `<arch>-apple-darwin` spelling.
///
/// Pairings outside the locked v0.1 target set get a best-effort mapping
/// rather than a panic, so the function stays total for direct callers;
/// the driver should reject invalid triples upstream via manifest /
/// CLI validation.
pub fn llvm_triple(triple: &TargetTriple) -> String {
    let arch = llvm_arch(triple.arch());
    let (vendor, sys, env) = match triple.arch() {
        // WebAssembly's object format is inferred from the arch, not an
        // environment token. Appending an object-format env like `-elf`
        // (the bare-metal `None`-ABI mapping) forces LLVM's `getObjectFormat`
        // to ELF on a Wasm arch; the Wasm AsmPrinter then drives an ELF
        // object streamer at emit time and the mismatch segfaults inside
        // `TargetMachine::write_to_file`. The
        // canonical Wasm triples carry no object-format env.
        Arch::Wasm32 | Arch::Wasm64 => llvm_wasm_sys_components(triple.os()),
        _ => llvm_sys_components(triple.os(), triple.abi()),
    };
    match env {
        Some(env) => format!("{arch}-{vendor}-{sys}-{env}"),
        None => format!("{arch}-{vendor}-{sys}"),
    }
}

// triple's object format is arch-determined; an env token would force a
// non-Wasm format and crash object emission
// Wasm OS (`BareMetal`, `Browser`, host-on-wasm fallback) → `-unknown-unknown`
/// LLVM `(vendor, sys, env)` for a WebAssembly target.
///
/// WASI maps to `wasm{32,64}-unknown-wasi`; every other Wasm OS (bare-metal,
/// browser, or a host OS paired with a Wasm arch) maps to the freestanding
/// `wasm{32,64}-unknown-unknown` triple. No arm carries an object-format
/// environment token — that is what keeps LLVM emitting Wasm objects.
const fn llvm_wasm_sys_components(os: Os) -> (&'static str, &'static str, Option<&'static str>) {
    match os {
        Os::Wasi => ("unknown", "wasi", None),
        Os::Linux | Os::Windows | Os::Macos | Os::Freebsd | Os::BareMetal | Os::Browser => {
            ("unknown", "unknown", None)
        }
    }
}

const fn llvm_arch(arch: Arch) -> &'static str {
    match arch {
        Arch::X86_64 => "x86_64",
        Arch::Aarch64 => "aarch64",
        Arch::Riscv64 => "riscv64",
        Arch::Wasm32 => "wasm32",
        Arch::Wasm64 => "wasm64",
    }
}

fn llvm_sys_components(
    os: Os,
    abi: AbiVariant,
) -> (&'static str, &'static str, Option<&'static str>) {
    match (os, abi) {
        (Os::Linux, AbiVariant::Gnu) => ("unknown", "linux", Some("gnu")),
        (Os::Linux, AbiVariant::Musl) => ("unknown", "linux", Some("musl")),
        (Os::Windows, AbiVariant::Msvc) => ("pc", "windows", Some("msvc")),
        (Os::Windows, AbiVariant::Gnu) => ("pc", "windows", Some("gnu")),
        (Os::Macos, AbiVariant::Darwin) => ("apple", "darwin", None),
        (Os::Freebsd, AbiVariant::Gnu) => ("unknown", "freebsd", None),
        (Os::Freebsd, AbiVariant::Musl) => ("unknown", "freebsd", Some("musl")),
        (Os::BareMetal, AbiVariant::None) => ("unknown", "none", Some("elf")),
        // Wasm OSes never reach here — `llvm_triple` routes every Wasm arch
        // through `llvm_wasm_sys_components`.
        // Not in the v0.1 target set; emit a structurally-valid string so
        // direct callers (e.g., test fixtures, future targets) get a
        // round-trippable result rather than a panic.
        (os, abi) => ("unknown", os_fallback_name(os), abi_fallback_env(abi)),
    }
}

const fn os_fallback_name(os: Os) -> &'static str {
    match os {
        Os::Linux => "linux",
        Os::Windows => "windows",
        Os::Macos => "darwin",
        Os::Freebsd => "freebsd",
        Os::BareMetal => "none",
        Os::Wasi => "wasi",
        Os::Browser => "unknown",
    }
}

const fn abi_fallback_env(abi: AbiVariant) -> Option<&'static str> {
    match abi {
        AbiVariant::Gnu => Some("gnu"),
        AbiVariant::Musl => Some("musl"),
        AbiVariant::Msvc => Some("msvc"),
        AbiVariant::Darwin => None,
        AbiVariant::None => Some("elf"),
        AbiVariant::WasiPreview1 => None,
    }
}

/// Initialise the LLVM backend for the given Edda [`Arch`].
///
/// LLVM's per-arch `Target::initialize_*` functions are idempotent so
/// repeated calls register the backend exactly once at LLVM's internal
/// layer.
#[cfg(feature = "llvm")]
pub(crate) fn initialize_for_arch(arch: Arch) {
    use inkwell::targets::{InitializationConfig, Target};
    let cfg = InitializationConfig::default();
    match arch {
        Arch::X86_64 => Target::initialize_x86(&cfg),
        Arch::Aarch64 => Target::initialize_aarch64(&cfg),
        Arch::Riscv64 => Target::initialize_riscv(&cfg),
        Arch::Wasm32 | Arch::Wasm64 => Target::initialize_webassembly(&cfg),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(arch: Arch, os: Os, abi: AbiVariant) -> TargetTriple {
        TargetTriple::new(arch, os, abi)
    }

    #[test]
    fn pointer_width_for_v0_1_archs() {
        assert_eq!(pointer_width(Arch::X86_64), 64);
        assert_eq!(pointer_width(Arch::Aarch64), 64);
        assert_eq!(pointer_width(Arch::Riscv64), 64);
        assert_eq!(pointer_width(Arch::Wasm32), 32);
        assert_eq!(pointer_width(Arch::Wasm64), 64);
    }

    #[test]
    fn every_v0_1_arch_is_little_endian() {
        for arch in Arch::ALL {
            assert_eq!(endianness(arch), Endianness::Little);
        }
    }

    #[test]
    fn linux_gnu_triples() {
        assert_eq!(
            llvm_triple(&t(Arch::X86_64, Os::Linux, AbiVariant::Gnu)),
            "x86_64-unknown-linux-gnu",
        );
        assert_eq!(
            llvm_triple(&t(Arch::Aarch64, Os::Linux, AbiVariant::Gnu)),
            "aarch64-unknown-linux-gnu",
        );
    }

    #[test]
    fn linux_musl_triple() {
        assert_eq!(
            llvm_triple(&t(Arch::X86_64, Os::Linux, AbiVariant::Musl)),
            "x86_64-unknown-linux-musl",
        );
    }

    #[test]
    fn windows_msvc_triple() {
        assert_eq!(
            llvm_triple(&t(Arch::X86_64, Os::Windows, AbiVariant::Msvc)),
            "x86_64-pc-windows-msvc",
        );
    }

    #[test]
    fn windows_gnu_triple() {
        assert_eq!(
            llvm_triple(&t(Arch::X86_64, Os::Windows, AbiVariant::Gnu)),
            "x86_64-pc-windows-gnu",
        );
    }

    #[test]
    fn macos_darwin_is_three_part() {
        assert_eq!(
            llvm_triple(&t(Arch::Aarch64, Os::Macos, AbiVariant::Darwin)),
            "aarch64-apple-darwin",
        );
        assert_eq!(
            llvm_triple(&t(Arch::X86_64, Os::Macos, AbiVariant::Darwin)),
            "x86_64-apple-darwin",
        );
    }

    #[test]
    fn riscv64_bare_metal_triple() {
        assert_eq!(
            llvm_triple(&t(Arch::Riscv64, Os::BareMetal, AbiVariant::None)),
            "riscv64-unknown-none-elf",
        );
    }

    #[test]
    fn wasm32_wasi_is_three_part() {
        assert_eq!(
            llvm_triple(&t(Arch::Wasm32, Os::Wasi, AbiVariant::WasiPreview1)),
            "wasm32-unknown-wasi",
        );
    }

    #[test]
    fn browser_wasm_triple_has_no_object_format_env() {
        // The browser-OS wasm target maps to
        // LLVM's `wasm32-unknown-unknown` form (no object-format env).
        assert_eq!(
            llvm_triple(&t(Arch::Wasm32, Os::Browser, AbiVariant::None)),
            "wasm32-unknown-unknown",
        );
    }

    #[test]
    fn wasm_freestanding_never_carries_object_format_env() {
        // A Wasm triple must NOT end in an
        // object-format env (`-elf`/`-gnu`/...). The `None`-ABI bare-metal
        // mapping appends `-elf` for ELF arches; on Wasm that token forces
        // LLVM to ELF object format and segfaults object emission. Every
        // freestanding Wasm triple must be `wasm{32,64}-unknown-unknown`.
        assert_eq!(
            llvm_triple(&t(Arch::Wasm32, Os::BareMetal, AbiVariant::None)),
            "wasm32-unknown-unknown",
        );
        assert_eq!(
            llvm_triple(&t(Arch::Wasm64, Os::BareMetal, AbiVariant::None)),
            "wasm64-unknown-unknown",
        );
        // WASI keeps its canonical `-wasi` sys component (also env-free).
        assert_eq!(
            llvm_triple(&t(Arch::Wasm32, Os::Wasi, AbiVariant::WasiPreview1)),
            "wasm32-unknown-wasi",
        );
        // No Wasm triple may carry an `-elf` object-format suffix.
        for os in [Os::BareMetal, Os::Wasi, Os::Browser] {
            for arch in [Arch::Wasm32, Arch::Wasm64] {
                let abi = if os == Os::Wasi {
                    AbiVariant::WasiPreview1
                } else {
                    AbiVariant::None
                };
                let s = llvm_triple(&t(arch, os, abi));
                assert!(!s.ends_with("-elf"), "wasm triple carried -elf: {s}");
            }
        }
    }

    #[test]
    fn fallback_combinations_stay_well_formed() {
        // Non-locked pairing: x86-64 + bare-metal + gnu. The function does
        // not validate; it produces a structurally-valid LLVM-shaped string.
        let s = llvm_triple(&t(Arch::X86_64, Os::BareMetal, AbiVariant::Gnu));
        assert!(s.starts_with("x86_64-unknown-"), "got: {s}");
        assert_eq!(s.matches('-').count(), 3);
    }
}
