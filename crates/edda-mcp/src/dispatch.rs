//! Per-namespace method dispatchers.
//!
//! [`dispatch`] walks the seven `mcp-protocol.md` §4 namespaces in
//! order and returns the first match, projecting each route's typed
//! result into a [`serde_json::Value`]. Splitting one dispatcher per
//! namespace keeps every helper under the 80-line function cap while
//! preserving the exhaustive method-name → handler mapping.

use serde_json::Value;

use edda_daemon::Daemon;

use crate::error::{ErrorClass, McpError};
use crate::methods;
use crate::routes;
use crate::session::Session;
use crate::wire::Request;

/// Top-level method dispatcher.
///
/// Walks the seven namespace dispatchers in §4 declaration order and
/// returns the first match. Unknown methods produce
/// [`ErrorClass::MethodNotImplemented`] — the JSON-RPC standard
/// `MethodNotFound` integer code is derived from the class via
/// [`ErrorClass::code`].
pub(crate) fn dispatch(
    daemon: &Daemon,
    session: &mut Session,
    req: &Request,
) -> Result<Value, McpError> {
    let method = req.method.as_str();
    let params = req.params.clone();
    if let Some(r) = dispatch_client(method, daemon, session, params.clone()) {
        return r;
    }
    if let Some(r) = dispatch_build(method, params.clone()) {
        return r;
    }
    if let Some(r) = dispatch_codegen(method, params.clone()) {
        return r;
    }
    if let Some(r) = dispatch_inspect(method, daemon, params.clone()) {
        return r;
    }
    if let Some(r) = dispatch_edit(method, params.clone()) {
        return r;
    }
    if let Some(r) = dispatch_typecheck(method, params.clone()) {
        return r;
    }
    if let Some(r) = dispatch_layout(method, params) {
        return r;
    }
    Err(McpError::new(
        ErrorClass::MethodNotImplemented,
        format!("unknown method: {method}"),
    ))
}

fn dispatch_client(
    method: &str,
    daemon: &Daemon,
    session: &mut Session,
    params: Option<Value>,
) -> Option<Result<Value, McpError>> {
    Some(match method {
        methods::client::HANDSHAKE => {
            routes::client_ops::handshake(session, params).and_then(|r| value_of(&r))
        }
        methods::client::SERVER_INFO => value_of(&routes::client_ops::server_info()),
        methods::client::OPEN_PROJECT => {
            routes::client_ops::open_project(daemon, params).and_then(|r| value_of(&r))
        }
        methods::client::CLOSE_PROJECT => value_of(&routes::client_ops::close_project(daemon)),
        methods::client::OPEN_DOCUMENT => {
            routes::client_ops::open_document(daemon, params).and_then(|r| value_of(&r))
        }
        methods::client::APPLY_CHANGE => {
            routes::client_ops::apply_change(daemon, params).and_then(|r| value_of(&r))
        }
        methods::client::CLOSE_DOCUMENT => {
            routes::client_ops::close_document(daemon, params).and_then(|r| value_of(&r))
        }
        methods::client::CANCEL => Err(McpError::new(
            ErrorClass::ArgShapeInvalid,
            "client.cancel is a notification, not a request",
        )),
        _ => return None,
    })
}

fn dispatch_build(method: &str, params: Option<Value>) -> Option<Result<Value, McpError>> {
    Some(match method {
        methods::build::COMPILE => routes::build::compile(params),
        methods::build::TYPECHECK => routes::build::typecheck(params),
        methods::build::RUN => routes::build::run(params),
        methods::build::TEST => routes::build::test(params),
        methods::build::BENCH => routes::build::bench(params),
        methods::build::FORMAT => routes::build::format(params),
        methods::build::LINT => routes::build::lint(params),
        methods::build::CLEAN => routes::build::clean(params),
        _ => return None,
    })
}

fn dispatch_codegen(method: &str, params: Option<Value>) -> Option<Result<Value, McpError>> {
    Some(match method {
        methods::codegen::PROMOTE => routes::codegen::promote(params),
        methods::codegen::DEMOTE => routes::codegen::demote(params),
        methods::codegen::REGENERATE => routes::codegen::regenerate(params),
        methods::codegen::GC => routes::codegen::gc(params),
        methods::codegen::FULL_HASH => routes::codegen::full_hash(params),
        _ => return None,
    })
}

