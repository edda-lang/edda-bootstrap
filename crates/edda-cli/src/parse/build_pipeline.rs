//! Per-verb parsers for the build-pipeline verbs: `test`, `fmt`, `build`,
//! `check`, and `gc`. Each consumes the common flag set plus its own
//! verb-specific boolean/selector flags.

use std::iter::Peekable;
use std::slice::Iter;

use edda_diag::Diagnostics;

use crate::cli::{BuildArgs, CheckArgs, FmtArgs, GcArgs, LintArgs, TestArgs};
use crate::common_flags::{
    consume_boolean_long, try_consume_common_long, try_consume_common_short,
};
use crate::flags::{Arg, classify, emit_parse_error};

/// Parse `edda test` flags (common + `--properties`) per the codex
/// `06-tooling.md` (`edda test [--filter <pattern>] [--properties]`).
/// `--filter` is reserved for a later wave and is intentionally not
/// accepted here yet — the codex grammar admits it but the bootstrap
/// has no test discovery to filter against.
pub(super) fn parse_test(
    rest: &mut Peekable<Iter<'_, String>>,
    diags: &mut Diagnostics,
) -> Option<TestArgs> {
    let mut args = TestArgs::default();
    while let Some(raw) = rest.next() {
        match classify(raw) {
            Arg::Long { name, value } => {
                if try_consume_common_long(&mut args.common, name, value, rest, diags) {
                    continue;
                }
                if name == "properties" {
                    consume_boolean_long("--properties", value, diags);
                    args.properties = true;
                    continue;
                }
                emit_parse_error(
                    diags,
                    format!("`edda test`: unknown flag `--{}`", name),
                );
            }
            Arg::Short { name, attached } => {
                if !try_consume_common_short(&mut args.common, name, attached, rest, diags) {
                    emit_parse_error(
                        diags,
                        format!("`edda test`: unknown flag `-{}`", name),
                    );
                }
            }
            Arg::Positional(s) => {
                emit_parse_error(
                    diags,
                    format!("`edda test`: unexpected positional argument `{}`", s),
                );
            }
            Arg::Separator => break,
        }
    }
    Some(args)
}

/// Parse `edda fmt` flags (common + `--strip-comments`). The strip flag
/// drives the no-comment migration (tokenizer-driven removal).
pub(super) fn parse_fmt(
    rest: &mut Peekable<Iter<'_, String>>,
    diags: &mut Diagnostics,
) -> Option<FmtArgs> {
    let mut args = FmtArgs::default();
    while let Some(raw) = rest.next() {
        match classify(raw) {
            Arg::Long { name, value } => {
                if try_consume_common_long(&mut args.common, name, value, rest, diags) {
                    continue;
                }
                if name == "strip-comments" {
                    consume_boolean_long("--strip-comments", value, diags);
                    args.strip_comments = true;
                    continue;
                }
                emit_parse_error(diags, format!("`edda fmt`: unknown flag `--{}`", name));
            }
            Arg::Short { name, attached } => {
                if !try_consume_common_short(&mut args.common, name, attached, rest, diags) {
                    emit_parse_error(diags, format!("`edda fmt`: unknown flag `-{}`", name));
                }
            }
            Arg::Positional(s) => {
                emit_parse_error(
                    diags,
                    format!("`edda fmt`: unexpected positional argument `{}`", s),
                );
            }
            Arg::Separator => break,
        }
    }
    Some(args)
}

/// Parse `edda build` flags (common + `--full-materialization`).
pub(super) fn parse_build(
    rest: &mut Peekable<Iter<'_, String>>,
    diags: &mut Diagnostics,
) -> Option<BuildArgs> {
    let mut args = BuildArgs::default();
    while let Some(raw) = rest.next() {
        match classify(raw) {
            Arg::Long { name, value } => {
                if try_consume_common_long(&mut args.common, name, value, rest, diags) {
                    continue;
                }
                if name == "full-materialization" {
                    consume_boolean_long("--full-materialization", value, diags);
                    args.full_materialization = true;
                    continue;
                }
                emit_parse_error(
                    diags,
                    format!("`edda build`: unknown flag `--{}`", name),
                );
            }
            Arg::Short { name, attached } => {
                if !try_consume_common_short(&mut args.common, name, attached, rest, diags) {
                    emit_parse_error(
                        diags,
                        format!("`edda build`: unknown flag `-{}`", name),
                    );
                }
            }
            Arg::Positional(s) => {
                emit_parse_error(
                    diags,
                    format!("`edda build`: unexpected positional argument `{}`", s),
                );
            }
            Arg::Separator => break,
        }
    }
    Some(args)
}

