//! [`McpServer`] — JSON-RPC dispatcher over a [`Daemon`] and a
//! [`Session`].
//!
//! # Lifecycle
//!
//! Construct one [`McpServer`] per process. The server owns an
//! [`edda_daemon::Daemon`] (shared across connections) and a
//! per-connection [`Session`]. The carrier thread calls
//! [`McpServer::serve`] with a [`Transport`]; `serve` loops on
//! `transport.recv_line()`, dispatches each envelope to the right
//! route, and writes the response back.
//!
//! # Dispatch
//!
//! - **Request** → route lookup by `method` → typed
//!   `serde_json::Value` → handler returns
//!   `Result<serde_json::Value, McpError>` → wire `Response`.
//! - **Notification** → `client.cancel` updates the session's
//!   in-flight set; every other notification is silently dropped
//!   (per `mcp-protocol.md` §2 notifications have no response).
//! - **Decode failure** at the envelope layer → `InvalidRequest` /
//!   `ParseError` response.

use serde_json::Value;

use edda_daemon::Daemon;

use crate::dispatch;
use crate::error::{ErrorClass, ErrorCode, McpError};
use crate::methods;
use crate::session::Session;
use crate::transport::{Transport, TransportError};
use crate::wire::{ErrorObject, Id, Notification, Request, Response, Version};

const MAX_SERVE_ITERATIONS: u64 = u64::MAX;

/// The MCP server.
pub struct McpServer<'d> {
    daemon: &'d Daemon,
    session: Session,
}

/// Static server identity.
#[derive(Clone, Debug)]
pub struct ServerInfo {
    /// Always `"edda-daemon"`.
    pub name: &'static str,
    /// Crate semver from `CARGO_PKG_VERSION`.
    pub version: &'static str,
    /// Negotiated protocol version (1 in this build).
    pub protocol_version: u32,
}

impl ServerInfo {
    /// Construct from the build-time constants.
    pub const fn from_build() -> Self {
        Self {
            name: crate::handshake::SERVER_NAME,
            version: env!("CARGO_PKG_VERSION"),
            protocol_version: crate::handshake::NEGOTIATED_PROTOCOL_VERSION,
        }
    }
}

impl<'d> McpServer<'d> {
    /// Construct a server bound to the given daemon.
    pub fn new(daemon: &'d Daemon) -> Self {
        Self {
            daemon,
            session: Session::new(),
        }
    }

    /// Borrow the per-connection session for inspection (tests, observability).
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Run the dispatch loop until the transport reports EOF.
    pub fn serve<T: Transport>(&mut self, transport: &mut T) -> Result<(), TransportError> {
        for _ in 0..MAX_SERVE_ITERATIONS {
            match transport.recv_line() {
                Ok(line) => {
                    if line.trim().is_empty() {
                        continue;
                    }
                    self.handle_line(&line, transport)?;
                }
                Err(TransportError::Eof) => return Ok(()),
                Err(err) => return Err(err),
            }
        }
        Ok(())
    }

    fn handle_line<T: Transport>(
        &mut self,
        line: &str,
        transport: &mut T,
    ) -> Result<(), TransportError> {
        // Sniff the envelope kind. Try request first, then notification.
        match serde_json::from_str::<Value>(line) {
            Ok(value) => self.dispatch_value(value, transport),
            Err(err) => {
                let resp = parse_error_response(err);
                transport.send_response(&resp)
            }
        }
    }

    fn dispatch_value<T: Transport>(
        &mut self,
        value: Value,
        transport: &mut T,
    ) -> Result<(), TransportError> {
        let has_id = value.get("id").is_some();
        if has_id {
            self.dispatch_request(value, transport)
        } else {
            self.dispatch_notification(value);
            Ok(())
        }
    }

    fn dispatch_request<T: Transport>(
        &mut self,
        value: Value,
        transport: &mut T,
    ) -> Result<(), TransportError> {
        let req = match serde_json::from_value::<Request>(value) {
            Ok(r) => r,
            Err(err) => {
                let resp = invalid_request_response(err);
                return transport.send_response(&resp);
            }
        };
        let id = req.id.clone();
        self.session.enter_request(&id);
        let result = self.route(&req);
        let response = match result {
            Ok(value) => Response::success(id.clone(), value),
            Err(err) => Response::failure(id.clone(), err.into_error_object()),
        };
        self.session.exit_request(&id);
        transport.send_response(&response)
    }

