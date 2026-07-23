//! `client.*` route handlers.
//!
//! Handles the handshake, the project lifecycle, the document overlay
//! lifecycle, and `server_info`. Cancellation is dispatched separately
//! (it's a notification, not a request) from
//! [`crate::server::McpServer`].

use std::path::PathBuf;

use serde_json::Value;

use edda_daemon::{Daemon, DaemonError, DocumentVersion};
use edda_driver::{BuildOptions, StructureBudgetMode};

use crate::diagnostic::to_wire_many;
use crate::error::{ErrorClass, McpError};
use crate::handshake::{
    HandshakeParams, HandshakeResult, SessionFeatures, NEGOTIATED_PROTOCOL_VERSION, SERVER_FEATURES,
    SERVER_NAME, SUPPORTED_NAMESPACES, streamable_operations, supported_operations,
};
use crate::params::{ApplyChangeParams, ClosePathParams, OpenDocumentParams, OpenProjectParams};
use crate::result::{
    CloseDocumentResult, DocumentParseResult, ProjectLifecycleResult,
};
use crate::routes::decode_params;
use crate::session::Session;

/// `client.handshake` route.
pub fn handshake(
    session: &mut Session,
    params: Option<Value>,
) -> Result<HandshakeResult, McpError> {
    let p: HandshakeParams = decode_params("client.handshake", params)?;
    if !p.protocol_versions.contains(&NEGOTIATED_PROTOCOL_VERSION) {
        return Err(McpError::new(
            ErrorClass::UnsupportedProtocolVersion,
            format!(
                "client does not announce protocol version {}; server speaks only that version",
                NEGOTIATED_PROTOCOL_VERSION
            ),
        ));
    }
    session.complete_handshake(
        p.client_name,
        p.client_version,
        &p.features,
        &SERVER_FEATURES,
    );
    let features = SessionFeatures::negotiate(&p.features, &SERVER_FEATURES).to_feature_map();
    Ok(HandshakeResult {
        server_name: SERVER_NAME.to_string(),
        server_version: env!("CARGO_PKG_VERSION").to_string(),
        protocol_version: NEGOTIATED_PROTOCOL_VERSION,
        supported_namespaces: SUPPORTED_NAMESPACES.iter().map(|s| s.to_string()).collect(),
        supported_operations: supported_operations(),
        streamable_operations: streamable_operations(),
        features,
    })
}

/// `client.server_info` route.
pub fn server_info() -> HandshakeResult {
    HandshakeResult {
        server_name: SERVER_NAME.to_string(),
        server_version: env!("CARGO_PKG_VERSION").to_string(),
        protocol_version: NEGOTIATED_PROTOCOL_VERSION,
        supported_namespaces: SUPPORTED_NAMESPACES.iter().map(|s| s.to_string()).collect(),
        supported_operations: supported_operations(),
        streamable_operations: streamable_operations(),
        features: SERVER_FEATURES,
    }
}

/// `client.open_project` route.
pub fn open_project(
    daemon: &Daemon,
    params: Option<Value>,
) -> Result<ProjectLifecycleResult, McpError> {
    let p: OpenProjectParams = decode_params("client.open_project", params)?;
    let options = build_options_from_open_params(&p);
    match daemon.open_project(options) {
        Ok(()) => {
            let root = daemon.project_root().map(|p| p.display().to_string());
            // After open, the daemon's cascade diagnostics have
            // been partitioned by file; we surface a flat empty list
            // here. Per-file diagnostics flow through
            // `inspect.diagnostics` once the client knows which files
            // are interesting.
            Ok(ProjectLifecycleResult {
                applied: true,
                project_root: root,
                diagnostics: Vec::new(),
            })
        }
        Err(err) => Err(project_open_error(err)),
    }
}

/// `client.close_project` route.
pub fn close_project(daemon: &Daemon) -> ProjectLifecycleResult {
    daemon.close_project();
    ProjectLifecycleResult {
        applied: true,
        project_root: None,
        diagnostics: Vec::new(),
    }
}