fn dispatch_inspect(
    method: &str,
    daemon: &Daemon,
    params: Option<Value>,
) -> Option<Result<Value, McpError>> {
    Some(match method {
        methods::inspect::PARSED_AST => {
            routes::inspect::parsed_ast(daemon, params).and_then(|r| value_of(&r))
        }
        methods::inspect::DIAGNOSTICS => {
            routes::inspect::diagnostics(daemon, params).and_then(|r| value_of(&r))
        }
        methods::inspect::ARTIFACT_OF_INVOCATION => routes::inspect::artifact_of_invocation(params),
        methods::inspect::ARTIFACT_OF_NAME => routes::inspect::artifact_of_name(params),
        methods::inspect::ARTIFACT_OF_SPEC_BODY_ITEM => {
            routes::inspect::artifact_of_spec_body_item(params)
        }
        methods::inspect::SOURCE_OF_ARTIFACT => routes::inspect::source_of_artifact(params),
        methods::inspect::SOURCE_OF_ARTIFACT_ITEM => {
            routes::inspect::source_of_artifact_item(params)
        }
        methods::inspect::INVOCATION_SITES_OF_ARTIFACT => {
            routes::inspect::invocation_sites_of_artifact(params)
        }
        methods::inspect::NESTED_DEPS => routes::inspect::nested_deps(params),
        methods::inspect::TRANSITIVE_DEPS => routes::inspect::transitive_deps(params),
        methods::inspect::DIRECT_CONSUMERS => routes::inspect::direct_consumers(params),
        methods::inspect::TRANSITIVE_CONSUMERS => routes::inspect::transitive_consumers(params),
        methods::inspect::LIVE_ARTIFACTS => routes::inspect::live_artifacts(params),
        methods::inspect::STALE_ARTIFACTS => routes::inspect::stale_artifacts(params),
        methods::inspect::GC_ELIGIBLE_ARTIFACTS => routes::inspect::gc_eligible_artifacts(params),
        methods::inspect::BODY_DIFF => routes::inspect::body_diff(params),
        methods::inspect::CASCADE_FROM_EDIT => routes::inspect::cascade_from_edit(params),
        _ => return None,
    })
}

fn dispatch_edit(method: &str, params: Option<Value>) -> Option<Result<Value, McpError>> {
    Some(match method {
        methods::edit::TRANSACTION => routes::edit::transaction(params),
        methods::edit::DECLARATION_RENAME => routes::edit::declaration_rename(params),
        methods::edit::SIGNATURE_PARAMETER_ADD => routes::edit::signature_parameter_add(params),
        methods::edit::SIGNATURE_PARAMETER_REMOVE => {
            routes::edit::signature_parameter_remove(params)
        }
        methods::edit::SIGNATURE_RETURN_TYPE_SET => routes::edit::signature_return_type_set(params),
        methods::edit::EFFECT_ROW_ADD => routes::edit::effect_row_add(params),
        methods::edit::EFFECT_ROW_REMOVE => routes::edit::effect_row_remove(params),
        methods::edit::REFACTOR_RENAME_WITH_CASCADE => {
            routes::edit::refactor_rename_with_cascade(params)
        }
        methods::edit::REFACTOR_EXTRACT_FUNCTION => routes::edit::refactor_extract_function(params),
        methods::edit::REFACTOR_INLINE_FUNCTION => routes::edit::refactor_inline_function(params),
        _ => return None,
    })
}

fn dispatch_typecheck(method: &str, params: Option<Value>) -> Option<Result<Value, McpError>> {
    Some(match method {
        methods::typecheck::TYPE_AT => routes::typecheck::type_at(params),
        methods::typecheck::MODE_AT => routes::typecheck::mode_at(params),
        methods::typecheck::EFFECT_ROW_AT => routes::typecheck::effect_row_at(params),
        methods::typecheck::REFINEMENT_OBLIGATIONS_AT => {
            routes::typecheck::refinement_obligations_at(params)
        }
        methods::typecheck::TRUST_POINTS_IN_SCOPE => {
            routes::typecheck::trust_points_in_scope(params)
        }
        methods::typecheck::COMPTIME_PURE_STATUS => routes::typecheck::comptime_pure_status(params),
        methods::typecheck::DISCHARGED_REFINEMENTS => {
            routes::typecheck::discharged_refinements(params)
        }
        _ => return None,
    })
}

fn dispatch_layout(method: &str, params: Option<Value>) -> Option<Result<Value, McpError>> {
    Some(match method {
        methods::layout::SIZE_OF => routes::layout::size_of(params),
        methods::layout::ALIGN_OF => routes::layout::align_of(params),
        methods::layout::OFFSET_OF => routes::layout::offset_of(params),
        methods::layout::ATTRIBUTES_OF => routes::layout::attributes_of(params),
        methods::layout::REPR_OF => routes::layout::repr_of(params),
        methods::layout::FIELD_LAYOUT => routes::layout::field_layout(params),
        methods::layout::ABI_OF => routes::layout::abi_of(params),
        _ => return None,
    })
}

/// Serialise a typed handler result into a `serde_json::Value`.
pub(crate) fn value_of<T: serde::Serialize>(v: &T) -> Result<Value, McpError> {
    serde_json::to_value(v).map_err(|err| {
        McpError::new(
            ErrorClass::ArgShapeInvalid,
            format!("internal serialisation failure: {err}"),
        )
    })
}
