//! LLVM code-model selection per target.
//!
//! The code model bounds the displacement range the backend may assume
//! when generating addresses (call targets, global references). Picking
//! the wrong model produces either link-time errors (`relocation
//! truncated to fit`) on too-small or sub-optimal code on too-large.
//!
//! Edda v0.1 ships the LLVM-default code model on every target. The
//! enum and the `default_for_target` selector exist so future profiles
//! (e.g., a Linux-kernel build that needs `Kernel`, or a freestanding
//! firmware build that needs `Tiny`) can override the default at a
//! single point without sprinkling target switches across the backend.

use edda_target::{Arch, TargetTriple};

/// LLVM code model.
///
/// LangRef nomenclature; verified against LLVM 18's `LLVMCodeModel`
/// enum. When the inkwell binding lands, callers obtain the numeric
/// discriminant via inkwell's `CodeModel` rather than parsing the name.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum CodeModel {
    /// Code + static data must fit in the first 1MB of the address
    /// space. Used by ARM Cortex-M baremetal images.
    Tiny,
    /// LLVM's default on the x86_64 and aarch64 user-space targets.
    /// Code + static data fit within ±2GB of each other.
    Small,
    /// Linux/Windows kernel mode: code + data live in the high half of
    /// the canonical address space.
    Kernel,
    /// Code in the small model, data in the large model. Used by
    /// large-static-data applications that still fit code within 2GB.
    Medium,
    /// No assumptions about displacement range; every reference is
    /// full-width. Used by `-mcmodel=large` builds and most JITs.
    Large,
}

impl CodeModel {
    /// LLVM `-mcmodel=...` spelling (also the LangRef name).
    pub const fn cli_name(self) -> &'static str {
        match self {
            CodeModel::Tiny => "tiny",
            CodeModel::Small => "small",
            CodeModel::Kernel => "kernel",
            CodeModel::Medium => "medium",
            CodeModel::Large => "large",
        }
    }
}

impl std::fmt::Display for CodeModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.cli_name())
    }
}

#[cfg(feature = "llvm")]
impl CodeModel {
    /// Map this [`CodeModel`] onto inkwell's `CodeModel`.
    ///
    /// Inkwell 0.5 has no `Tiny` variant — Edda's [`CodeModel::Tiny`]
    /// falls back to `Default`. Tiny is ARM Cortex-M baremetal; v0.1
    /// doesn't ship that profile so the fallback is unreachable from
    /// manifest / CLI paths.
    pub(crate) const fn to_inkwell(self) -> inkwell::targets::CodeModel {
        use inkwell::targets::CodeModel as InkwellCodeModel;
        match self {
            CodeModel::Tiny => InkwellCodeModel::Default,
            CodeModel::Small => InkwellCodeModel::Small,
            CodeModel::Kernel => InkwellCodeModel::Kernel,
            CodeModel::Medium => InkwellCodeModel::Medium,
            CodeModel::Large => InkwellCodeModel::Large,
        }
    }
}

/// LLVM-default code model for `triple`.
///
/// All v0.1 targets default to [`CodeModel::Small`]:
///
/// - **x86_64**, **aarch64**, **riscv64** on user-space OSes — Small is
///   LLVM's default and the right choice for user binaries up to 2GB.
/// - **bare-metal** — Small still applies; firmware that needs `Tiny`
///   (ARM Cortex-M) or `Kernel` (Linux kernel modules) opts in via a
///   future profile setting, not via the triple.
/// - **wasm32**, **wasm64** — code model is meaningless for Wasm; LLVM
///   accepts the value but ignores it. Returning `Small` keeps the
///   function total without branching.
pub const fn default_for_target(triple: &TargetTriple) -> CodeModel {
    let _ = triple;
    CodeModel::Small
}