/// `client.open_document` route.
pub fn open_document(
    daemon: &Daemon,
    params: Option<Value>,
) -> Result<DocumentParseResult, McpError> {
    let p: OpenDocumentParams = decode_params("client.open_document", params)?;
    let version = DocumentVersion(p.version);
    match daemon.open_document(&p.path, version, p.text) {
        Ok(parse) => Ok(project_document_result(daemon, parse)),
        Err(err) => Err(document_op_error("open_document", err)),
    }
}

/// `client.apply_change` route.
pub fn apply_change(
    daemon: &Daemon,
    params: Option<Value>,
) -> Result<DocumentParseResult, McpError> {
    let p: ApplyChangeParams = decode_params("client.apply_change", params)?;
    let version = DocumentVersion(p.version);
    match daemon.apply_change(&p.path, version, p.new_text) {
        Ok(parse) => Ok(project_document_result(daemon, parse)),
        Err(err) => Err(document_op_error("apply_change", err)),
    }
}

/// `client.close_document` route.
pub fn close_document(
    daemon: &Daemon,
    params: Option<Value>,
) -> Result<CloseDocumentResult, McpError> {
    let p: ClosePathParams = decode_params("client.close_document", params)?;
    match daemon.close_document(&p.path) {
        Ok(()) => Ok(CloseDocumentResult { applied: true }),
        Err(err) => Err(document_op_error("close_document", err)),
    }
}

fn project_document_result(
    daemon: &Daemon,
    parse: edda_daemon::DocumentParseResult,
) -> DocumentParseResult {
    // Render diagnostics under the daemon's source-map borrow; the
    // accessor releases the read lock before this function returns.
    let diagnostics = daemon
        .with_source_map(|map| to_wire_many(map, &parse.diagnostics))
        .unwrap_or_default();
    DocumentParseResult {
        version: parse.version.0,
        diagnostics,
    }
}

fn build_options_from_open_params(p: &OpenProjectParams) -> BuildOptions {
    let manifest_path = p
        .manifest_path
        .clone()
        .unwrap_or_else(|| default_manifest_in(&p.project_root));
    BuildOptions {
        manifest_path,
        target_override: p.target.clone(),
        feature_override: p.features.clone(),
        profile_override: p.profile.clone(),
        full_materialization: false,
        jobs: None,
        warn_as_error: Vec::new(),
        properties: false,
        structure_budget: StructureBudgetMode::default(),
        freestanding: false,
        structmap_check: false,
        lint_trust_points: false,
        lint_capability_safe_stdlib: false,
    }
}

fn default_manifest_in(project_root: &std::path::Path) -> PathBuf {
    project_root.join("package.toml")
}

fn project_open_error(err: DaemonError) -> McpError {
    match err {
        DaemonError::ProjectAlreadyOpen => McpError::new(
            ErrorClass::ProjectAlreadyOpen,
            "a project is already open in this daemon",
        ),
        DaemonError::DriverInit(init) => McpError::new(
            ErrorClass::DriverInit,
            format!("driver init failed: {init}"),
        ),
        DaemonError::CascadeFailed {
            exit_code,
            diagnostics,
        } => McpError::new(
            ErrorClass::CascadeFailed,
            format!(
                "cascade exit {} with {} diagnostics ({} errors)",
                exit_code.as_i32(),
                diagnostics.len(),
                diagnostics.error_count(),
            ),
        ),
        other => McpError::new(
            ErrorClass::DriverInit,
            format!("unexpected daemon error during open_project: {other}"),
        ),
    }
}

fn document_op_error(op: &str, err: DaemonError) -> McpError {
    let class = match err {
        DaemonError::NoProjectOpen => ErrorClass::NoProjectOpen,
        DaemonError::DocumentAlreadyOpen { .. }
        | DaemonError::DocumentNotOpen { .. }
        | DaemonError::DocumentVersionStale { .. } => ErrorClass::ArgShapeInvalid,
        _ => ErrorClass::DriverInit,
    };
    McpError::new(class, format!("{op}: {err}"))
}
