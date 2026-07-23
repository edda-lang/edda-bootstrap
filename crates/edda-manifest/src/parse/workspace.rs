//! Parse the [workspace] table (members, discover, default_run).


use edda_diag::{DiagnosticClass, Diagnostics, LintConfig};
use edda_span::FileId;

use crate::schema::{
    WorkspaceDiscover, WorkspaceTable,
};
use super::{
    emit, emit_unknown_key, fspan, invalid_member_path_reason,
};

pub(super) fn parse_workspace(
    value: &toml::Value,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<WorkspaceTable> {
    let table = match value.as_table() {
        Some(t) => t,
        None => {
            emit(
                diags,
                lint_cfg,
                DiagnosticClass::ParseError,
                fspan(file),
                "`[workspace]` must be a table".to_owned(),
            );
            return None;
        }
    };

    let mut members: Vec<Box<str>> = Vec::new();
    let mut members_seen = false;
    let mut discover: Option<WorkspaceDiscover> = None;
    let mut default_run: Option<Box<str>> = None;
    let mut had_error = false;
    for (key, value) in table {
        match key.as_str() {
            "members" => {
                members_seen = true;
                match parse_workspace_members(value, file, diags, lint_cfg) {
                    Some(v) => members = v,
                    None => had_error = true,
                }
            }
            "discover" => match parse_workspace_discover(value, file, diags, lint_cfg) {
                Some(v) => discover = Some(v),
                None => had_error = true,
            },
            "default_run" => match parse_workspace_default_run(value, file, diags, lint_cfg) {
                Some(v) => default_run = Some(v),
                None => had_error = true,
            },
            _ => emit_unknown_key(key, file, diags, lint_cfg),
        }
    }

    if had_error {
        return None;
    }
    if discover.is_some() && members_seen {
        emit(
            diags,
            lint_cfg,
            DiagnosticClass::ParseError,
            fspan(file),
            "`[workspace]` may set `members` or `discover`, not both — \
             the filesystem and a hand-maintained list cannot both be the source of truth"
                .to_owned(),
        );
        return None;
    }
    if discover.is_none() && members.is_empty() {
        emit(
            diags,
            lint_cfg,
            DiagnosticClass::ParseError,
            fspan(file),
            "`[workspace]` must set either `members = [...]` or `discover = true`".to_owned(),
        );
        return None;
    }

    Some(WorkspaceTable {
        members,
        discover,
        default_run,
    })
}

/// Parse the value of the `[workspace] default_run` key: a `lib/`-relative
/// member path naming the member a bare `edda run` builds + launches.
/// Admitted in the same POSIX-style relative shape as a `members` entry.
fn parse_workspace_default_run(
    value: &toml::Value,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<Box<str>> {
    let Some(s) = value.as_str() else {
        emit(
            diags,
            lint_cfg,
            DiagnosticClass::ParseError,
            fspan(file),
            "`[workspace].default_run` must be a string naming a workspace member".to_owned(),
        );
        return None;
    };
    if let Some(reason) = invalid_member_path_reason(s) {
        emit(
            diags,
            lint_cfg,
            DiagnosticClass::ParseError,
            fspan(file),
            format!("`[workspace].default_run` {s:?} {reason}"),
        );
        return None;
    }
    Some(s.into())
}

/// Parse the value of the `[workspace] discover` key. Admits `true`
/// (walks `lib/`) or a string path (walks the named directory). `false`
/// is a parse error — the absence-form is "omit the key entirely".
fn parse_workspace_discover(
    value: &toml::Value,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<WorkspaceDiscover> {
    if let Some(b) = value.as_bool() {
        if b {
            return Some(WorkspaceDiscover::LibRoot);
        }
        emit(
            diags,
            lint_cfg,
            DiagnosticClass::ParseError,
            fspan(file),
            "`[workspace].discover = false` is not admitted — omit the key to disable auto-discovery"
                .to_owned(),
        );
        return None;
    }
    if let Some(s) = value.as_str() {
        if let Some(reason) = invalid_member_path_reason(s) {
            emit(
                diags,
                lint_cfg,
                DiagnosticClass::ParseError,
                fspan(file),
                format!("`[workspace].discover` path {s:?} {reason}"),
            );
            return None;
        }
        return Some(WorkspaceDiscover::Path(s.into()));
    }
    emit(
        diags,
        lint_cfg,
        DiagnosticClass::ParseError,
        fspan(file),
        "`[workspace].discover` must be `true` or a relative path string".to_owned(),
    );
    None
}

fn parse_workspace_members(
    value: &toml::Value,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<Vec<Box<str>>> {
    let array = match value.as_array() {
        Some(a) => a,
        None => {
            emit(
                diags,
                lint_cfg,
                DiagnosticClass::ParseError,
                fspan(file),
                "`[workspace].members` must be an array of strings".to_owned(),
            );
            return None;
        }
    };

    let mut out: Vec<Box<str>> = Vec::with_capacity(array.len());
    let mut seen: std::collections::HashSet<Box<str>> = std::collections::HashSet::new();
    for entry in array {
        let s = match entry.as_str() {
            Some(s) => s,
            None => {
                emit(
                    diags,
                    lint_cfg,
                    DiagnosticClass::ParseError,
                    fspan(file),
                    "workspace member must be a string".to_owned(),
                );
                return None;
            }
        };
        if let Some(reason) = invalid_member_path_reason(s) {
            emit(
                diags,
                lint_cfg,
                DiagnosticClass::ParseError,
                fspan(file),
                format!("workspace member {s:?} {reason}"),
            );
            return None;
        }
        let boxed: Box<str> = s.into();
        if !seen.insert(boxed.clone()) {
            emit(
                diags,
                lint_cfg,
                DiagnosticClass::ParseError,
                fspan(file),
                format!("workspace member {s:?} appears more than once"),
            );
            return None;
        }
        out.push(boxed);
    }

    Some(out)
}

