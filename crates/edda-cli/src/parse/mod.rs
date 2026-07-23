//! `argv` → [`Cli`] driver. Walks the user's command line through
//! [`crate::flags`]'s tokenizer, dispatches into a per-verb parser, and
//! surfaces malformed input as `parse_error` diagnostics at
//! [`edda_span::Span::DUMMY`] (no file is open at argv-parse time).

mod artifacts;
mod build_pipeline;
mod common;
mod hot;
mod mimir;
mod run;

use edda_diag::Diagnostics;

use crate::cli::{BenchArgs, CleanArgs, Cli, DaemonArgs, PublishArgs, Verb};
use crate::flags::emit_parse_error;

use self::artifacts::{parse_demote, parse_promote, parse_regenerate, parse_structmap};
use self::build_pipeline::{parse_build, parse_check, parse_fmt, parse_gc, parse_lint, parse_test};
use self::common::parse_common_only;
use self::hot::parse_hot;
use self::run::parse_run;
use self::mimir::{
    parse_add, parse_audit, parse_contract_diff, parse_key, parse_update, parse_why,
};

/// Parse a full argv (`argv[0]` is the program name and is ignored).
///
/// Returns `None` if argv is empty (no verb given), if the verb is
/// unknown, or if a verb-required positional argument is missing.
/// Soft errors (unknown flags, malformed values) populate `diags`
/// but the parse continues so multiple errors are surfaced at once.
pub fn parse_args(argv: &[String], diags: &mut Diagnostics) -> Option<Cli> {
    if argv.len() < 2 {
        emit_parse_error(
            diags,
            "edda: missing subcommand (expected one of build, check, run, hot, test, \
             bench, gc, promote, demote, regenerate, clean, fmt, lint, daemon, \
             add, update, audit, publish, contract-diff, why, key)",
        );
        return None;
    }
    let verb_name = argv[1].as_str();
    let mut rest = argv[2..].iter().peekable();
    let verb = match verb_name {
        "build" => Verb::Build(parse_build(&mut rest, diags)?),
        "check" => Verb::Check(parse_check(&mut rest, diags)?),
        "run" => Verb::Run(parse_run(&mut rest, diags)?),
        "hot" => Verb::Hot(parse_hot(&mut rest, diags)?),
        "test" => Verb::Test(parse_test(&mut rest, diags)?),
        "bench" => Verb::Bench(BenchArgs {
            common: parse_common_only("edda bench", &mut rest, diags)?,
        }),
        "gc" => Verb::Gc(parse_gc(&mut rest, diags)?),
        "promote" => Verb::Promote(parse_promote(&mut rest, diags)?),
        "demote" => Verb::Demote(parse_demote(&mut rest, diags)?),
        "regenerate" => Verb::Regenerate(parse_regenerate(&mut rest, diags)?),
        "clean" => Verb::Clean(CleanArgs {
            common: parse_common_only("edda clean", &mut rest, diags)?,
        }),
        "fmt" => Verb::Fmt(parse_fmt(&mut rest, diags)?),
        "lint" => Verb::Lint(parse_lint(&mut rest, diags)?),
        "daemon" => Verb::Daemon(DaemonArgs {
            common: parse_common_only("edda daemon", &mut rest, diags)?,
        }),
        "structmap" => Verb::Structmap(parse_structmap(&mut rest, diags)?),
        "add" => Verb::Add(parse_add(&mut rest, diags)?),
        "update" => Verb::Update(parse_update(&mut rest, diags)?),
        "audit" => Verb::Audit(parse_audit(&mut rest, diags)?),
        "publish" => Verb::Publish(PublishArgs {
            common: parse_common_only("edda publish", &mut rest, diags)?,
        }),
        "contract-diff" => Verb::ContractDiff(parse_contract_diff(&mut rest, diags)?),
        "why" => Verb::Why(parse_why(&mut rest, diags)?),
        "key" => Verb::Key(parse_key(&mut rest, diags)?),
        other => {
            emit_parse_error(
                diags,
                format!(
                    "edda: unknown subcommand `{}` (expected one of build, check, \
                     run, hot, test, bench, gc, promote, demote, regenerate, clean, fmt, \
                     lint, daemon, structmap, add, update, audit, publish, \
                     contract-diff, why, key)",
                    other
                ),
            );
            return None;
        }
    };
    Some(Cli { verb })
}
