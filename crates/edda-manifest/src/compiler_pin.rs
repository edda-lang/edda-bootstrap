//! Rune `manifest.toml` compiler-version pin (§7.3 of
//! `corpus/edda-codex/language/08-packages.md`).
//!
//! Every `.rune` records the compiler that built it in its `manifest.toml`
//! as a top-level `compiler = "edda <major>.<minor>"` field. The patch level
//! is deliberately excluded: `rune_hash = f(source × compiler)`, so the same
//! source under a different *major.minor* produces a different archive, while
//! patch-level byte-reproducibility is enforced by the self-hosting suite. A
//! consumer rejects a rune whose pin disagrees with its own compiler at link
//! time (see the `edda add` verify chain).
//!
//! This module is the canonical reader/writer for that single field; the full
//! `package.toml` schema lives in [`crate::schema`].

use std::fmt;

/// A compiler-version pin: the `major.minor` recorded in (or to be written
/// into) a rune `manifest.toml` `compiler` field.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CompilerPin {
    pub major: u64,
    pub minor: u64,
}

/// Failure modes when reading a rune `manifest.toml` `compiler` field.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CompilerPinError {
    /// The `manifest.toml` bytes were not valid UTF-8 or not valid TOML.
    Malformed(String),
    /// A `compiler` key is present but its value is not a TOML string.
    NotAString,
    /// The value string is not of the form `edda <major>.<minor>`.
    BadFormat(String),
}

impl fmt::Display for CompilerPinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompilerPinError::Malformed(m) => write!(f, "malformed manifest.toml: {}", m),
            CompilerPinError::NotAString => {
                write!(f, "`compiler` field is not a string")
            }
            CompilerPinError::BadFormat(v) => write!(
                f,
                "`compiler` value `{}` is not of the form `edda <major>.<minor>`",
                v
            ),
        }
    }
}

impl std::error::Error for CompilerPinError {}

impl CompilerPin {
    /// Build a pin from the compiler's own SemVer string (e.g.
    /// `env!("CARGO_PKG_VERSION")`). Returns `None` when the leading
    /// `major.minor` cannot be parsed.
    pub fn from_compiler_version(version: &str) -> Option<CompilerPin> {
        let mut parts = version.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor_raw = parts.next()?;
        // Stop the minor segment at the first non-digit (e.g. `1.2-rc1`).
        let minor_digits: String = minor_raw.chars().take_while(|c| c.is_ascii_digit()).collect();
        let minor = minor_digits.parse().ok()?;
        Some(CompilerPin { major, minor })
    }

    /// Parse a `compiler` field value, e.g. `"edda 1.0"`.
    pub fn parse_field_value(value: &str) -> Result<CompilerPin, CompilerPinError> {
        let rest = value
            .strip_prefix("edda ")
            .ok_or_else(|| CompilerPinError::BadFormat(value.to_owned()))?;
        let (major_str, minor_str) = rest
            .split_once('.')
            .ok_or_else(|| CompilerPinError::BadFormat(value.to_owned()))?;
        let major = major_str
            .parse()
            .map_err(|_| CompilerPinError::BadFormat(value.to_owned()))?;
        let minor = minor_str
            .parse()
            .map_err(|_| CompilerPinError::BadFormat(value.to_owned()))?;
        Ok(CompilerPin { major, minor })
    }

    /// Render the canonical `compiler` field value, e.g. `edda 1.0`.
    pub fn to_field_value(self) -> String {
        format!("edda {}.{}", self.major, self.minor)
    }
}

impl fmt::Display for CompilerPin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "edda {}.{}", self.major, self.minor)
    }
}

