//! Parse the [package] table (name, version, root_namespace, ...).


use edda_diag::{DiagnosticClass, Diagnostics, LintConfig};
use edda_span::FileId;

use crate::schema::{PackageKind, SemVer};
use super::{
    RESERVED_ROOT_NAMESPACES, emit, emit_unknown_key, fspan,
    is_identifier, is_package_name, parse_semver_text, require_field,
};

/// Fields parsed out of the `[package]` table. The first four are
/// required/locked; the last four are the optional descriptive keys
/// (codex `06-tooling.md` §9.3) captured verbatim.
pub(super) struct ParsedPackage {
    pub name: Box<str>,
    pub version: SemVer,
    pub root_namespace: Box<str>,
    pub max_trust: Option<u32>,
    pub edition: Option<Box<str>>,
    pub authors: Vec<Box<str>>,
    pub license: Option<Box<str>>,
    pub description: Option<Box<str>>,
    pub kind: Option<PackageKind>,
}

pub(super) fn parse_package_section(
    value: &toml::Value,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<ParsedPackage> {
    let tbl = match value.as_table() {
        Some(t) => t,
        None => {
            emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                 "`[package]` must be a table");
            return None;
        }
    };

    let mut name: Option<Box<str>> = None;
    let mut version: Option<SemVer> = None;
    let mut root_namespace: Option<Box<str>> = None;
    let mut max_trust: Option<u32> = None;
    let mut edition: Option<Box<str>> = None;
    let mut authors: Vec<Box<str>> = Vec::new();
    let mut license: Option<Box<str>> = None;
    let mut description: Option<Box<str>> = None;
    let mut kind: Option<PackageKind> = None;
    let mut had_error = false;

    for (k, v) in tbl {
        match k.as_str() {
            "name" => match v.as_str() {
                Some(s) => {
                    if !is_package_name(s) {
                        emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                             format!("`[package].name` must be lowercase with hyphens or underscores; got {s:?}"));
                        had_error = true;
                    } else {
                        name = Some(s.into());
                    }
                }
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         "`[package].name` must be a string");
                    had_error = true;
                }
            },
            "version" => match v.as_str() {
                Some(s) => match parse_semver_text(s) {
                    Some(sv) => version = Some(sv),
                    None => {
                        emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                             format!("invalid SemVer in `[package].version`: {s:?}"));
                        had_error = true;
                    }
                },
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         "`[package].version` must be a string");
                    had_error = true;
                }
            },
            "root_namespace" => match v.as_str() {
                Some(s) => {
                    if !is_identifier(s) {
                        emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                             format!("`[package].root_namespace` must be `lowercase_snake_case`; got {s:?}"));
                        had_error = true;
                    } else if RESERVED_ROOT_NAMESPACES.contains(&s) {
                        emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                             format!("`root_namespace` cannot be the reserved prefix `{s}` (build-system.md §4)"));
                        had_error = true;
                    } else {
                        root_namespace = Some(s.into());
                    }
                }
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         "`[package].root_namespace` must be a string");
                    had_error = true;
                }
            },
            "max_trust" => match v.as_integer() {
                Some(n) if (0..=u32::MAX as i64).contains(&n) => max_trust = Some(n as u32),
                Some(n) => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         format!("`[package].max_trust` must be a non-negative integer that fits in u32; got {n}"));
                    had_error = true;
                }
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         "`[package].max_trust` must be an integer");
                    had_error = true;
                }
            },
            "edition" => match v.as_str() {
                Some(s) => edition = Some(s.into()),
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         "`[package].edition` must be a string");
                    had_error = true;
                }
            },
            "authors" => match v.as_array() {
                Some(arr) => {
                    let mut collected = Vec::with_capacity(arr.len());
                    for entry in arr {
                        match entry.as_str() {
                            Some(s) => collected.push(s.into()),
                            None => {
                                emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                                     "`[package].authors` must be an array of strings");
                                had_error = true;
                            }
                        }
                    }
                    authors = collected;
                }
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         "`[package].authors` must be an array of strings");
                    had_error = true;
                }
            },
            "kind" => match v.as_str() {
                Some(s) => match PackageKind::from_name(s) {
                    Some(k) => kind = Some(k),
                    None => {
                        emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                             format!("`[package].kind` must be one of `executable`, `static_library`, `dynamic_library`; got {s:?}"));
                        had_error = true;
                    }
                },
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         "`[package].kind` must be a string");
                    had_error = true;
                }
            },
            "license" => match v.as_str() {
                Some(s) => license = Some(s.into()),
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         "`[package].license` must be a string");
                    had_error = true;
                }
            },
            "description" => match v.as_str() {
                Some(s) => description = Some(s.into()),
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         "`[package].description` must be a string");
                    had_error = true;
                }
            },
            _ => emit_unknown_key(k, file, diags, lint_cfg),
        }
    }

    if had_error {
        return None;
    }
    let name = require_field(name, "[package].name", file, diags, lint_cfg)?;
    let version = require_field(version, "[package].version", file, diags, lint_cfg)?;
    let root_namespace = require_field(root_namespace, "[package].root_namespace", file, diags, lint_cfg)?;
    Some(ParsedPackage {
        name,
        version,
        root_namespace,
        max_trust,
        edition,
        authors,
        license,
        description,
        kind,
    })
}

