//! Per-verb parsers for the Mímir package-management verbs: `add`,
//! `update`, `audit`, `contract-diff`, `why`, and `key` (with its nested
//! `generate` subcommand). Per §8 of the codex `08-packages.md`.

use std::iter::Peekable;
use std::slice::Iter;

use edda_diag::Diagnostics;

use crate::cli::{
    AddArgs, AuditArgs, CommonArgs, ContractDiffArgs, KeyArgs, KeyGenerateArgs, KeySubcommand,
    UpdateArgs, WhyArgs,
};
use crate::common_flags::{
    consume_boolean_long, try_consume_common_long, try_consume_common_short,
};
use crate::flags::{Arg, classify, emit_parse_error};

use super::common::{parse_with_positionals, take_single_positional};

/// Parse `edda add <name>[@<version>]` flags (§8.1).
pub(super) fn parse_add(
    rest: &mut Peekable<Iter<'_, String>>,
    diags: &mut Diagnostics,
) -> Option<AddArgs> {
    let mut args = AddArgs::default();
    let mut positionals = Vec::new();
    while let Some(raw) = rest.next() {
        match classify(raw) {
            Arg::Long { name, value } => {
                if try_consume_common_long(&mut args.common, name, value, rest, diags) {
                    continue;
                }
                match name {
                    "accept-unstable" => {
                        consume_boolean_long("--accept-unstable", value, diags);
                        args.accept_unstable = true;
                    }
                    "max-effects" => {
                        if let Some(v) = crate::flags::take_value(
                            "max-effects",
                            value,
                            rest,
                            diags,
                        ) {
                            args.max_effects.push(v.to_owned());
                        }
                    }
                    _ => emit_parse_error(
                        diags,
                        format!("`edda add`: unknown flag `--{}`", name),
                    ),
                }
            }
            Arg::Short { name, attached } => {
                if !try_consume_common_short(&mut args.common, name, attached, rest, diags) {
                    emit_parse_error(
                        diags,
                        format!("`edda add`: unknown flag `-{}`", name),
                    );
                }
            }
            Arg::Positional(s) => positionals.push(s.to_owned()),
            Arg::Separator => break,
        }
    }
    args.name = take_single_positional("edda add", "name[@version]", positionals, diags)?;
    Some(args)
}

/// Parse `edda update [<name>]` flags (§8.2).
pub(super) fn parse_update(
    rest: &mut Peekable<Iter<'_, String>>,
    diags: &mut Diagnostics,
) -> Option<UpdateArgs> {
    let mut args = UpdateArgs::default();
    let mut positionals = Vec::new();
    while let Some(raw) = rest.next() {
        match classify(raw) {
            Arg::Long { name, value } => {
                if try_consume_common_long(&mut args.common, name, value, rest, diags) {
                    continue;
                }
                match name {
                    "accept-surface-change" => {
                        if let Some(v) = crate::flags::take_value(
                            "accept-surface-change",
                            value,
                            rest,
                            diags,
                        ) {
                            args.accept_surface_change.push(v.to_owned());
                        }
                    }
                    "accept-publisher-rotation" => {
                        if let Some(v) = crate::flags::take_value(
                            "accept-publisher-rotation",
                            value,
                            rest,
                            diags,
                        ) {
                            args.accept_publisher_rotation.push(v.to_owned());
                        }
                    }
                    _ => emit_parse_error(
                        diags,
                        format!("`edda update`: unknown flag `--{}`", name),
                    ),
                }
            }
            Arg::Short { name, attached } => {
                if !try_consume_common_short(&mut args.common, name, attached, rest, diags) {
                    emit_parse_error(
                        diags,
                        format!("`edda update`: unknown flag `-{}`", name),
                    );
                }
            }
            Arg::Positional(s) => positionals.push(s.to_owned()),
            Arg::Separator => break,
        }
    }
    match positionals.len() {
        0 => args.name = None,
        1 => args.name = Some(positionals.remove(0)),
        n => {
            emit_parse_error(
                diags,
                format!("`edda update` takes at most one `<name>` argument (got {})", n),
            );
            return None;
        }
    }
    Some(args)
}

