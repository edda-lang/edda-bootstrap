//! Parse [[dependencies]] entries, sources, and publisher pins.


use edda_diag::{DiagnosticClass, Diagnostics, LintConfig};
use edda_span::FileId;

use crate::schema::{
    Dependency,
    DependencySource, PublisherPin,
};
use super::{
    emit, fspan,
};

pub(super) fn parse_dependencies(
    value: &toml::Value,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<Vec<Dependency>> {
    let arr = match value.as_array() {
        Some(a) => a,
        None => {
            emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                 "`dependencies` must be an array of tables (`[[dependencies]]`)");
            return None;
        }
    };
    let mut out = Vec::with_capacity(arr.len());
    let mut seen: Vec<Box<str>> = Vec::new();
    for item in arr {
        let tbl = match item.as_table() {
            Some(t) => t,
            None => {
                emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                     "`[[dependencies]]` entries must be tables");
                return None;
            }
        };
        let name: Box<str> = match tbl.get("name").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.into(),
            _ => {
                emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                     "dependency entry missing required `name` field");
                return None;
            }
        };
        if seen.contains(&name) {
            emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                 format!("duplicate dependency name `{name}`"));
            return None;
        }
        seen.push(name.clone());
        let version_req: Box<str> = match tbl.get("version").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.into(),
            _ => {
                emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                     format!("dependency `{name}` missing required `version` field"));
                return None;
            }
        };
        let source = match tbl.get("source").and_then(|v| v.as_str()) {
            Some(s) => parse_dependency_source(s, file, diags, lint_cfg)?,
            None => {
                emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                     format!("dependency `{name}` missing required `source` field"));
                return None;
            }
        };
        let surface_hash = match tbl.get("surface_hash") {
            Some(v) => match v.as_str() {
                Some(s) => {
                    if let Err(reason) = validate_blake3_hash(s) {
                        emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                             format!("dependency `{name}` field `surface_hash` {reason}"));
                        return None;
                    }
                    Some(Box::from(s))
                }
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         format!("dependency `{name}` field `surface_hash` must be a string"));
                    return None;
                }
            },
            None => None,
        };
        let max_effects = match tbl.get("max_effects") {
            Some(v) => match v.as_array() {
                Some(arr) => {
                    let mut effects = Vec::with_capacity(arr.len());
                    for item in arr {
                        match item.as_str() {
                            Some(s) => effects.push(Box::from(s)),
                            None => {
                                emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                                     format!("dependency `{name}` field `max_effects` items must be strings"));
                                return None;
                            }
                        }
                    }
                    effects
                }
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         format!("dependency `{name}` field `max_effects` must be an array of strings"));
                    return None;
                }
            },
            None => Vec::new(),
        };
        let accept_unstable = match tbl.get("accept_unstable") {
            Some(v) => match v.as_bool() {
                Some(b) => b,
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         format!("dependency `{name}` field `accept_unstable` must be a boolean"));
                    return None;
                }
            },
            None => false,
        };
        let publisher = match tbl.get("publisher") {
            Some(v) => match v.as_table() {
                Some(pub_tbl) => {
                    Some(parse_publisher_pin(&name, pub_tbl, file, diags, lint_cfg)?)
                }
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         format!("dependency `{name}` field `publisher` must be a table"));
                    return None;
                }
            },
            None => None,
        };
        out.push(Dependency { name, version_req, source, surface_hash, max_effects, accept_unstable, publisher });
    }
    Some(out)
}

/// Validate that `s` starts with `"blake3:"` followed by at least one lowercase hex character.
/// Returns `Ok(())` on success, `Err(reason)` describing the failure.
fn validate_blake3_hash(s: &str) -> Result<(), &'static str> {
    let hex = match s.strip_prefix("blake3:") {
        Some(h) => h,
        None => return Err("must start with \"blake3:\" prefix"),
    };
    if hex.is_empty() {
        return Err("hex portion must not be empty");
    }
    if !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err("hex portion must contain only lowercase hex digits (0-9, a-f)");
    }
    Ok(())
}

/// Validate that `s` starts with `"ed25519:"` followed by at least one lowercase hex character.
fn validate_ed25519_prefix(s: &str) -> Result<(), &'static str> {
    let rest = match s.strip_prefix("ed25519:") {
        Some(r) => r,
        None => return Err("must start with \"ed25519:\" prefix"),
    };
    if rest.is_empty() {
        return Err("hex portion must not be empty");
    }
    if !rest.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err("hex portion must contain only lowercase hex digits (0-9, a-f)");
    }
    Ok(())
}

/// Parse the `publisher = { key_fingerprint = "ed25519:<hex>" }` sub-table within a `[[dependencies]]` entry.
fn parse_publisher_pin(
    dep_name: &str,
    tbl: &toml::map::Map<String, toml::Value>,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<PublisherPin> {
    let key_fingerprint = match tbl.get("key_fingerprint").and_then(|v| v.as_str()) {
        Some(s) => {
            if let Err(reason) = validate_ed25519_prefix(s) {
                emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                     format!("dependency `{dep_name}` publisher `key_fingerprint` {reason}"));
                return None;
            }
            Box::from(s)
        }
        None => {
            emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                 format!("dependency `{dep_name}` `[publisher]` table is missing required `key_fingerprint` field"));
            return None;
        }
    };
    Some(PublisherPin { key_fingerprint })
}

fn parse_dependency_source(
    raw: &str,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<DependencySource> {
    if raw == "registry" {
        return Some(DependencySource::Registry);
    }
    if let Some(url) = raw.strip_prefix("git+") {
        if url.is_empty() {
            emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                 "`git+` source requires a URL");
            return None;
        }
        return Some(DependencySource::Git(url.into()));
    }
    if let Some(path) = raw.strip_prefix("path+") {
        if path.is_empty() {
            emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                 "`path+` source requires a relative path");
            return None;
        }
        return Some(DependencySource::Path(path.into()));
    }
    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
         format!("unknown dependency source {raw:?}; expected `registry`, `git+<url>`, or `path+<rel>`"));
    None
}

