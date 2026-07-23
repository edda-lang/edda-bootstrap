//! LSP server entry point, lifecycle handshake, and main dispatch loop.
//!
//! [`run_stdio`] is the canonical entry: hands the caller stdin/stdout
//! transport, performs the `initialize` / `initialized` handshake, and
//! pumps the [`lsp_server::Connection::receiver`] until `exit` arrives.
//! Each message is matched against a flat method-name table and routed
//! to the matching `crate::handlers` function.

use lsp_server::{Connection, ExtractError, Message, Notification, Request, RequestId, Response};
use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DidSaveTextDocumentParams, InitializeParams, InitializeResult, ServerInfo,
    notification::{
        DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, DidSaveTextDocument,
        Notification as LspNotification,
    },
    request::{
        CodeActionRequest, Completion, HoverRequest, Request as LspRequest,
        SemanticTokensFullRequest,
    },
};
use serde::de::DeserializeOwned;

use edda_daemon::Daemon;
use edda_driver::BuildOptions;

use crate::capabilities::{negotiate_encoding, server_capabilities};
use crate::error::LspError;
use crate::handlers::{
    code_action, completion as completion_handler, hover, semtokens, sync,
};
use crate::state::LspState;

/// Run the LSP server on stdin/stdout, blocking until the client sends
/// `exit`.
///
/// # Errors
///
/// Returns the first transport-level failure (the I/O thread panic, an
/// unparseable message, etc.). Per-request handler failures are
/// projected onto JSON-RPC response errors and do not propagate.
pub fn run_stdio() -> Result<(), ServerError> {
    let (connection, io_threads) = Connection::stdio();
    let result = run_with_connection(&connection);
    drop(connection);
    io_threads.join().map_err(ServerError::Io)?;
    result
}

/// Run the server against an already-constructed connection. Split out
/// so a future test harness can drive the server over an in-memory
/// transport.
fn run_with_connection(connection: &Connection) -> Result<(), ServerError> {
    let (init_id, init_params) = connection
        .initialize_start()
        .map_err(|e| ServerError::Protocol(format!("initialize_start: {e}")))?;
    let init_params: InitializeParams = serde_json::from_value(init_params)
        .map_err(|e| ServerError::Protocol(format!("initialize params: {e}")))?;

    let encoding = negotiate_encoding(
        init_params
            .capabilities
            .general
            .as_ref()
            .and_then(|g| g.position_encodings.as_deref()),
    );
    let capabilities = server_capabilities(encoding);
    let init_result = InitializeResult {
        capabilities,
        server_info: Some(ServerInfo {
            name: "edda-lsp".to_string(),
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }),
    };
    let init_value = serde_json::to_value(init_result)
        .map_err(|e| ServerError::Protocol(format!("initialize encode: {e}")))?;
    connection
        .initialize_finish(init_id, init_value)
        .map_err(|e| ServerError::Protocol(format!("initialize_finish: {e}")))?;

    let daemon = match Daemon::new().open_project_for_root(&init_params) {
        Some(d) => d,
        None => Daemon::new(),
    };

    let state = LspState::new(daemon, encoding);
    main_loop(connection, &state)
}

/// Pump the connection until the client sends `exit`.
fn main_loop(connection: &Connection, state: &LspState) -> Result<(), ServerError> {
    // Bounded loop: terminates on receiver close or `exit` notification.
    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req).unwrap_or(false) {
                    return Ok(());
                }
                handle_request(connection, state, req);
            }
            Message::Notification(notif) => {
                if notif.method == "exit" {
                    return Ok(());
                }
                handle_notification(connection, state, notif);
            }
            Message::Response(_) => {
                // The server does not issue requests to the client in
                // v0.1; any inbound response is unsolicited and dropped.
            }
        }
    }
    Ok(())
}

/// Route a request to its handler and send the response.
fn handle_request(connection: &Connection, state: &LspState, req: Request) {
    let id = req.id.clone();
    match req.method.as_str() {
        SemanticTokensFullRequest::METHOD => {
            dispatch_request::<SemanticTokensFullRequest, _>(connection, &id, req, |params| {
                semtokens::semantic_tokens_full(state, params)
            });
        }
        Completion::METHOD => {
            dispatch_request::<Completion, _>(connection, &id, req, |params| {
                completion_handler::completion(state, params)
            });
        }
        HoverRequest::METHOD => {
            dispatch_request::<HoverRequest, _>(connection, &id, req, |params| {
                hover::hover(state, params)
            });
        }
        CodeActionRequest::METHOD => {
            dispatch_request::<CodeActionRequest, _>(connection, &id, req, |params| {
                code_action::code_action(state, params)
            });
        }
        _ => {
            let err = LspError::InvalidRequest(format!("method not handled: {}", req.method));
            send_error(connection, id, &err);
        }
    }
}

/// Route a notification to its handler.
fn handle_notification(connection: &Connection, state: &LspState, notif: Notification) {
    let result = match notif.method.as_str() {
        DidOpenTextDocument::METHOD => extract::<DidOpenTextDocumentParams>(&notif)
            .and_then(|p| sync::did_open(state, connection, p)),
        DidChangeTextDocument::METHOD => extract::<DidChangeTextDocumentParams>(&notif)
            .and_then(|p| sync::did_change(state, connection, p)),
        DidCloseTextDocument::METHOD => extract::<DidCloseTextDocumentParams>(&notif)
            .and_then(|p| sync::did_close(state, connection, p)),
        DidSaveTextDocument::METHOD => extract::<DidSaveTextDocumentParams>(&notif)
            .and_then(|p| sync::did_save(state, connection, p)),
        _ => Ok(()), // Unknown notifications are dropped per LSP spec.
    };
    if let Err(err) = result {
        log_notification_failure(connection, &notif.method, &err);
    }
}

