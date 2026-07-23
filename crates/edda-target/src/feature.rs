//! Per-arch target-feature catalogue and validated feature sets.
//!
//! Feature names are spec-cited at build-system.md §9 (`sse4.2`, `avx2`,
//! `avx512f`, `neon`, `sve`, `bmi2`, `aes`, `crc32`) and pin down which
//! `target_has(feature)` queries are well-formed for a given architecture.

use std::fmt;

use crate::triple::Arch;

// Per-arch feature catalogues. Each slice is the spec-locked, alphabetically
// sorted set of features admissible at v0.1 for the corresponding arch.
//
// Sources:
//   * build-system.md §9 (examples of valid feature names per arch)
//   * backend-choice.md §6.6 (v0.1 SIMD intrinsic catalogue)
//
// Sorted alphabetically so a future migration to binary search is mechanical;
// for the current sizes (≤14 entries) a linear scan suffices.

const X86_64_FEATURES: &[&str] = &[
    "aes",
    "avx",
    "avx2",
    "avx512f",
    "bmi2",
    "crc32",
    "fma",
    "popcnt",
    "sse",
    "sse2",
    "sse3",
    "sse4.1",
    "sse4.2",
    "ssse3",
];

const AARCH64_FEATURES: &[&str] = &["aes", "crc32", "neon", "sve"];

const WASM32_FEATURES: &[&str] = &["simd128"];

const WASM64_FEATURES: &[&str] = &[];

const RISCV64_FEATURES: &[&str] = &[];

/// Locked feature catalogue for `arch`.
///
/// Every name in the returned slice is a valid argument to
/// `target_has(feature)` (build-system.md §9) on a build whose triple has
/// the given `arch`. Names outside the slice surface as the
/// `unknown_target_feature` diagnostic class.
pub const fn known_features(arch: Arch) -> &'static [&'static str] {
    match arch {
        Arch::X86_64 => X86_64_FEATURES,
        Arch::Aarch64 => AARCH64_FEATURES,
        Arch::Wasm32 => WASM32_FEATURES,
        Arch::Wasm64 => WASM64_FEATURES,
        Arch::Riscv64 => RISCV64_FEATURES,
    }
}

/// `true` if `name` is in the locked feature catalogue for `arch`.
fn is_known(arch: Arch, name: &str) -> bool {
    known_features(arch).iter().any(|&f| f == name)
}

/// Mutable set of target features, all valid for one fixed [`Arch`].
///
/// A [`FeatureSet`] is the canonical input to [`crate::TargetCfg`] and the
/// manifest parser's `default_features` list. Insert rejects unknown
/// (arch, name) pairings so callers can surface `unknown_target_feature`
/// at the manifest parse boundary instead of carrying the failure deeper.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct FeatureSet {
    arch: Arch,
    names: Vec<Box<str>>,
}

impl FeatureSet {
    /// Construct an empty feature set for `arch`.
    pub fn new(arch: Arch) -> Self {
        Self {
            arch,
            names: Vec::new(),
        }
    }

    /// Architecture the set's feature names must be valid for.
    pub const fn arch(&self) -> Arch {
        self.arch
    }

    /// Insert `name`. Returns `Ok(true)` if newly added, `Ok(false)` if it
    /// was already present, and `Err` if the name is not in the locked
    /// catalogue for the set's arch.
    pub fn insert(&mut self, name: &str) -> Result<bool, UnknownFeature> {
        if !is_known(self.arch, name) {
            return Err(UnknownFeature {
                arch: self.arch,
                name: name.to_owned(),
            });
        }
        match self.names.binary_search_by(|existing| (**existing).cmp(name)) {
            Ok(_) => Ok(false),
            Err(idx) => {
                self.names.insert(idx, name.into());
                Ok(true)
            }
        }
    }

    /// `true` if `name` is in the set. Does not validate against the
    /// catalogue — call after [`Self::insert`] has done so, or use
    /// [`crate::TargetCfg::target_has`] for the tri-valued query.
    pub fn contains(&self, name: &str) -> bool {
        self.names
            .binary_search_by(|existing| (**existing).cmp(name))
            .is_ok()
    }

    /// Iterate over feature names in sorted order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(|n| n.as_ref())
    }

    /// Number of features in the set.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// `true` if the set is empty.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

