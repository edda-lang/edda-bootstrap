//! Parse the [lints] table.


use edda_diag::{DiagnosticClass, Diagnostics, LintConfig, LintSeverity};
use edda_span::FileId;

use super::{
    emit, fspan,
};

pub(super) fn parse_lints(
    value: &toml::Value,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<LintConfig> {
    let tbl = match value.as_table() {
        Some(t) => t,
        None => {
            emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                 "`[lints]` must be a table");
            return None;
        }
    };
    let mut out = LintConfig::new();
    for (class_name, sev_val) in tbl {
        let class = match DiagnosticClass::from_name(class_name) {
            Some(c) => c,
            None => {
                emit(diags, lint_cfg, DiagnosticClass::UnknownManifestKey, fspan(file),
                     format!("unknown diagnostic class {class_name:?}; ignoring entry"));
                continue;
            }
        };
        let severity = match sev_val.as_str() {
            Some("allow") => {
                emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                     format!(
                        "`allow` is no longer a permitted lint severity — the per-package opt-out feature was removed. Every diagnostic class must be left at its default or escalated; the class for `{}` cannot be suppressed.",
                        class.name(),
                     ));
                continue;
            }
            Some(s) => match LintSeverity::from_name(s) {
                Some(sv) => sv,
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         format!("invalid lint severity {s:?}; expected `warn`/`deny`/`error`"));
                    return None;
                }
            },
            None => {
                emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                     format!("lint severity for `{class_name}` must be a string"));
                return None;
            }
        };
        out.set(class, severity);
    }
    Some(out)
}

