//! `inspect.*` route handlers.
//!
//! This module wires the two queries the daemon already implements:
//! [`edda_daemon::query::parsed_ast_for_file`] and
//! [`edda_daemon::query::diagnostics_for_file`]. Every other leaf
//! returns `method_not_implemented` until the daemon's persistent
//! index lands (see `edda-daemon`'s plan).

use serde_json::Value;

use edda_daemon::{Daemon, DaemonError};

use crate::diagnostic::to_wire_many;
use crate::error::{ErrorClass, McpError};
use crate::methods;
use crate::params::FileQueryParams;
use crate::result::{DiagnosticsResult, ParsedAstResult};
use crate::routes::{decode_params, not_implemented};

/// `inspect.parsed_ast` route — wraps
/// [`edda_daemon::query::parsed_ast_for_file`].
pub fn parsed_ast(daemon: &Daemon, params: Option<Value>) -> Result<ParsedAstResult, McpError> {
    let p: FileQueryParams = decode_params(methods::inspect::PARSED_AST, params)?;
    match edda_daemon::query::parsed_ast_for_file(daemon, &p.file) {
        Ok(Some(ast)) => Ok(ParsedAstResult {
            available: true,
            top_level_items: Some(ast.items.len()),
        }),
        Ok(None) => Ok(ParsedAstResult {
            available: false,
            top_level_items: None,
        }),
        Err(err) => Err(daemon_query_error("inspect.parsed_ast", err)),
    }
}

/// `inspect.diagnostics` route — wraps
/// [`edda_daemon::query::diagnostics_for_file`].
pub fn diagnostics(daemon: &Daemon, params: Option<Value>) -> Result<DiagnosticsResult, McpError> {
    let p: FileQueryParams = decode_params(methods::inspect::DIAGNOSTICS, params)?;
    match edda_daemon::query::diagnostics_for_file(daemon, &p.file) {
        Ok(diags) => {
            let rendered = daemon
                .with_source_map(|map| to_wire_many(map, &diags))
                .map_err(|err| daemon_query_error("inspect.diagnostics", err))?;
            Ok(DiagnosticsResult {
                diagnostics: rendered,
            })
        }
        Err(err) => Err(daemon_query_error("inspect.diagnostics", err)),
    }
}

/// `inspect.artifact_of_invocation` route.
pub fn artifact_of_invocation(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::inspect::ARTIFACT_OF_INVOCATION))
}

/// `inspect.artifact_of_name` route.
pub fn artifact_of_name(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::inspect::ARTIFACT_OF_NAME))
}

/// `inspect.artifact_of_spec_body_item` route.
pub fn artifact_of_spec_body_item(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::inspect::ARTIFACT_OF_SPEC_BODY_ITEM))
}

/// `inspect.source_of_artifact` route.
pub fn source_of_artifact(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::inspect::SOURCE_OF_ARTIFACT))
}

/// `inspect.source_of_artifact_item` route.
pub fn source_of_artifact_item(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::inspect::SOURCE_OF_ARTIFACT_ITEM))
}

/// `inspect.invocation_sites_of_artifact` route.
pub fn invocation_sites_of_artifact(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(
        methods::inspect::INVOCATION_SITES_OF_ARTIFACT,
    ))
}

/// `inspect.nested_deps` route.
pub fn nested_deps(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::inspect::NESTED_DEPS))
}

/// `inspect.transitive_deps` route.
pub fn transitive_deps(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::inspect::TRANSITIVE_DEPS))
}

/// `inspect.direct_consumers` route.
pub fn direct_consumers(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::inspect::DIRECT_CONSUMERS))
}

/// `inspect.transitive_consumers` route.
pub fn transitive_consumers(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::inspect::TRANSITIVE_CONSUMERS))
}

/// `inspect.live_artifacts` route.
pub fn live_artifacts(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::inspect::LIVE_ARTIFACTS))
}

/// `inspect.stale_artifacts` route.
pub fn stale_artifacts(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::inspect::STALE_ARTIFACTS))
}

/// `inspect.gc_eligible_artifacts` route.
pub fn gc_eligible_artifacts(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::inspect::GC_ELIGIBLE_ARTIFACTS))
}

/// `inspect.body_diff` route.
pub fn body_diff(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::inspect::BODY_DIFF))
}

/// `inspect.cascade_from_edit` route.
pub fn cascade_from_edit(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::inspect::CASCADE_FROM_EDIT))
}

fn daemon_query_error(op: &str, err: DaemonError) -> McpError {
    let class = match err {
        DaemonError::NoProjectOpen => ErrorClass::NoProjectOpen,
        _ => ErrorClass::DriverInit,
    };
    McpError::new(class, format!("{op}: {err}"))
}