    fn dispatch_notification(&mut self, value: Value) {
        // Try to decode just the method to decide what to do.
        let method = match value.get("method").and_then(|v| v.as_str()) {
            Some(m) => m,
            None => return,
        };
        if method == methods::client::CANCEL {
            // Pull `params.request_id` if present and drop it from the
            // in-flight set. Cancellation against a non-in-flight id is
            // a silent drop per `mcp-protocol.md` §12.
            let id = value
                .pointer("/params/request_id")
                .and_then(id_from_value);
            if let Some(id) = id {
                self.session.exit_request(&id);
            }
        }
    }

    fn route(&mut self, req: &Request) -> Result<Value, McpError> {
        let method = req.method.as_str();
        // Gate every operation other than handshake / cancel /
        // server_info on completed handshake. `client.cancel` is a
        // notification and never reaches `route`; we list it here for
        // documentation only.
        if !self.session.is_handshake_complete()
            && method != methods::client::HANDSHAKE
            && method != methods::client::SERVER_INFO
        {
            return Err(McpError::new(
                ErrorClass::HandshakeRequired,
                format!("{method}: complete client.handshake first"),
            ));
        }
        dispatch::dispatch(self.daemon, &mut self.session, req)
    }
}

fn parse_error_response(err: serde_json::Error) -> Response {
    Response::failure(
        Id::Number(0),
        ErrorObject {
            code: ErrorCode::ParseError.into(),
            message: format!("could not parse JSON envelope: {err}"),
            class: Some(ErrorClass::ParseErrorClass.name().to_string()),
            target: None,
            suggestions: Vec::new(),
            streaming: None,
        },
    )
}

fn invalid_request_response(err: serde_json::Error) -> Response {
    Response::failure(
        Id::Number(0),
        ErrorObject {
            code: ErrorCode::InvalidRequest.into(),
            message: format!("envelope did not match a JSON-RPC request: {err}"),
            class: Some(ErrorClass::ArgShapeInvalid.name().to_string()),
            target: None,
            suggestions: Vec::new(),
            streaming: None,
        },
    )
}

fn id_from_value(value: &Value) -> Option<Id> {
    if let Some(n) = value.as_i64() {
        return Some(Id::Number(n));
    }
    if let Some(s) = value.as_str() {
        return Some(Id::String(s.to_string()));
    }
    None
}

// Use of `Version` and `Notification` here keeps the symbols referenced
// from the dispatcher module, which makes it easy to spot when an
// envelope value goes through the wire pipeline.
#[doc(hidden)]
const _: fn() = || {
    let _ = Version;
    let _ = Notification {
        jsonrpc: Version,
        method: String::new(),
        params: None,
    };
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MemoryTransport;
    use serde_json::json;

    fn run_one(daemon: &Daemon, line: &str) -> String {
        let mut server = McpServer::new(daemon);
        let mut transport = MemoryTransport::new([line.to_string()]);
        let _ = server.serve(&mut transport);
        transport.outbox().first().cloned().unwrap_or_default()
    }

    #[test]
    fn handshake_succeeds() {
        let daemon = Daemon::new();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "client.handshake",
            "params": {
                "client_name": "t",
                "client_version": "0",
                "protocol_versions": [1],
                "features": {}
            }
        });
        let out = run_one(&daemon, &req.to_string());
        assert!(out.contains("\"server_name\":\"edda-daemon\""));
        assert!(out.contains("\"protocol_version\":1"));
    }

    #[test]
    fn unknown_method_returns_method_not_found() {
        let daemon = Daemon::new();
        // Handshake first so the gate passes
        let mut server = McpServer::new(&daemon);
        let mut transport = MemoryTransport::new([
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "client.handshake",
                "params": {
                    "client_name": "t",
                    "client_version": "0",
                    "protocol_versions": [1],
                    "features": {}
                }
            })
            .to_string(),
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "frobnicate",
            })
            .to_string(),
        ]);
        let _ = server.serve(&mut transport);
        let last = transport.outbox().last().cloned().unwrap();
        assert!(last.contains("method_not_implemented"));
    }

    #[test]
    fn pre_handshake_op_is_rejected() {
        let daemon = Daemon::new();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "inspect.parsed_ast",
            "params": {"project_root": "/tmp", "file": "/tmp/a.ea"}
        });
        let out = run_one(&daemon, &req.to_string());
        assert!(out.contains("handshake_required"));
    }

    #[test]
    fn invalid_protocol_version_rejected() {
        let daemon = Daemon::new();
        let req = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "client.handshake",
            "params": {
                "client_name": "t",
                "client_version": "0",
                "protocol_versions": [99],
                "features": {}
            }
        });
        let out = run_one(&daemon, &req.to_string());
        assert!(out.contains("unsupported_protocol_version"));
    }

    #[test]
    fn parse_error_returns_jsonrpc_parse_error() {
        let daemon = Daemon::new();
        let out = run_one(&daemon, "not json");
        assert!(out.contains("\"code\":-32700"));
    }
}