/// Read the `compiler` pin from rune `manifest.toml` bytes.
///
/// - `Ok(Some(pin))` — a well-formed `compiler = "edda <major>.<minor>"`.
/// - `Ok(None)` — the manifest parses but carries no `compiler` key.
/// - `Err(_)` — the bytes are not valid TOML, or `compiler` is malformed.
pub fn read_compiler_pin(manifest_toml: &[u8]) -> Result<Option<CompilerPin>, CompilerPinError> {
    let text = std::str::from_utf8(manifest_toml)
        .map_err(|e| CompilerPinError::Malformed(e.to_string()))?;
    let table: toml::Table = text
        .parse()
        .map_err(|e: toml::de::Error| CompilerPinError::Malformed(e.to_string()))?;
    match table.get("compiler") {
        None => Ok(None),
        Some(toml::Value::String(s)) => CompilerPin::parse_field_value(s).map(Some),
        Some(_) => Err(CompilerPinError::NotAString),
    }
}

/// Inject a top-level `compiler = "edda <major>.<minor>"` line ahead of the
/// supplied `package.toml` bytes, producing the rune `manifest.toml` bytes.
///
/// A `package.toml` always opens with the `[package]` table, so a leading
/// top-level key is valid TOML and leaves the original document verbatim
/// after it.
pub fn inject_compiler_pin(package_toml: &[u8], pin: CompilerPin) -> Vec<u8> {
    let mut out = format!("compiler = \"{}\"\n\n", pin.to_field_value()).into_bytes();
    out.extend_from_slice(package_toml);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_compiler_version_drops_patch() {
        assert_eq!(
            CompilerPin::from_compiler_version("1.0.7"),
            Some(CompilerPin { major: 1, minor: 0 })
        );
        assert_eq!(
            CompilerPin::from_compiler_version("0.0.0"),
            Some(CompilerPin { major: 0, minor: 0 })
        );
        assert_eq!(
            CompilerPin::from_compiler_version("12.34.56"),
            Some(CompilerPin { major: 12, minor: 34 })
        );
    }

    #[test]
    fn from_compiler_version_tolerates_pre_release_minor() {
        assert_eq!(
            CompilerPin::from_compiler_version("2.5-rc1"),
            Some(CompilerPin { major: 2, minor: 5 })
        );
    }

    #[test]
    fn field_value_round_trips() {
        let pin = CompilerPin { major: 1, minor: 0 };
        assert_eq!(pin.to_field_value(), "edda 1.0");
        assert_eq!(CompilerPin::parse_field_value("edda 1.0"), Ok(pin));
    }

    #[test]
    fn parse_field_value_rejects_bad_shapes() {
        assert!(matches!(
            CompilerPin::parse_field_value("rustc 1.0"),
            Err(CompilerPinError::BadFormat(_))
        ));
        assert!(matches!(
            CompilerPin::parse_field_value("edda 1"),
            Err(CompilerPinError::BadFormat(_))
        ));
        assert!(matches!(
            CompilerPin::parse_field_value("edda x.y"),
            Err(CompilerPinError::BadFormat(_))
        ));
    }

    #[test]
    fn read_pin_from_injected_manifest_round_trips() {
        let package = b"[package]\nname = \"regex\"\nversion = \"1.4.2\"\nroot_namespace = \"regex\"\n";
        let pin = CompilerPin { major: 1, minor: 0 };
        let manifest = inject_compiler_pin(package, pin);
        // The injected document is valid TOML and the pin reads back.
        assert_eq!(read_compiler_pin(&manifest), Ok(Some(pin)));
        // The original package.toml body survives verbatim after the pin line.
        let text = std::str::from_utf8(&manifest).unwrap();
        assert!(text.starts_with("compiler = \"edda 1.0\"\n\n[package]\n"));
    }

    #[test]
    fn read_pin_absent_is_none() {
        let manifest = b"[package]\nname = \"regex\"\nversion = \"1.4.2\"\n";
        assert_eq!(read_compiler_pin(manifest), Ok(None));
    }

    #[test]
    fn read_pin_non_string_rejected() {
        let manifest = b"compiler = 10\n[package]\nname = \"r\"\n";
        assert_eq!(read_compiler_pin(manifest), Err(CompilerPinError::NotAString));
    }

    #[test]
    fn read_pin_malformed_toml_rejected() {
        let manifest = b"compiler = \"edda 1.0\"\n[package\n";
        assert!(matches!(
            read_compiler_pin(manifest),
            Err(CompilerPinError::Malformed(_))
        ));
    }
}