/// Extract `R::Params` from a request and dispatch through `f`.
fn dispatch_request<R, F>(
    connection: &Connection,
    id: &RequestId,
    req: Request,
    f: F,
) where
    R: LspRequest,
    R::Params: DeserializeOwned,
    R::Result: serde::Serialize,
    F: FnOnce(R::Params) -> Result<R::Result, LspError>,
{
    let params: R::Params = match cast_request::<R>(req) {
        Ok(p) => p,
        Err(err) => {
            send_error(connection, id.clone(), &err);
            return;
        }
    };
    match f(params) {
        Ok(value) => {
            let json = match serde_json::to_value(value) {
                Ok(v) => v,
                Err(e) => {
                    let err = LspError::InvalidRequest(format!("response encode: {e}"));
                    send_error(connection, id.clone(), &err);
                    return;
                }
            };
            let response = Response {
                id: id.clone(),
                result: Some(json),
                error: None,
            };
            if connection.sender.send(Message::Response(response)).is_err() {
                // Channel closed: connection is shutting down.
            }
        }
        Err(err) => send_error(connection, id.clone(), &err),
    }
}

/// Extract a request's params, projecting failure modes onto [`LspError`].
fn cast_request<R>(req: Request) -> Result<R::Params, LspError>
where
    R: LspRequest,
    R::Params: DeserializeOwned,
{
    let method = req.method.clone();
    match req.extract::<R::Params>(R::METHOD) {
        Ok((_, params)) => Ok(params),
        Err(ExtractError::MethodMismatch(_)) => Err(LspError::InvalidRequest(format!(
            "method mismatch: {method}"
        ))),
        Err(ExtractError::JsonError { method: _, error }) => {
            Err(LspError::InvalidRequest(format!("json: {error}")))
        }
    }
}

/// Extract a notification's params with the [`LspError`] projection.
fn extract<P: DeserializeOwned>(notif: &Notification) -> Result<P, LspError> {
    serde_json::from_value::<P>(notif.params.clone())
        .map_err(|e| LspError::InvalidRequest(format!("notification params: {e}")))
}

/// Send a `ResponseError` for the given request id.
fn send_error(connection: &Connection, id: RequestId, err: &LspError) {
    let response = Response {
        id,
        result: None,
        error: Some(err.to_response_error()),
    };
    if connection.sender.send(Message::Response(response)).is_err() {
        // Channel closed.
    }
}

/// Push a `window/logMessage` notification for a notification-handler
/// failure.
fn log_notification_failure(connection: &Connection, method: &str, err: &LspError) {
    let params = lsp_types::LogMessageParams {
        typ: lsp_types::MessageType::ERROR,
        message: format!("{method}: {err}"),
    };
    let notif = Notification::new(
        <lsp_types::notification::LogMessage as LspNotification>::METHOD.to_string(),
        params,
    );
    if connection.sender.send(Message::Notification(notif)).is_err() {
        // Channel closed.
    }
}

/// Top-level failure mode for [`run_stdio`].
#[derive(Debug)]
pub enum ServerError {
    /// One of the connection's I/O threads panicked or returned an error.
    Io(std::io::Error),
    /// The client violated the LSP protocol (e.g. a malformed
    /// `initialize` payload). The server logs this and exits.
    Protocol(String),
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O: {e}"),
            Self::Protocol(s) => write!(f, "protocol: {s}"),
        }
    }
}

impl std::error::Error for ServerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

/// One-shot helper attaching daemon `open_project` to the LSP init
/// payload. Implemented as a free function so [`Daemon`] keeps its
/// transport-agnostic surface.
trait DaemonInitExt {
    fn open_project_for_root(self, params: &InitializeParams) -> Option<Daemon>;
}

impl DaemonInitExt for Daemon {
    fn open_project_for_root(self, params: &InitializeParams) -> Option<Daemon> {
        let root_path = root_path_from_params(params)?;
        let manifest = root_path.join("package.toml");
        let mut options = BuildOptions::for_default_manifest();
        options.manifest_path = manifest;
        match self.open_project(options) {
            Ok(()) => Some(self),
            Err(_) => Some(self), // Keep the (empty) daemon; queries surface NoProjectOpen.
        }
    }
}

/// Decode the LSP workspace root from an `initialize` payload, preferring
/// `workspaceFolders[0]` over the deprecated `rootUri`.
fn root_path_from_params(params: &InitializeParams) -> Option<std::path::PathBuf> {
    let folders = params.workspace_folders.as_ref()?;
    let first = folders.first()?;
    let s = first.uri.as_str();
    let rest = s.strip_prefix("file://")?;
    Some(std::path::PathBuf::from(decode_path(rest)))
}

/// Minimal percent-decode for workspace-root paths. The full decoder
/// lives in `crate::uri`; this is the trimmed inline form used at init
/// time before the daemon is constructed.
fn decode_path(s: &str) -> String {
    let trimmed = if cfg!(windows) {
        s.strip_prefix('/').unwrap_or(s)
    } else {
        s
    };
    trimmed.to_string()
}