/// Parse `edda check` flags (common + `--full-materialization`).
pub(super) fn parse_check(
    rest: &mut Peekable<Iter<'_, String>>,
    diags: &mut Diagnostics,
) -> Option<CheckArgs> {
    let mut args = CheckArgs::default();
    while let Some(raw) = rest.next() {
        match classify(raw) {
            Arg::Long { name, value } => {
                if try_consume_common_long(&mut args.common, name, value, rest, diags) {
                    continue;
                }
                if name == "full-materialization" {
                    consume_boolean_long("--full-materialization", value, diags);
                    args.full_materialization = true;
                    continue;
                }
                emit_parse_error(
                    diags,
                    format!("`edda check`: unknown flag `--{}`", name),
                );
            }
            Arg::Short { name, attached } => {
                if !try_consume_common_short(&mut args.common, name, attached, rest, diags) {
                    emit_parse_error(
                        diags,
                        format!("`edda check`: unknown flag `-{}`", name),
                    );
                }
            }
            Arg::Positional(s) => {
                emit_parse_error(
                    diags,
                    format!("`edda check`: unexpected positional argument `{}`", s),
                );
            }
            Arg::Separator => break,
        }
    }
    Some(args)
}

/// Parse `edda gc` flags (common + `--dry-run`, `--cache-tier`,
/// `--repo-tier`, `--global`).
pub(super) fn parse_gc(
    rest: &mut Peekable<Iter<'_, String>>,
    diags: &mut Diagnostics,
) -> Option<GcArgs> {
    let mut args = GcArgs::default();
    while let Some(raw) = rest.next() {
        match classify(raw) {
            Arg::Long { name, value } => {
                if try_consume_common_long(&mut args.common, name, value, rest, diags) {
                    continue;
                }
                match name {
                    "dry-run" => {
                        consume_boolean_long("--dry-run", value, diags);
                        args.dry_run = true;
                    }
                    "cache-tier" => {
                        consume_boolean_long("--cache-tier", value, diags);
                        args.cache_tier = true;
                    }
                    "repo-tier" => {
                        consume_boolean_long("--repo-tier", value, diags);
                        args.repo_tier = true;
                    }
                    "global" => {
                        consume_boolean_long("--global", value, diags);
                        args.global = true;
                    }
                    _ => emit_parse_error(
                        diags,
                        format!("`edda gc`: unknown flag `--{}`", name),
                    ),
                }
            }
            Arg::Short { name, attached } => {
                if !try_consume_common_short(&mut args.common, name, attached, rest, diags) {
                    emit_parse_error(
                        diags,
                        format!("`edda gc`: unknown flag `-{}`", name),
                    );
                }
            }
            Arg::Positional(s) => {
                emit_parse_error(
                    diags,
                    format!("`edda gc`: unexpected positional argument `{}`", s),
                );
            }
            Arg::Separator => break,
        }
    }
    if args.cache_tier && args.repo_tier {
        emit_parse_error(
            diags,
            "`edda gc`: `--cache-tier` and `--repo-tier` are mutually exclusive",
        );
    }
    Some(args)
}

/// Parse `edda lint` flags (common + `--trust-points`,
/// `--capability-safe-stdlib`). The two subflags select the audit
/// surfaces locked in codex `06-tooling.md` §8.
pub(super) fn parse_lint(
    rest: &mut Peekable<Iter<'_, String>>,
    diags: &mut Diagnostics,
) -> Option<LintArgs> {
    let mut args = LintArgs::default();
    while let Some(raw) = rest.next() {
        match classify(raw) {
            Arg::Long { name, value } => {
                if try_consume_common_long(&mut args.common, name, value, rest, diags) {
                    continue;
                }
                match name {
                    "trust-points" => {
                        consume_boolean_long("--trust-points", value, diags);
                        args.trust_points = true;
                    }
                    "capability-safe-stdlib" => {
                        consume_boolean_long("--capability-safe-stdlib", value, diags);
                        args.capability_safe_stdlib = true;
                    }
                    _ => emit_parse_error(
                        diags,
                        format!("`edda lint`: unknown flag `--{}`", name),
                    ),
                }
            }
            Arg::Short { name, attached } => {
                if !try_consume_common_short(&mut args.common, name, attached, rest, diags) {
                    emit_parse_error(
                        diags,
                        format!("`edda lint`: unknown flag `-{}`", name),
                    );
                }
            }
            Arg::Positional(s) => {
                emit_parse_error(
                    diags,
                    format!("`edda lint`: unexpected positional argument `{}`", s),
                );
            }
            Arg::Separator => break,
        }
    }
    Some(args)
}