/// Parse `edda audit` flags (§8.3).
pub(super) fn parse_audit(
    rest: &mut Peekable<Iter<'_, String>>,
    diags: &mut Diagnostics,
) -> Option<AuditArgs> {
    let mut args = AuditArgs::default();
    while let Some(raw) = rest.next() {
        match classify(raw) {
            Arg::Long { name, value } => {
                if try_consume_common_long(&mut args.common, name, value, rest, diags) {
                    continue;
                }
                if name == "quiet" {
                    consume_boolean_long("--quiet", value, diags);
                    args.quiet = true;
                    continue;
                }
                emit_parse_error(
                    diags,
                    format!("`edda audit`: unknown flag `--{}`", name),
                );
            }
            Arg::Short { name, attached } => {
                if !try_consume_common_short(&mut args.common, name, attached, rest, diags) {
                    emit_parse_error(
                        diags,
                        format!("`edda audit`: unknown flag `-{}`", name),
                    );
                }
            }
            Arg::Positional(s) => {
                emit_parse_error(
                    diags,
                    format!("`edda audit`: unexpected positional argument `{}`", s),
                );
            }
            Arg::Separator => break,
        }
    }
    Some(args)
}

/// Parse `edda contract-diff <a> <b>` flags (§8.5).
pub(super) fn parse_contract_diff(
    rest: &mut Peekable<Iter<'_, String>>,
    diags: &mut Diagnostics,
) -> Option<ContractDiffArgs> {
    let (common, positionals) = parse_with_positionals("edda contract-diff", rest, diags);
    if positionals.len() != 2 {
        emit_parse_error(
            diags,
            format!(
                "`edda contract-diff` requires exactly two arguments `<a>` and `<b>` (got {})",
                positionals.len()
            ),
        );
        return None;
    }
    let mut it = positionals.into_iter();
    let a = it.next().unwrap();
    let b = it.next().unwrap();
    Some(ContractDiffArgs { common, a, b })
}

/// Parse `edda why <name>` flags (§8.6).
pub(super) fn parse_why(
    rest: &mut Peekable<Iter<'_, String>>,
    diags: &mut Diagnostics,
) -> Option<WhyArgs> {
    let (common, positionals) = parse_with_positionals("edda why", rest, diags);
    let name = take_single_positional("edda why", "name", positionals, diags)?;
    Some(WhyArgs { common, name })
}

/// Parse `edda key <subcommand> [flags]`.
///
/// The first remaining argv token names the subcommand (`generate`
/// today). Per-subcommand flag parsing is delegated to a helper. The
/// `key` verb does not accept common flags before the subcommand —
/// keeping it strictly `edda key <subcommand>` matches the codex
/// convention for nested verbs.
pub(super) fn parse_key(
    rest: &mut Peekable<Iter<'_, String>>,
    diags: &mut Diagnostics,
) -> Option<KeyArgs> {
    let Some(sub_token) = rest.next() else {
        emit_parse_error(
            diags,
            "`edda key`: missing subcommand (expected one of: generate)",
        );
        return None;
    };
    let sub_name = sub_token.as_str();
    let common = CommonArgs::default();
    match sub_name {
        "generate" => {
            let gen_args = parse_key_generate(rest, diags)?;
            Some(KeyArgs {
                common,
                subcommand: KeySubcommand::Generate(gen_args),
            })
        }
        other => {
            emit_parse_error(
                diags,
                format!(
                    "`edda key`: unknown subcommand `{}` (expected one of: generate)",
                    other
                ),
            );
            None
        }
    }
}

/// Parse `edda key generate [--keystore <dir>] [--label <name>]` flags.
fn parse_key_generate(
    rest: &mut Peekable<Iter<'_, String>>,
    diags: &mut Diagnostics,
) -> Option<KeyGenerateArgs> {
    let mut args = KeyGenerateArgs::default();
    while let Some(raw) = rest.next() {
        match classify(raw) {
            Arg::Long { name, value } => match name {
                "keystore" => {
                    if let Some(v) = crate::flags::take_value("keystore", value, rest, diags) {
                        args.keystore = Some(std::path::PathBuf::from(v));
                    }
                }
                "label" => {
                    if let Some(v) = crate::flags::take_value("label", value, rest, diags) {
                        args.label = Some(v.to_owned());
                    }
                }
                _ => emit_parse_error(
                    diags,
                    format!("`edda key generate`: unknown flag `--{}`", name),
                ),
            },
            Arg::Short { name, .. } => {
                emit_parse_error(
                    diags,
                    format!("`edda key generate`: unknown flag `-{}`", name),
                );
            }
            Arg::Positional(s) => {
                emit_parse_error(
                    diags,
                    format!(
                        "`edda key generate`: unexpected positional argument `{}`",
                        s
                    ),
                );
            }
            Arg::Separator => break,
        }
    }
    Some(args)
}
