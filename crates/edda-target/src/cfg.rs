//! [`TargetCfg`] — the (triple, feature set) pair that grounds the
//! `target_has(feature)` comptime predicate.

use std::fmt;

use crate::feature::{FeatureSet, known_features};
use crate::triple::{Arch, Os, TargetTriple};

/// A build target: the locked triple plus the set of enabled features.
///
/// This is the value that `edda-comptime` evaluates
/// `target_has(feature)` against (build-system.md §9). It is also the
/// input to artifact-hashing (`storage.md` §7) — the triple and feature
/// set both participate in the BLAKE3 input per `abi-and-layout.md` §7.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct TargetCfg {
    triple: TargetTriple,
    features: FeatureSet,
}

impl TargetCfg {
    /// Construct a config with an empty feature set.
    pub fn new(triple: TargetTriple) -> Self {
        let features = FeatureSet::new(triple.arch());
        Self { triple, features }
    }

    /// Construct a config from a triple and a pre-built feature set.
    /// Fails if the feature set was built for a different arch than the
    /// triple — manifest parsing already enforces arch consistency, so
    /// this is a defensive boundary check.
    pub fn with_features(
        triple: TargetTriple,
        features: FeatureSet,
    ) -> Result<Self, ArchMismatch> {
        if features.arch() != triple.arch() {
            return Err(ArchMismatch {
                triple: triple.arch(),
                features: features.arch(),
            });
        }
        Ok(Self { triple, features })
    }

    /// Target triple.
    pub const fn triple(&self) -> &TargetTriple {
        &self.triple
    }

    /// Enabled feature set.
    pub const fn features(&self) -> &FeatureSet {
        &self.features
    }

    /// Ground truth for the `target_has(feature)` comptime built-in.
    ///
    /// Feature names span three flat namespaces, checked in order:
    /// - **OS family** — the triple-OS spec spellings (`windows`, `linux`,
    ///   `macos`, `freebsd`, `bare-metal`, `wasi`); `Yes` iff it names the
    ///   active target's OS. The runtime-in-Edda seam branches on these
    ///   (`comptime if target_has("windows")`).
    /// - **Arch family** — the triple-arch spec spellings (`x86-64`,
    ///   `aarch64`, `riscv64`, `wasm32`, `wasm64`); `Yes` iff it names the
    ///   active target's arch.
    /// - **Arch SIMD catalogue** — `known_features(arch)`; `Yes` if enabled,
    ///   `No` if known-but-absent.
    ///
    /// Returns:
    /// - [`TargetHas::Unknown`] — the name is in none of the three. Comptime
    ///   lowers this to an `unknown_target_feature` diagnostic
    ///   (build-system.md §8) without producing a bool.
    /// - [`TargetHas::Yes`] / [`TargetHas::No`] — see per-namespace rules above.
    pub fn target_has(&self, feature: &str) -> TargetHas {
        if let Some(os) = Os::from_name(feature) {
            return bool_to_has(self.triple.os() == os);
        }
        if let Some(a) = Arch::from_name(feature) {
            return bool_to_has(self.triple.arch() == a);
        }
        let arch = self.triple.arch();
        if !is_known_feature(arch, feature) {
            return TargetHas::Unknown;
        }
        bool_to_has(self.features.contains(feature))
    }
}

/// Linear scan against the locked catalogue. Catalogues are small (≤14)
/// and queries are not in a hot loop.
fn is_known_feature(arch: Arch, name: &str) -> bool {
    known_features(arch).iter().any(|&f| f == name)
}

/// Project a definite OS/arch-family answer onto the tri-state.
const fn bool_to_has(b: bool) -> TargetHas {
    if b {
        TargetHas::Yes
    } else {
        TargetHas::No
    }
}

/// Result of [`TargetCfg::target_has`]; tri-valued so the comptime
/// evaluator can distinguish "feature unknown for this arch" from "known
/// feature, not enabled in this target".
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum TargetHas {
    /// The feature name is not in the locked catalogue for this arch;
    /// caller emits `unknown_target_feature`.
    Unknown,
    /// The feature is enabled in this target.
    Yes,
    /// The feature is known but not enabled.
    No,
}

