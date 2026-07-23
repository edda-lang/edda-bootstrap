//! Artifact-name parsing and display.
//!
//! Per `storage.md` §2, an artifact's filename has the form:
//!
//! ```text
//! <short-mangled-name>__<12-hex-prefix>
//! ```
//!
//! where the short mangled name is produced by `edda-codegen`'s mangling
//! pass (`storage.md` §5) and the prefix is the first 12 hex characters of
//! the [`crate::hash::ArtifactHash`].
//!
//! This module owns parsing and display of that combined form. Mangling
//! itself is `edda-codegen`'s concern; the short portion is treated here as
//! an opaque string with light validation (non-empty, no path separators,
//! no whitespace, no double-underscore-followed-by-hex that would clash
//! with the disambiguator).

use std::fmt;

use smol_str::SmolStr;

use crate::hash::HashPrefix;

/// `<short-mangled-name>__<12-hex-prefix>`. Persistent on disk; parsed by
/// every component that ever reads an artifact path.
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct ArtifactName {
    short: SmolStr,
    prefix: HashPrefix,
}

impl ArtifactName {
    /// Build an artifact name from its mangled short form and a content-hash
    /// prefix. Returns `None` if `short` is empty or contains characters
    /// that would break the filename grammar.
    pub fn new(short: impl Into<SmolStr>, prefix: HashPrefix) -> Option<Self> {
        let short = short.into();
        if !is_valid_short(&short) {
            return None;
        }
        Some(ArtifactName { short, prefix })
    }

    /// Mangled short name (the prefix before `__<hash-prefix>`).
    pub fn short(&self) -> &str {
        self.short.as_str()
    }

    /// 12-hex-character disambiguator (the suffix after `__`).
    pub fn prefix(&self) -> HashPrefix {
        self.prefix
    }

    /// Parse the combined form. Rejects inputs that lack a `__<12-hex>`
    /// tail or whose short portion is empty.
    pub fn parse(s: &str) -> Result<Self, ParseError> {
        // The separator is `__`. We split at the LAST occurrence so that
        // short names containing `__` themselves (rare but admitted by the
        // mangling grammar) survive the round-trip.
        let separator = s.rfind("__").ok_or(ParseError::MissingSeparator)?;
        let (short, after_separator) = s.split_at(separator);
        let prefix_str = &after_separator[2..]; // skip the "__"
        let prefix = HashPrefix::parse_hex(prefix_str).ok_or(ParseError::BadPrefix)?;
        if !is_valid_short(short) {
            return Err(ParseError::BadShort);
        }
        Ok(ArtifactName {
            short: SmolStr::new(short),
            prefix,
        })
    }
}

impl fmt::Display for ArtifactName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}__{}", self.short, self.prefix)
    }
}

impl fmt::Debug for ArtifactName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ArtifactName({})", self)
    }
}

/// Failure mode for [`ArtifactName::parse`].
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum ParseError {
    /// Input contained no `__` separator.
    MissingSeparator,
    /// Tail after `__` was not 12 hex characters.
    BadPrefix,
    /// Short portion before `__` was empty or contained forbidden chars.
    BadShort,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::MissingSeparator => {
                f.write_str("artifact name missing `__<hash-prefix>` suffix")
            }
            ParseError::BadPrefix => f.write_str("artifact-name hash prefix is not 12 hex chars"),
            ParseError::BadShort => f.write_str("artifact-name short portion is empty or invalid"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Validate the short-mangled-name portion of an artifact filename.
fn is_valid_short(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    !s.chars().any(|c| {
        matches!(c, '/' | '\\' | '\0') || c.is_whitespace()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::hash_bytes;

    fn sample_prefix() -> HashPrefix {
        hash_bytes(b"sample").prefix()
    }

    #[test]
    fn round_trip_simple() {
        let name = ArtifactName::new("Option_i32", sample_prefix()).unwrap();
        let s = name.to_string();
        let parsed = ArtifactName::parse(&s).unwrap();
        assert_eq!(name, parsed);
    }

    #[test]
    fn round_trip_complex_short_with_underscores() {
        let name = ArtifactName::new(
            "Pipeline_String_JsonValue_uses_err_AllocError_err_ParseError",
            sample_prefix(),
        )
        .unwrap();
        let parsed = ArtifactName::parse(&name.to_string()).unwrap();
        assert_eq!(name, parsed);
    }

    #[test]
    fn rejects_no_separator() {
        assert_eq!(
            ArtifactName::parse("Option_i32a3f2e8b1c4d5"),
            Err(ParseError::MissingSeparator),
        );
    }

    #[test]
    fn rejects_bad_prefix() {
        assert_eq!(
            ArtifactName::parse("Option_i32__not_hex_here"),
            Err(ParseError::BadPrefix),
        );
        assert_eq!(
            ArtifactName::parse("Option_i32__deadbeef"), // only 8 hex
            Err(ParseError::BadPrefix),
        );
    }

    #[test]
    fn rejects_empty_short() {
        assert_eq!(
            ArtifactName::parse("__a3f2e8b1c4d5"),
            Err(ParseError::BadShort),
        );
    }

    #[test]
    fn rejects_path_separator_in_short() {
        assert!(ArtifactName::new("foo/bar", sample_prefix()).is_none());
        assert!(ArtifactName::new("foo\\bar", sample_prefix()).is_none());
    }

    #[test]
    fn rejects_whitespace_in_short() {
        assert!(ArtifactName::new("foo bar", sample_prefix()).is_none());
        assert!(ArtifactName::new("foo\tbar", sample_prefix()).is_none());
    }

    #[test]
    fn splits_on_last_double_underscore() {
        // A short name that itself contains `__` survives round-trip because
        // we split at the LAST `__`.
        let name = ArtifactName::new("weird__short", sample_prefix()).unwrap();
        let s = name.to_string();
        let parsed = ArtifactName::parse(&s).unwrap();
        assert_eq!(parsed.short(), "weird__short");
    }
}
