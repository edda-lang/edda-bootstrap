//! LLVM calling-convention selection per target.
//!
//! Two questions live here:
//!
//! 1. **What does `@abi("system")` resolve to on each target?** That is the
//!    *platform default* — the calling convention an unannotated `extern`
//!    function uses to interop with the platform C ABI. Modelled by
//!    [`default_for_target`].
//! 2. **Which LLVM IR calling conventions does Edda need to name
//!    explicitly?** Most of the time `ccc` (LLVM's default C) is correct
//!    because the LLVM `TargetMachine` carries the platform-specific ABI
//!    rules; the explicit variants are only needed when forcing a
//!    non-default convention on the same target (e.g., `win64` on a Linux
//!    host for cross-compiled callbacks, or `sysv64` on a Windows host).
//!
//! The full Edda `@abi("...")` string catalogue is spec-locked in
//! `docs/tooling/abi-and-layout.md`, which is not yet in `corpus/`. When
//! that doc arrives, add the string → [`CallingConv`] table behind a
//! `from_abi_attr` resolver. Until then, downstream code uses
//! [`default_for_target`] for any unannotated extern boundary.

use edda_target::{Arch, TargetTriple};

/// LLVM-level calling convention.
///
/// Every variant maps to a single LLVM IR text-IR spelling
/// ([`CallingConv::llvm_ir_name`]); when the inkwell binding lands, the
/// same mapping feeds inkwell's `Function::set_call_conventions` via the
/// numeric `LLVMCallConv` discriminants.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum CallingConv {
    /// LLVM `ccc` — the platform-default C calling convention. The actual
    /// register/stack rules are determined by the active `TargetMachine`,
    /// not by this enum. Use this for `@abi("system")` on every target
    /// where the platform default is the right choice.
    C,
    /// LLVM `x86_64_sysvcc` — System V AMD64 ABI, used explicitly on
    /// x86_64 to force the SysV convention even when the host platform
    /// default is something else.
    X86_64SysV,
    /// LLVM `win64cc` — Microsoft x64 ABI, used explicitly on x86_64 to
    /// force the Win64 convention even when the host platform default is
    /// something else (e.g., MinGW callbacks on Linux).
    X86_64Win64,
    /// LLVM `fastcc` — caller-optimised convention. Reserved for internal
    /// (non-extern) functions where Edda owns both sides of the call.
    Fast,
    /// LLVM `coldcc` — convention for functions invoked rarely (panic /
    /// abort paths). Reserved for the compiler's own runtime helpers.
    Cold,
}

impl CallingConv {
    /// LLVM IR text spelling of this calling convention.
    ///
    /// Verified against the LLVM 18 language reference (LangRef §"calling
    /// conventions"). When the inkwell binding lands, callers obtain the
    /// numeric `LLVMCallConv` value via inkwell's enum instead of parsing
    /// this string.
    pub const fn llvm_ir_name(self) -> &'static str {
        match self {
            CallingConv::C => "ccc",
            CallingConv::X86_64SysV => "x86_64_sysvcc",
            CallingConv::X86_64Win64 => "win64cc",
            CallingConv::Fast => "fastcc",
            CallingConv::Cold => "coldcc",
        }
    }
}

impl std::fmt::Display for CallingConv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.llvm_ir_name())
    }
}

/// Platform-default calling convention for `triple`.
///
/// Returns [`CallingConv::C`] for every target. The platform-specific
/// register allocation, stack layout, and red-zone rules are determined
/// by the LLVM `TargetMachine` that the inkwell binding builds from the
/// triple — not by this enum. The function exists so call sites name
/// their intent clearly ("use the platform default") rather than
/// hard-coding `CallingConv::C`, and so the contract has a single point
/// to change if a future target requires a different default.
///
/// Use [`CallingConv::X86_64SysV`] / [`CallingConv::X86_64Win64`]
/// explicitly when forcing a non-default convention on x86_64 (e.g.,
/// `@abi("sysv64")` or `@abi("win64")` if Edda exposes them — string
/// catalogue is spec-locked).
pub const fn default_for_target(triple: &TargetTriple) -> CallingConv {
    let _ = triple;
    CallingConv::C
}