impl TargetHas {
    /// `true` if the result is `Yes`. `Unknown` and `No` both project to
    /// `false` — callers that care about the unknown case must match the
    /// variant directly before calling this.
    pub const fn is_yes(self) -> bool {
        matches!(self, TargetHas::Yes)
    }
}

/// Defensive error from [`TargetCfg::with_features`].
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct ArchMismatch {
    /// Arch named by the triple.
    pub triple: Arch,
    /// Arch the feature set was built for.
    pub features: Arch,
}

impl fmt::Display for ArchMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "feature set arch {} does not match triple arch {}",
            self.features, self.triple
        )
    }
}

impl std::error::Error for ArchMismatch {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triple::{AbiVariant, Os};

    fn x86_64_linux() -> TargetTriple {
        TargetTriple::new(Arch::X86_64, Os::Linux, AbiVariant::Gnu)
    }

    fn aarch64_linux() -> TargetTriple {
        TargetTriple::new(Arch::Aarch64, Os::Linux, AbiVariant::Gnu)
    }

    #[test]
    fn new_starts_with_matching_empty_feature_set() {
        let cfg = TargetCfg::new(x86_64_linux());
        assert_eq!(cfg.triple().arch(), Arch::X86_64);
        assert_eq!(cfg.features().arch(), Arch::X86_64);
        assert!(cfg.features().is_empty());
    }

    #[test]
    fn with_features_accepts_matching_arch() {
        let mut features = FeatureSet::new(Arch::X86_64);
        features.insert("avx2").unwrap();
        let cfg = TargetCfg::with_features(x86_64_linux(), features).unwrap();
        assert_eq!(cfg.features().len(), 1);
    }

    #[test]
    fn with_features_rejects_mismatched_arch() {
        let aarch64_features = FeatureSet::new(Arch::Aarch64);
        let err = TargetCfg::with_features(x86_64_linux(), aarch64_features).unwrap_err();
        assert_eq!(err.triple, Arch::X86_64);
        assert_eq!(err.features, Arch::Aarch64);
    }

    #[test]
    fn target_has_yes_for_enabled_feature() {
        let mut features = FeatureSet::new(Arch::X86_64);
        features.insert("avx2").unwrap();
        let cfg = TargetCfg::with_features(x86_64_linux(), features).unwrap();
        assert_eq!(cfg.target_has("avx2"), TargetHas::Yes);
    }

    #[test]
    fn target_has_no_for_known_but_disabled_feature() {
        let cfg = TargetCfg::new(x86_64_linux());
        // `sse4.2` is in the x86-64 catalogue but the empty feature set
        // does not enable it.
        assert_eq!(cfg.target_has("sse4.2"), TargetHas::No);
    }

    #[test]
    fn target_has_unknown_for_off_arch_feature() {
        let cfg = TargetCfg::new(x86_64_linux());
        // `neon` is aarch64-only; querying it on x86_64 is `unknown`,
        // not `false`, so the caller can emit `unknown_target_feature`.
        assert_eq!(cfg.target_has("neon"), TargetHas::Unknown);
    }

    #[test]
    fn target_has_unknown_for_typo() {
        let cfg = TargetCfg::new(aarch64_linux());
        assert_eq!(cfg.target_has("nean"), TargetHas::Unknown);
    }

    #[test]
    fn is_yes_projection() {
        assert!(TargetHas::Yes.is_yes());
        assert!(!TargetHas::No.is_yes());
        assert!(!TargetHas::Unknown.is_yes());
    }

    #[test]
    fn arch_mismatch_displays_both_arches() {
        let err = ArchMismatch {
            triple: Arch::X86_64,
            features: Arch::Aarch64,
        };
        let s = format!("{err}");
        assert!(s.contains("x86-64"));
        assert!(s.contains("aarch64"));
    }
}
