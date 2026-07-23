//! Repo / cache tier classification and the manifest's `default_tier`
//! policy.
//!
//! Per `storage.md` §1, every artifact lives in exactly one of two tiers:
//!
//! - **`Repo`** — `<project>/codegen/`, version-controlled, ships with the
//!   project.
//! - **`Cache`** — `<project>/.edda/cache/codegen/`, gitignored, build
//!   state.
//!
//! Tier assignment follows the chain-origin rule per `storage.md` §1 +
//! `build-system.md` §6 (2026-05-11). The `package.toon`
//! `codegen.default_tier` field has the two values [`TierPolicy::Auto`] and
//! [`TierPolicy::Cache`]; the earlier `repo` value was retracted.
//!
//! This module does not compute the chain origin — that's the driver's
//! call. It owns the value types and their TOON-name table.

use std::fmt;

/// On-disk tier of a generated artifact.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Tier {
    /// `<project>/codegen/<spec-qualified-path>/<name>.ea`.
    Repo,
    /// `<project>/.edda/cache/codegen/<2-byte-shard>/<name>.ea`.
    Cache,
}

impl Tier {
    /// Lowercase name used in the manifest's `tier:` field.
    pub const fn name(self) -> &'static str {
        match self {
            Tier::Repo => "repo",
            Tier::Cache => "cache",
        }
    }

    /// Parse a tier name from `lowercase_snake_case`.
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "repo" => Some(Tier::Repo),
            "cache" => Some(Tier::Cache),
            _ => None,
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// `package.toon`'s `codegen.default_tier` policy.
///
/// - [`Auto`](Self::Auto) — apply the chain-origin rule per `storage.md` §1.
/// - [`Cache`](Self::Cache) — force every new artifact into the cache tier;
///   the user must `codegen.promote` to place an artifact in the repo.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug, Default)]
pub enum TierPolicy {
    /// Chain-origin rule decides per artifact (the default).
    #[default]
    Auto,
    /// Force every artifact into the cache tier.
    Cache,
}

impl TierPolicy {
    /// Lowercase name used in the manifest.
    pub const fn name(self) -> &'static str {
        match self {
            TierPolicy::Auto => "auto",
            TierPolicy::Cache => "cache",
        }
    }

    /// Parse a policy name from `package.toon`.
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(TierPolicy::Auto),
            "cache" => Some(TierPolicy::Cache),
            _ => None,
        }
    }
}

impl fmt::Display for TierPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_name_round_trip() {
        for &t in &[Tier::Repo, Tier::Cache] {
            assert_eq!(Tier::from_name(t.name()), Some(t));
        }
    }

    #[test]
    fn tier_unknown_name_is_none() {
        assert!(Tier::from_name("global").is_none());
        assert!(Tier::from_name("").is_none());
        assert!(Tier::from_name("REPO").is_none());
    }

    #[test]
    fn tier_policy_default_is_auto() {
        assert_eq!(TierPolicy::default(), TierPolicy::Auto);
    }

    #[test]
    fn tier_policy_rejects_retracted_repo() {
        // `default_tier: repo` was retracted; it must not parse.
        assert!(TierPolicy::from_name("repo").is_none());
    }

    #[test]
    fn tier_policy_round_trip() {
        for &p in &[TierPolicy::Auto, TierPolicy::Cache] {
            assert_eq!(TierPolicy::from_name(p.name()), Some(p));
        }
    }
}