/// Whether `model` is admissible on `arch`. Used by manifest /
/// CLI validation when a future profile asks for a non-default model.
///
/// Mapping is conservative: only the (arch, model) pairs LLVM 18
/// documents as supported return `true`. Unknown pairings return
/// `false` so the validator can produce a clean error rather than
/// silently passing through a setting that LLVM will later reject at
/// `TargetMachine` construction.
#[allow(dead_code)] // manifest / CLI override path not yet wired; retained for the locked table
pub(crate) const fn is_admissible(arch: Arch, model: CodeModel) -> bool {
    match (arch, model) {
        // x86_64 supports Small / Kernel / Medium / Large; Tiny is ARM-only.
        (Arch::X86_64, CodeModel::Tiny) => false,
        (Arch::X86_64, _) => true,
        // aarch64 supports Tiny / Small / Large; Kernel and Medium are
        // not in LLVM 18's aarch64 backend.
        (Arch::Aarch64, CodeModel::Tiny | CodeModel::Small | CodeModel::Large) => true,
        (Arch::Aarch64, _) => false,
        // riscv64 supports Small / Medium only (LLVM `medlow` / `medany`).
        (Arch::Riscv64, CodeModel::Small | CodeModel::Medium) => true,
        (Arch::Riscv64, _) => false,
        // Wasm: code model is a no-op; accept everything for forward
        // compatibility but the value has no effect.
        (Arch::Wasm32 | Arch::Wasm64, _) => true,
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
    fn every_v0_1_target_defaults_to_small() {
        let triples = [
            t(Arch::X86_64, Os::Linux, AbiVariant::Gnu),
            t(Arch::X86_64, Os::Windows, AbiVariant::Msvc),
            t(Arch::Aarch64, Os::Macos, AbiVariant::Darwin),
            t(Arch::Aarch64, Os::Linux, AbiVariant::Gnu),
            t(Arch::Riscv64, Os::BareMetal, AbiVariant::None),
            t(Arch::Wasm32, Os::Wasi, AbiVariant::WasiPreview1),
        ];
        for triple in triples {
            assert_eq!(
                default_for_target(&triple),
                CodeModel::Small,
                "non-Small default for {triple}",
            );
        }
    }

    #[test]
    fn cli_names_match_mcmodel() {
        assert_eq!(CodeModel::Tiny.cli_name(), "tiny");
        assert_eq!(CodeModel::Small.cli_name(), "small");
        assert_eq!(CodeModel::Kernel.cli_name(), "kernel");
        assert_eq!(CodeModel::Medium.cli_name(), "medium");
        assert_eq!(CodeModel::Large.cli_name(), "large");
    }

    #[test]
    fn x86_64_rejects_tiny_accepts_others() {
        assert!(!is_admissible(Arch::X86_64, CodeModel::Tiny));
        for m in [CodeModel::Small, CodeModel::Kernel, CodeModel::Medium, CodeModel::Large] {
            assert!(is_admissible(Arch::X86_64, m), "{m:?}");
        }
    }

    #[test]
    fn aarch64_admits_tiny_small_large_only() {
        for m in [CodeModel::Tiny, CodeModel::Small, CodeModel::Large] {
            assert!(is_admissible(Arch::Aarch64, m), "{m:?}");
        }
        for m in [CodeModel::Kernel, CodeModel::Medium] {
            assert!(!is_admissible(Arch::Aarch64, m), "{m:?}");
        }
    }

    #[test]
    fn riscv64_admits_small_medium_only() {
        for m in [CodeModel::Small, CodeModel::Medium] {
            assert!(is_admissible(Arch::Riscv64, m), "{m:?}");
        }
        for m in [CodeModel::Tiny, CodeModel::Kernel, CodeModel::Large] {
            assert!(!is_admissible(Arch::Riscv64, m), "{m:?}");
        }
    }

    #[test]
    fn wasm_admits_every_model() {
        for arch in [Arch::Wasm32, Arch::Wasm64] {
            for m in [CodeModel::Tiny, CodeModel::Small, CodeModel::Kernel, CodeModel::Medium, CodeModel::Large] {
                assert!(is_admissible(arch, m), "{arch:?} / {m:?}");
            }
        }
    }

    #[test]
    fn display_matches_cli_name() {
        assert_eq!(format!("{}", CodeModel::Small), "small");
        assert_eq!(format!("{}", CodeModel::Kernel), "kernel");
    }
}
