//! Per-verb parser for `edda hot [member] [-- <args>]`.

use std::iter::Peekable;
use std::slice::Iter;

use edda_diag::Diagnostics;

use crate::cli::HotArgs;
use crate::flags::emit_parse_error;

use super::common::parse_with_positionals;

/// Parse `edda hot` flags: the common flag set, an optional `<member>`
/// positional, and a `--`-separated tail forwarded to the supervised
/// child on every (re)spawn.
pub(super) fn parse_hot(
    rest: &mut Peekable<Iter<'_, String>>,
    diags: &mut Diagnostics,
) -> Option<HotArgs> {
    let (common, mut positionals) = parse_with_positionals("edda hot", rest, diags);
    let member = match positionals.len() {
        0 => None,
        1 => Some(positionals.remove(0)),
        n => {
            emit_parse_error(
                diags,
                format!("`edda hot` takes at most one `<member>` argument (got {n})"),
            );
            return None;
        }
    };
    let child_args: Vec<String> = rest.cloned().collect();
    Some(HotArgs {
        common,
        member,
        child_args,
    })
}
