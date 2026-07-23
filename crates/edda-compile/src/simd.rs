//! SIMD-width gating against a target's feature set.
//!
//! Per build-system.md §9, vector-width SIMD operations are valid only
//! when the active target's feature set enables the canonical feature for
//! that width on that arch. This module owns the (arch, width) → feature
//! mapping used by typecheck / lowering when it encounters a SIMD
//! intrinsic.

use edda_target::{Arch, TargetCfg};

use crate::error::SimdRejection;

/// Canonical target feature required to use SIMD operations of
/// `width_bits` on `arch`. Returns `None` when the width is not
/// representable on the arch's v0.1 SIMD ISA (e.g., 256-bit on aarch64,
/// any SIMD on riscv64 or wasm64).
///
/// Mapping (follows `docs/bootstrap/backend-choice.md` §6.6 to the extent
/// the descriptions in `edda-target` reproduce it; verify when the doc
/// lands in `corpus/`):
///
/// - x86-64:  128 → `sse2`, 256 → `avx2`, 512 → `avx512f`
/// - aarch64: 128 → `neon`  (SVE is variable-width — see Notes)
/// - wasm32:  128 → `simd128`
/// - riscv64, wasm64: no v0.1 SIMD ISA
///
/// # Notes
///
/// **SVE on aarch64.** SVE (Scalable Vector Extension) intentionally has
/// no entry above. Its width is determined at runtime by the hardware
/// vector length, so a fixed-bits query does not apply. The hypothetical
/// `sve` intrinsic surface lives on a separate path this crate does not
/// yet model. When `docs/bootstrap/backend-choice.md` §6.6 lands in
/// `corpus/`, this table may grow a scalable shape rather than a fixed
/// bits query.
///
/// **Baseline-feature semantics on x86-64.** `sse2` is part of the
/// architectural baseline for x86-64 CPUs, so requiring it through the
/// feature set may be redundant. The decision (always-on vs.
/// declared-in-manifest) is spec-dependent — confirm against
/// `docs/bootstrap/backend-choice.md` §6.6 once the doc is in `corpus/`.
pub const fn required_feature(arch: Arch, width_bits: u32) -> Option<&'static str> {
    match (arch, width_bits) {
        (Arch::X86_64, 128) => Some("sse2"),
        (Arch::X86_64, 256) => Some("avx2"),
        (Arch::X86_64, 512) => Some("avx512f"),
        (Arch::Aarch64, 128) => Some("neon"),
        (Arch::Wasm32, 128) => Some("simd128"),
        _ => None,
    }
}

/// Validate that `width_bits`-wide SIMD operations are usable on `cfg`.
///
/// Returns:
///
/// - `Ok(())` when the active target's feature set enables the canonical
///   feature for the (arch, width) pair.
/// - `Err(SimdRejection::UnsupportedWidth)` when no feature in the v0.1
///   SIMD ISA represents that width on that arch.
/// - `Err(SimdRejection::MissingFeature { feature })` when the feature
///   exists in the locked catalogue but is not enabled in `cfg`.
///
/// Callers attach a source span and surface the result through
/// [`crate::CompileError::SimdUnsupported`] for rendering.
pub fn simd_width_supported(width_bits: u32, cfg: &TargetCfg) -> Result<(), SimdRejection> {
    let arch = cfg.triple().arch();
    let Some(feature) = required_feature(arch, width_bits) else {
        return Err(SimdRejection::UnsupportedWidth);
    };
    if cfg.features().contains(feature) {
        Ok(())
    } else {
        Err(SimdRejection::MissingFeature { feature })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use edda_target::{AbiVariant, FeatureSet, Os, TargetTriple};

    fn cfg(arch: Arch, features: &[&str]) -> TargetCfg {
        let (os, abi) = match arch {
            Arch::X86_64 | Arch::Aarch64 | Arch::Riscv64 => (Os::Linux, AbiVariant::Gnu),
            Arch::Wasm32 | Arch::Wasm64 => (Os::Wasi, AbiVariant::WasiPreview1),
        };
        let triple = TargetTriple::new(arch, os, abi);
        let mut fs = FeatureSet::new(arch);
        for name in features {
            fs.insert(name).unwrap();
        }
        TargetCfg::with_features(triple, fs).unwrap()
    }

    #[test]
    fn x86_64_widths_map_to_features() {
        assert_eq!(required_feature(Arch::X86_64, 128), Some("sse2"));
        assert_eq!(required_feature(Arch::X86_64, 256), Some("avx2"));
        assert_eq!(required_feature(Arch::X86_64, 512), Some("avx512f"));
    }

    #[test]
    fn aarch64_128_is_neon_and_256_is_unmapped() {
        assert_eq!(required_feature(Arch::Aarch64, 128), Some("neon"));
        assert_eq!(required_feature(Arch::Aarch64, 256), None);
    }

    #[test]
    fn wasm32_128_is_simd128() {
        assert_eq!(required_feature(Arch::Wasm32, 128), Some("simd128"));
    }

    #[test]
    fn riscv64_and_wasm64_have_no_simd_at_v0_1() {
        for arch in [Arch::Riscv64, Arch::Wasm64] {
            for width in [64, 128, 256, 512] {
                assert_eq!(
                    required_feature(arch, width),
                    None,
                    "{arch:?} @ {width}",
                );
            }
        }
    }

    #[test]
    fn unsupported_width_when_arch_has_no_simd() {
        let c = cfg(Arch::Riscv64, &[]);
        assert_eq!(
            simd_width_supported(128, &c),
            Err(SimdRejection::UnsupportedWidth),
        );
    }

    #[test]
    fn missing_feature_names_canonical_feature() {
        let c = cfg(Arch::X86_64, &[]);
        assert_eq!(
            simd_width_supported(256, &c),
            Err(SimdRejection::MissingFeature { feature: "avx2" }),
        );
    }

    #[test]
    fn enabled_feature_accepts_width() {
        let c = cfg(Arch::X86_64, &["avx2"]);
        assert_eq!(simd_width_supported(256, &c), Ok(()));
    }

    #[test]
    fn higher_width_does_not_imply_lower() {
        // 256-bit enabled (avx2) does not auto-enable 128-bit (sse2). The
        // manifest can declare both in `default_features` if needed; the
        // gate is per-feature.
        let c = cfg(Arch::X86_64, &["avx2"]);
        assert_eq!(
            simd_width_supported(128, &c),
            Err(SimdRejection::MissingFeature { feature: "sse2" }),
        );
    }

    #[test]
    fn off_catalogue_width_returns_unsupported_width() {
        let c = cfg(Arch::X86_64, &["avx2"]);
        assert_eq!(
            simd_width_supported(1024, &c),
            Err(SimdRejection::UnsupportedWidth),
        );
    }

    #[test]
    fn wasm32_simd128_accept_and_reject() {
        let off = cfg(Arch::Wasm32, &[]);
        assert_eq!(
            simd_width_supported(128, &off),
            Err(SimdRejection::MissingFeature { feature: "simd128" }),
        );
        let on = cfg(Arch::Wasm32, &["simd128"]);
        assert_eq!(simd_width_supported(128, &on), Ok(()));
    }
}
