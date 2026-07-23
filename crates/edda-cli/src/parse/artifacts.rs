//! Per-verb parsers for the artifact/cache positional verbs: `promote`,
//! `demote`, `regenerate`, and `structmap`. Each collects positionals via
//! [`super::common::parse_with_positionals`] and validates arity.

use std::iter::Peekable;
use std::slice::Iter;

use edda_diag::Diagnostics;

use crate::cli::{CommonArgs, DemoteArgs, PromoteArgs, RegenerateArgs};
use crate::common_flags::{
    consume_boolean_long, try_consume_common_long, try_consume_common_short,
};
use crate::flags::{Arg, classify, emit_parse_error};

use super::common::{parse_with_positionals, take_single_positional};

/// Parse `edda promote <artifact>` flags.
pub(super) fn parse_promote(
    rest: &mut Peekable<Iter<'_, String>>,
    diags: &mut Diagnostics,
) -> Option<PromoteArgs> {
    let (common, positionals) = parse_with_positionals("edda promote", rest, diags);
    let artifact = take_single_positional("edda promote", "artifact", positionals, diags)?;
    Some(PromoteArgs { common, artifact })
}

/// Parse `edda demote <artifact>` flags.
pub(super) fn parse_demote(
    rest: &mut Peekable<Iter<'_, String>>,
    diags: &mut Diagnostics,
) -> Option<DemoteArgs> {
    let (common, positionals) = parse_with_positionals("edda demote", rest, diags);
    let artifact = take_single_positional("edda demote", "artifact", positionals, diags)?;
    Some(DemoteArgs { common, artifact })
}

/// Parse `edda regenerate <spec>` flags.
pub(super) fn parse_regenerate(
    rest: &mut Peekable<Iter<'_, String>>,
    diags: &mut Diagnostics,
) -> Option<RegenerateArgs> {
    let (common, positionals) = parse_with_positionals("edda regenerate", rest, diags);
    let spec = take_single_positional("edda regenerate", "spec", positionals, diags)?;
    Some(RegenerateArgs { common, spec })
}

/// Parse `edda structmap [<path>] [--check]` flags. `--check` exits
/// non-zero on a stale on-disk structmap rather than rewriting it
/// (codex `06-tooling.md` §8).
pub(super) fn parse_structmap(
    rest: &mut Peekable<Iter<'_, String>>,
    diags: &mut Diagnostics,
) -> Option<crate::cli::StructmapArgs> {
    let mut common = CommonArgs::default();
    let mut positionals: Vec<String> = Vec::new();
    let mut check = false;
    while let Some(raw) = rest.next() {
        match classify(raw) {
            Arg::Long { name, value } => {
                if try_consume_common_long(&mut common, name, value, rest, diags) {
                    continue;
                }
                if name == "check" {
                    consume_boolean_long("--check", value, diags);
                    check = true;
                    continue;
                }
                emit_parse_error(
                    diags,
                    format!("`edda structmap`: unknown flag `--{}`", name),
                );
            }
            Arg::Short { name, attached } => {
                if !try_consume_common_short(&mut common, name, attached, rest, diags) {
                    emit_parse_error(
                        diags,
                        format!("`edda structmap`: unknown flag `-{}`", name),
                    );
                }
            }
            Arg::Positional(s) => positionals.push(s.to_owned()),
            Arg::Separator => break,
        }
    }
    let path = match positionals.len() {
        0 => None,
        1 => Some(std::path::PathBuf::from(&positionals[0])),
        n => {
            emit_parse_error(
                diags,
                format!(
                    "`edda structmap` takes at most one `<path>` argument (got {})",
                    n
                ),
            );
            return None;
        }
    };
    Some(crate::cli::StructmapArgs { common, path, check })
}
