//! Parse the [structmap] table.


use edda_diag::{DiagnosticClass, Diagnostics, LintConfig};
use edda_span::FileId;

use crate::schema::{StructmapConfig, TokenBudgetEncoding};
use super::{
    emit, emit_unknown_key, fspan,
};

pub(super) fn parse_structmap(
    value: &toml::Value,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<StructmapConfig> {
    let tbl = match value.as_table() {
        Some(t) => t,
        None => {
            emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                 "`[structmap]` must be a table");
            return None;
        }
    };
    let mut out = StructmapConfig::locked_defaults();
    for (k, v) in tbl {
        match k.as_str() {
            "descendant_tree" => match v.as_bool() {
                Some(b) => out.descendant_tree = b,
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         "`structmap.descendant_tree` must be a boolean");
                    return None;
                }
            },
            "token_budget_node_green" => out.node_green_max = parse_token_field(v, k, file, diags, lint_cfg)?,
            "token_budget_node_red" => out.node_amber_max = parse_token_field(v, k, file, diags, lint_cfg)?,
            "token_budget_chars_per_token" => out.chars_per_token_centi = parse_chars_per_token(v, file, diags, lint_cfg)?,
            "token_budget_encoding" => match v.as_str().and_then(TokenBudgetEncoding::from_config_str) {
                Some(enc) => out.token_budget_encoding = enc,
                None => {
                    emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                         "`structmap.token_budget_encoding` must be `o200k_base` or `cl100k_base`");
                    return None;
                }
            },
            _ => emit_unknown_key(k, file, diags, lint_cfg),
        }
    }
    Some(out)
}

/// Parse a non-negative integer `[structmap]` token-budget threshold.
fn parse_token_field(
    value: &toml::Value,
    key: &str,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<usize> {
    match value.as_integer() {
        Some(n) if n >= 0 => Some(n as usize),
        _ => {
            emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                 format!("`structmap.{key}` must be a non-negative integer (tokens)"));
            None
        }
    }
}

/// Parse the fallback chars-per-token ratio and store it as hundredths.
fn parse_chars_per_token(
    value: &toml::Value,
    file: FileId,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> Option<u32> {
    let ratio = value
        .as_float()
        .or_else(|| value.as_integer().map(|n| n as f64));
    match ratio {
        Some(r) if r > 0.0 => Some((r * 100.0).round() as u32),
        _ => {
            emit(diags, lint_cfg, DiagnosticClass::ParseError, fspan(file),
                 "`structmap.token_budget_chars_per_token` must be a positive number");
            None
        }
    }
}

