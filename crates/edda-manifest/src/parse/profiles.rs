//! Parse the [profiles] table (dev/release/bench).


use edda_diag::{DiagnosticClass, Diagnostics, LintConfig};
use edda_span::FileId;

use crate::schema::{
    DebugInfo, Profile, ProfileSet,
};
use super::{
    emit, fspan,
};

pub(super) fn parse_profiles(
    mut profiles: ProfileSet,
    value: &toml::Value,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<ProfileSet> {
    let tbl = match value.as_table() {
        Some(t) => t,
        None => {
            emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                 "`[profiles]` must be a table");
            return None;
        }
    };
    for (name, profile_val) in tbl {
        let profile_tbl = match profile_val.as_table() {
            Some(t) => t,
            None => {
                emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                     format!("`[profiles.{name}]` must be a table"));
                return None;
            }
        };
        let opt_level: u8 = match profile_tbl.get("opt_level").and_then(|v| v.as_integer()) {
            Some(n) if (0..=3).contains(&n) => n as u8,
            Some(n) => {
                emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                     format!("`profiles.{name}.opt_level` must be 0..=3; got {n}"));
                return None;
            }
            None => {
                emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                     format!("`[profiles.{name}]` missing required `opt_level` (integer 0..=3)"));
                return None;
            }
        };
        let debug_info = match profile_tbl.get("debug_info").and_then(|v| v.as_str()) {
            Some(s) => match DebugInfo::from_name(s) {
                Some(d) => d,
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         format!("invalid `profiles.{name}.debug_info` {s:?}; expected `full` or `line-tables-only`"));
                    return None;
                }
            },
            None => {
                emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                     format!("`[profiles.{name}]` missing required `debug_info`"));
                return None;
            }
        };
        let sanitizers: Vec<Box<str>> = match profile_tbl.get("sanitizers") {
            Some(v) => match v.as_array() {
                Some(arr) => {
                    let mut out = Vec::with_capacity(arr.len());
                    for item in arr {
                        match item.as_str() {
                            Some(s) => out.push(Box::from(s)),
                            None => {
                                emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                                     format!("`profiles.{name}.sanitizers` items must be strings"));
                                return None;
                            }
                        }
                    }
                    out
                }
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         format!("`profiles.{name}.sanitizers` must be an array"));
                    return None;
                }
            },
            None => Vec::new(),
        };
        profiles.set(name.clone().into(), Profile { opt_level, debug_info, sanitizers });
    }
    Some(profiles)
}

