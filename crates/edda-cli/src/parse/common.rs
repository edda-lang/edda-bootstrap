//! Shared verb-parser helpers: the common-flag-only parser, the
//! positional-collecting parser, and the single-positional arity check.
//! Each per-verb parser submodule reaches for these.

use std::iter::Peekable;
use std::slice::Iter;

use edda_diag::Diagnostics;

use crate::cli::CommonArgs;
use crate::common_flags::{try_consume_common_long, try_consume_common_short};
use crate::flags::{Arg, classify, emit_parse_error};

/// Parse the flags of a verb that takes only the common flag set.
pub(super) fn parse_common_only(
    verb_label: &'static str,
    rest: &mut Peekable<Iter<'_, String>>,
    diags: &mut Diagnostics,
) -> Option<CommonArgs> {
    let mut common = CommonArgs::default();
    while let Some(raw) = rest.next() {
        match classify(raw) {
            Arg::Long { name, value } => {
                if !try_consume_common_long(&mut common, name, value, rest, diags) {
                    emit_parse_error(
                        diags,
                        format!("`{}`: unknown flag `--{}`", verb_label, name),
                    );
                }
            }
            Arg::Short { name, attached } => {
                if !try_consume_common_short(&mut common, name, attached, rest, diags) {
                    emit_parse_error(
                        diags,
                        format!("`{}`: unknown flag `-{}`", verb_label, name),
                    );
                }
            }
            Arg::Positional(s) => {
                emit_parse_error(
                    diags,
                    format!(
                        "`{}`: unexpected positional argument `{}`",
                        verb_label, s
                    ),
                );
            }
            Arg::Separator => break,
        }
    }
    Some(common)
}

/// Parse the common-flag set and collect every positional argument into a `Vec`.
pub(super) fn parse_with_positionals(
    verb_label: &'static str,
    rest: &mut Peekable<Iter<'_, String>>,
    diags: &mut Diagnostics,
) -> (CommonArgs, Vec<String>) {
    let mut common = CommonArgs::default();
    let mut positionals = Vec::new();
    while let Some(raw) = rest.next() {
        match classify(raw) {
            Arg::Long { name, value } => {
                if !try_consume_common_long(&mut common, name, value, rest, diags) {
                    emit_parse_error(
                        diags,
                        format!("`{}`: unknown flag `--{}`", verb_label, name),
                    );
                }
            }
            Arg::Short { name, attached } => {
                if !try_consume_common_short(&mut common, name, attached, rest, diags) {
                    emit_parse_error(
                        diags,
                        format!("`{}`: unknown flag `-{}`", verb_label, name),
                    );
                }
            }
            Arg::Positional(s) => positionals.push(s.to_owned()),
            Arg::Separator => break,
        }
    }
    (common, positionals)
}

/// Enforce that exactly one positional argument was supplied.
pub(super) fn take_single_positional(
    verb_label: &'static str,
    name: &'static str,
    mut positionals: Vec<String>,
    diags: &mut Diagnostics,
) -> Option<String> {
    match positionals.len() {
        0 => {
            emit_parse_error(
                diags,
                format!("`{}` requires a `<{}>` argument", verb_label, name),
            );
            None
        }
        1 => Some(positionals.remove(0)),
        n => {
            emit_parse_error(
                diags,
                format!(
                    "`{}` takes exactly one `<{}>` argument (got {})",
                    verb_label, name, n
                ),
            );
            None
        }
    }
}
