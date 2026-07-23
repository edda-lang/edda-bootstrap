//! Argv tokenizer and value-parsing helpers for `edda-cli`.
//!
//! The big-picture parsing loop lives in [`crate::parse`]; this module
//! provides the small primitives that loop reaches for repeatedly:
//! classifying an argv element, splitting `--flag=value`, parsing
//! comma-separated lists, and lowering text into typed values like
//! [`edda_diag::DiagnosticClass`].

use std::iter::Peekable;
use std::slice::Iter;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, Severity};
use edda_span::Span;

/// Result of classifying a single argv element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arg<'a> {
    /// `--long-name` or `--long-name=value`.
    Long {
        /// Flag name without the leading `--`.
        name: &'a str,
        /// Value following an `=`, if any.
        value: Option<&'a str>,
    },
    /// `-x` or `-xVALUE` (short flag, optionally with attached value like `-j8`).
    Short {
        /// Single-character flag name.
        name: char,
        /// Characters following the flag char, if any.
        attached: Option<&'a str>,
    },
    /// Any token without a leading `-`, plus the bare `-` token itself.
    Positional(&'a str),
    /// The bare `--` separator (everything after is positional per common convention; v0.1 unused).
    Separator,
}

/// Classify a single argv element into one of the [`Arg`] shapes.
pub fn classify(arg: &str) -> Arg<'_> {
    if let Some(rest) = arg.strip_prefix("--") {
        if rest.is_empty() {
            return Arg::Separator;
        }
        if let Some((name, value)) = rest.split_once('=') {
            return Arg::Long {
                name,
                value: Some(value),
            };
        }
        return Arg::Long { name: rest, value: None };
    }
    if let Some(rest) = arg.strip_prefix('-') {
        if rest.is_empty() {
            return Arg::Positional(arg);
        }
        let mut chars = rest.chars();
        let name = chars.next().expect("non-empty after strip_prefix");
        let attached = chars.as_str();
        let attached = if attached.is_empty() { None } else { Some(attached) };
        return Arg::Short { name, attached };
    }
    Arg::Positional(arg)
}

/// Resolve a flag's value, taking it from `explicit` (the `--k=v` form
/// or a short flag's attached suffix) when present, or pulling the next
/// argv element otherwise. Emits `parse_error` and returns `None` if
/// the value is required but absent.
pub fn take_value<'a>(
    flag_name: &str,
    explicit: Option<&'a str>,
    rest: &mut Peekable<Iter<'a, String>>,
    diags: &mut Diagnostics,
) -> Option<&'a str> {
    if let Some(v) = explicit {
        return Some(v);
    }
    match rest.peek() {
        Some(s) if !s.starts_with('-') || *s == "-" => {
            let next = rest.next().expect("peeked");
            Some(next.as_str())
        }
        _ => {
            emit_parse_error(
                diags,
                format!("flag `{}` expects a value", flag_name),
            );
            None
        }
    }
}

/// Split a comma-separated list into a `Vec<String>`. Whitespace around
/// each item is trimmed; empty entries (e.g. `a,,b`) are dropped.
pub fn parse_comma_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Parse a `--warn-as-error` / `--allow` argument: a comma-list of
/// `lowercase_snake_case` class names lowered into typed
/// [`DiagnosticClass`] values. Unknown class names are reported as
/// `parse_error` and skipped.
pub fn parse_class_list(
    flag_name: &str,
    value: &str,
    diags: &mut Diagnostics,
) -> Vec<DiagnosticClass> {
    let mut out = Vec::new();
    for raw in parse_comma_list(value) {
        match DiagnosticClass::from_name(&raw) {
            Some(class) => out.push(class),
            None => emit_parse_error(
                diags,
                format!(
                    "flag `{}` references unknown diagnostic class `{}`",
                    flag_name, raw
                ),
            ),
        }
    }
    out
}

/// Parse the value of `--jobs` / `-j` as a positive `u32`. Zero,
/// negatives, or non-numeric input produce `parse_error` and return
/// `None`.
pub fn parse_jobs(
    flag_name: &str,
    value: &str,
    diags: &mut Diagnostics,
) -> Option<u32> {
    match value.parse::<u32>() {
        Ok(0) => {
            emit_parse_error(
                diags,
                format!("flag `{}` requires a positive integer (got 0)", flag_name),
            );
            None
        }
        Ok(n) => Some(n),
        Err(_) => {
            emit_parse_error(
                diags,
                format!(
                    "flag `{}` requires a positive integer (got `{}`)",
                    flag_name, value
                ),
            );
            None
        }
    }
}

/// Push a `parse_error` diagnostic at `Span::DUMMY`. Internal to the
/// CLI crate; mirrors the convention `edda-manifest::load` uses when
/// no file has been registered yet.
pub(crate) fn emit_parse_error(diags: &mut Diagnostics, msg: impl Into<String>) {
    diags.push(Diagnostic::new(
        DiagnosticClass::ParseError,
        Severity::Error,
        Span::DUMMY,
        msg,
    ));
}

