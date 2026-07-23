//! Per-verb parser for `edda run [member]`.

use std::iter::Peekable;
use std::slice::Iter;

use edda_diag::Diagnostics;

use crate::cli::RunArgs;
use crate::flags::emit_parse_error;

use super::common::parse_with_positionals;

/// Parse `edda run` flags: the common flag set plus an optional
/// `<member>` positional selecting the workspace member to build and run.
pub(super) fn parse_run(
    rest: &mut Peekable<Iter<'_, String>>,
    diags: &mut Diagnostics,
) -> Option<RunArgs> {
    let (common, mut positionals) = parse_with_positionals("edda run", rest, diags);
    let member = match positionals.len() {
        0 => None,
        1 => Some(positionals.remove(0)),
        n => {
            emit_parse_error(
                diags,
                format!("`edda run` takes at most one `<member>` argument (got {n})"),
            );
            return None;
        }
    };
    Some(RunArgs { common, member })
}
