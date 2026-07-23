//! Parse the [codegen] table and its GC schedule.


use edda_diag::{DiagnosticClass, Diagnostics, LintConfig};
use edda_span::FileId;

use crate::schema::{
    CodegenConfig, Compression, DefaultTier, GcSchedule, GcTag, GlobalCache,
};
use super::{
    emit, emit_unknown_key, fspan,
};

pub(super) fn parse_codegen(
    value: &toml::Value,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<CodegenConfig> {
    let tbl = match value.as_table() {
        Some(t) => t,
        None => {
            emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                 "`[codegen]` must be a table");
            return None;
        }
    };
    let mut out = CodegenConfig::locked_defaults();
    for (k, v) in tbl {
        match k.as_str() {
            "default_tier" => match v.as_str() {
                Some(s) => match DefaultTier::from_name(s) {
                    Some(t) => out.default_tier = t,
                    None => {
                        emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                             format!("invalid `codegen.default_tier` {s:?}; expected `auto` or `cache`"));
                        return None;
                    }
                },
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         "`codegen.default_tier` must be a string");
                    return None;
                }
            },
            "compression" => match v.as_str() {
                Some(s) => match Compression::from_name(s) {
                    Some(c) => out.compression = c,
                    None => {
                        emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                             format!("invalid `codegen.compression` {s:?}; expected `false` or `zstd`"));
                        return None;
                    }
                },
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         "`codegen.compression` must be a string");
                    return None;
                }
            },
            "global_cache" => match v.as_str() {
                Some(s) => match GlobalCache::from_name(s) {
                    Some(g) => out.global_cache = g,
                    None => {
                        emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                             format!("invalid `codegen.global_cache` {s:?}; expected `enabled` or `disabled`"));
                        return None;
                    }
                },
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         "`codegen.global_cache` must be a string");
                    return None;
                }
            },
            "gc_schedule" => match parse_gc_schedule(v, file, diags, lint_cfg) {
                Some(s) => out.gc_schedule = s,
                None => return None,
            },
            _ => emit_unknown_key(k, file, diags, lint_cfg),
        }
    }
    Some(out)
}

fn parse_gc_schedule(
    value: &toml::Value,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<GcSchedule> {
    let tbl = match value.as_table() {
        Some(t) => t,
        None => {
            emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                 "`[codegen.gc_schedule]` must be a table");
            return None;
        }
    };
    let mut out = GcSchedule::locked_defaults();
    for (k, v) in tbl {
        let tag = match v.as_str().and_then(GcTag::from_name) {
            Some(t) => t,
            None => {
                let raw = v.as_str().unwrap_or("(non-string)");
                emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                     format!("invalid GC tag {raw:?}; expected one of `never`/`on_ci`/`daily`/`weekly`/`on_promote_revoke`"));
                return None;
            }
        };
        match k.as_str() {
            "cache_tier" => out.cache_tier = tag,
            "repo_tier" => out.repo_tier = tag,
            "global_cache" => out.global_cache = tag,
            _ => emit_unknown_key(k, file, diags, lint_cfg),
        }
    }
    Some(out)
}