/// Convention that an explicit `@abi("sysv64")` / `@abi("win64")`-style
/// annotation forces on x86_64. Returns `None` for archs where the
/// convention does not apply.
///
/// This is the *override* path — for the unannotated platform default,
/// use [`default_for_target`].
pub const fn explicit_x86_64_sysv(arch: Arch) -> Option<CallingConv> {
    match arch {
        Arch::X86_64 => Some(CallingConv::X86_64SysV),
        _ => None,
    }
}

/// Convention that an explicit `@abi("win64")` annotation forces on
/// x86_64. Returns `None` for archs where the convention does not apply.
pub const fn explicit_x86_64_win64(arch: Arch) -> Option<CallingConv> {
    match arch {
        Arch::X86_64 => Some(CallingConv::X86_64Win64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edda_target::{AbiVariant, Os, TargetTriple};

    fn t(arch: Arch, os: Os, abi: AbiVariant) -> TargetTriple {
        TargetTriple::new(arch, os, abi)
    }

    #[test]
    fn default_is_c_on_every_arch_os_pair() {
        // The platform-default story is "LLVM target machine handles it"
        // for every v0.1 target. If a future target needs a non-C
        // default (unlikely but possible), this test pins the change.
        let triples = [
            t(Arch::X86_64, Os::Linux, AbiVariant::Gnu),
            t(Arch::X86_64, Os::Windows, AbiVariant::Msvc),
            t(Arch::X86_64, Os::Macos, AbiVariant::Darwin),
            t(Arch::Aarch64, Os::Linux, AbiVariant::Gnu),
            t(Arch::Aarch64, Os::Macos, AbiVariant::Darwin),
            t(Arch::Aarch64, Os::Windows, AbiVariant::Msvc),
            t(Arch::Riscv64, Os::BareMetal, AbiVariant::None),
            t(Arch::Wasm32, Os::Wasi, AbiVariant::WasiPreview1),
            t(Arch::Wasm64, Os::Wasi, AbiVariant::WasiPreview1),
        ];
        for triple in triples {
            assert_eq!(
                default_for_target(&triple),
                CallingConv::C,
                "non-C default for {triple}",
            );
        }
    }

    #[test]
    fn llvm_ir_names_match_langref() {
        assert_eq!(CallingConv::C.llvm_ir_name(), "ccc");
        assert_eq!(CallingConv::X86_64SysV.llvm_ir_name(), "x86_64_sysvcc");
        assert_eq!(CallingConv::X86_64Win64.llvm_ir_name(), "win64cc");
        assert_eq!(CallingConv::Fast.llvm_ir_name(), "fastcc");
        assert_eq!(CallingConv::Cold.llvm_ir_name(), "coldcc");
    }

    #[test]
    fn llvm_ir_names_are_prefix_free() {
        // Lexer-level concern: text-IR emission writes the CC name
        // followed by a space; prefix collisions would mis-parse.
        let names = [
            CallingConv::C.llvm_ir_name(),
            CallingConv::X86_64SysV.llvm_ir_name(),
            CallingConv::X86_64Win64.llvm_ir_name(),
            CallingConv::Fast.llvm_ir_name(),
            CallingConv::Cold.llvm_ir_name(),
        ];
        for (i, a) in names.iter().enumerate() {
            for (j, b) in names.iter().enumerate() {
                if i == j {
                    continue;
                }
                assert!(
                    !b.starts_with(a),
                    "{b:?} starts with {a:?}: prefix collision",
                );
            }
        }
    }

    #[test]
    fn explicit_sysv_only_on_x86_64() {
        assert_eq!(explicit_x86_64_sysv(Arch::X86_64), Some(CallingConv::X86_64SysV));
        for other in [Arch::Aarch64, Arch::Riscv64, Arch::Wasm32, Arch::Wasm64] {
            assert_eq!(explicit_x86_64_sysv(other), None);
        }
    }

    #[test]
    fn explicit_win64_only_on_x86_64() {
        assert_eq!(explicit_x86_64_win64(Arch::X86_64), Some(CallingConv::X86_64Win64));
        for other in [Arch::Aarch64, Arch::Riscv64, Arch::Wasm32, Arch::Wasm64] {
            assert_eq!(explicit_x86_64_win64(other), None);
        }
    }

    #[test]
    fn display_matches_llvm_ir_name() {
        assert_eq!(format!("{}", CallingConv::C), "ccc");
        assert_eq!(format!("{}", CallingConv::X86_64Win64), "win64cc");
    }
}
