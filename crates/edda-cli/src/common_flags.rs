//! Shared long/short flag dispatchers for the locked §10 common flag
//! set. Every verb parser in [`crate::parse`] consults these before
//! falling through to verb-specific flags.

use std::iter::Peekable;
use std::path::PathBuf;
use std::slice::Iter;

use edda_diag::Diagnostics;

use edda_driver::StructureBudgetMode;

use crate::cli::{CommonArgs, Verbosity};
use crate::flags::{
    emit_parse_error, parse_class_list, parse_comma_list, parse_jobs, take_value,
};

/// Try to consume a long flag against the common-flag set. Returns
/// `true` if the flag was recognised (whether or not its value parsed
/// cleanly); `false` if the flag name is unknown to this layer.
pub(crate) fn try_consume_common_long<'a>(
    common: &mut CommonArgs,
    name: &str,
    value: Option<&'a str>,
    rest: &mut Peekable<Iter<'a, String>>,
    diags: &mut Diagnostics,
) -> bool {
    match name {
        "target" => {
            if let Some(v) = take_value("--target", value, rest, diags) {
                common.target = Some(v.to_owned());
            }
            true
        }
        "features" => {
            if let Some(v) = take_value("--features", value, rest, diags) {
                common.features.extend(parse_comma_list(v));
            }
            true
        }
        "profile" => {
            if let Some(v) = take_value("--profile", value, rest, diags) {
                common.profile = Some(v.to_owned());
            }
            true
        }
        "manifest-path" => {
            if let Some(v) = take_value("--manifest-path", value, rest, diags) {
                common.manifest_path = Some(PathBuf::from(v));
            }
            true
        }
        "warn-as-error" => {
            if let Some(v) = take_value("--warn-as-error", value, rest, diags) {
                common
                    .warn_as_error
                    .extend(parse_class_list("--warn-as-error", v, diags));
            }
            true
        }
        "allow" => {
            // `--allow` was removed: the
            // per-invocation opt-out feature no longer exists. Surface a
            // `parse_error` rather than silently swallowing the value.
            if let Some(_v) = take_value("--allow", value, rest, diags) {
                emit_parse_error(
                    diags,
                    "`--allow` is no longer a recognised flag — the lint opt-out feature was removed. Use `--warn-as-error <classes>` to escalate; classes cannot be suppressed.",
                );
            } else {
                emit_parse_error(
                    diags,
                    "`--allow` is no longer a recognised flag — the lint opt-out feature was removed.",
                );
            }
            true
        }
        "freestanding" => {
            consume_boolean_long("--freestanding", value, diags);
            common.freestanding = true;
            true
        }
        "quiet" => {
            consume_boolean_long("--quiet", value, diags);
            common.verbosity = Verbosity::Quiet;
            true
        }
        "verbose" => {
            consume_boolean_long("--verbose", value, diags);
            common.verbosity = Verbosity::Verbose;
            true
        }
        "jobs" => {
            if let Some(v) = take_value("--jobs", value, rest, diags) {
                if let Some(n) = parse_jobs("--jobs", v, diags) {
                    common.jobs = Some(n);
                }
            }
            true
        }
        "structure-budget" => {
            if let Some(v) = take_value("--structure-budget", value, rest, diags) {
                match StructureBudgetMode::from_flag_str(v) {
                    Some(mode) => common.structure_budget = mode,
                    None => emit_parse_error(
                        diags,
                        format!(
                            "`--structure-budget` expects `off`, `report`, or `error` (got `{v}`)"
                        ),
                    ),
                }
            }
            true
        }
        _ => false,
    }
}

/// Try to consume a short flag against the common-flag set.
pub(crate) fn try_consume_common_short<'a>(
    common: &mut CommonArgs,
    name: char,
    attached: Option<&'a str>,
    rest: &mut Peekable<Iter<'a, String>>,
    diags: &mut Diagnostics,
) -> bool {
    match name {
        'q' => {
            reject_attached('q', attached, diags);
            common.verbosity = Verbosity::Quiet;
            true
        }
        'v' => {
            reject_attached('v', attached, diags);
            common.verbosity = Verbosity::Verbose;
            true
        }
        'j' => {
            if let Some(v) = take_value("-j", attached, rest, diags) {
                if let Some(n) = parse_jobs("-j", v, diags) {
                    common.jobs = Some(n);
                }
            }
            true
        }
        _ => false,
    }
}

/// Validate that a boolean long flag was not given an explicit value.
pub(crate) fn consume_boolean_long(
    flag: &str,
    value: Option<&str>,
    diags: &mut Diagnostics,
) {
    if let Some(v) = value {
        emit_parse_error(
            diags,
            format!("flag `{}` takes no value (got `={}`)", flag, v),
        );
    }
}

/// Validate that a boolean short flag has no attached suffix.
pub(crate) fn reject_attached(
    flag: char,
    attached: Option<&str>,
    diags: &mut Diagnostics,
) {
    if let Some(v) = attached {
        emit_parse_error(
            diags,
            format!("flag `-{}` takes no value (got `{}`)", flag, v),
        );
    }
}
