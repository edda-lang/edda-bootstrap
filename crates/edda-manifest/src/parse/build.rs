//! Parse the [build] table.


use edda_diag::{DiagnosticClass, Diagnostics, LintConfig};
use edda_span::FileId;
use edda_target::TargetTriple;

use crate::schema::{
    BuildConfig, FeatureSpec,
};
use super::{
    emit, emit_unknown_key, fspan,
    is_identifier,
};

pub(super) fn parse_build(
    value: &toml::Value,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<BuildConfig> {
    let tbl = match value.as_table() {
        Some(t) => t,
        None => {
            emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                 "`[build]` must be a table");
            return None;
        }
    };
    let mut out = BuildConfig {
        default_target: None,
        default_features: Vec::new(),
        default_profile: "dev".into(),
        emit_structmap: true,
    };
    for (k, v) in tbl {
        match k.as_str() {
            "default_target" => match v.as_str() {
                Some(s) => match TargetTriple::parse(s) {
                    Ok(t) => out.default_target = Some(t),
                    Err(e) => {
                        emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                             format!("invalid `default_target`: {e}"));
                        return None;
                    }
                },
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         "`build.default_target` must be a string");
                    return None;
                }
            },
            "default_features" => match v.as_array() {
                Some(arr) => {
                    let mut features = Vec::with_capacity(arr.len());
                    for item in arr {
                        match item.as_str() {
                            Some(s) => features.push(FeatureSpec { name: s.into(), span: fspan(file) }),
                            None => {
                                emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                                     "`build.default_features` items must be strings");
                                return None;
                            }
                        }
                    }
                    out.default_features = features;
                }
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         "`build.default_features` must be an array of strings");
                    return None;
                }
            },
            "default_profile" => match v.as_str() {
                Some(s) => {
                    if !is_identifier(s) {
                        emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                             format!("`build.default_profile` must be `lowercase_snake_case`; got {s:?}"));
                        return None;
                    }
                    out.default_profile = s.into();
                }
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         "`build.default_profile` must be a string");
                    return None;
                }
            },
            "emit_structmap" => match v.as_bool() {
                Some(b) => out.emit_structmap = b,
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         "`build.emit_structmap` must be a boolean");
                    return None;
                }
            },
            _ => emit_unknown_key(k, file, diags, lint_cfg),
        }
    }
    Some(out)
}