/// Surface error for an (arch, name) pairing rejected by
/// [`FeatureSet::insert`]. Caller maps to `unknown_target_feature`
/// (build-system.md §8) at the manifest parse / CLI boundary.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct UnknownFeature {
    /// Architecture against which validation failed.
    pub arch: Arch,
    /// The rejected feature name.
    pub name: String,
}

impl fmt::Display for UnknownFeature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "feature {:?} is not in the {} feature catalogue",
            self.name, self.arch
        )
    }
}

impl std::error::Error for UnknownFeature {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_is_sorted_with_no_duplicates() {
        for arch in Arch::ALL {
            let cat = known_features(arch);
            for win in cat.windows(2) {
                assert!(
                    win[0] < win[1],
                    "{:?} catalogue not strictly sorted: {:?} vs {:?}",
                    arch,
                    win[0],
                    win[1]
                );
            }
        }
    }

    #[test]
    fn catalogue_names_are_lowercase_or_digit_or_punct() {
        for arch in Arch::ALL {
            for name in known_features(arch) {
                assert!(
                    name.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.'),
                    "feature name {name:?} violates lowercase_snake_case-with-dot grammar",
                );
            }
        }
    }

    #[test]
    fn catalogue_covers_spec_examples() {
        // build-system.md §9 names every example as valid for its arch.
        assert!(known_features(Arch::X86_64).contains(&"sse4.2"));
        assert!(known_features(Arch::X86_64).contains(&"avx2"));
        assert!(known_features(Arch::X86_64).contains(&"avx512f"));
        assert!(known_features(Arch::X86_64).contains(&"bmi2"));
        assert!(known_features(Arch::X86_64).contains(&"aes"));
        assert!(known_features(Arch::X86_64).contains(&"crc32"));
        assert!(known_features(Arch::Aarch64).contains(&"neon"));
        assert!(known_features(Arch::Aarch64).contains(&"sve"));
        assert!(known_features(Arch::Wasm32).contains(&"simd128"));
    }

    #[test]
    fn arch_isolation() {
        // Cross-arch feature names are rejected.
        assert!(!is_known(Arch::Aarch64, "avx2"));
        assert!(!is_known(Arch::X86_64, "neon"));
        assert!(!is_known(Arch::Wasm32, "sse4.2"));
    }

    #[test]
    fn insert_rejects_unknown_for_arch() {
        let mut set = FeatureSet::new(Arch::Aarch64);
        let err = set.insert("avx2").unwrap_err();
        assert_eq!(err.arch, Arch::Aarch64);
        assert_eq!(err.name, "avx2");
        assert!(set.is_empty());
    }

    #[test]
    fn insert_dedups_and_sorts() {
        let mut set = FeatureSet::new(Arch::X86_64);
        assert_eq!(set.insert("avx2").unwrap(), true);
        assert_eq!(set.insert("avx2").unwrap(), false);
        assert_eq!(set.insert("sse4.2").unwrap(), true);
        assert_eq!(set.insert("aes").unwrap(), true);
        let names: Vec<_> = set.iter().collect();
        assert_eq!(names, ["aes", "avx2", "sse4.2"]);
        assert!(set.contains("avx2"));
        assert!(!set.contains("avx512f"));
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn empty_set_has_arch() {
        let set = FeatureSet::new(Arch::Wasm32);
        assert_eq!(set.arch(), Arch::Wasm32);
        assert!(set.is_empty());
        assert_eq!(set.len(), 0);
    }

    #[test]
    fn wasm64_and_riscv64_are_empty_at_v0_1() {
        // backend-choice.md §6.6: wasm64 has no v0.1 SIMD ISA;
        // riscv64.v is reserved for v1.0.
        assert!(known_features(Arch::Wasm64).is_empty());
        assert!(known_features(Arch::Riscv64).is_empty());
    }

    #[test]
    fn unknown_feature_displays_with_arch() {
        let err = UnknownFeature {
            arch: Arch::X86_64,
            name: "neon".to_owned(),
        };
        let rendered = format!("{err}");
        assert!(rendered.contains("\"neon\""));
        assert!(rendered.contains("x86-64"));
    }
}
