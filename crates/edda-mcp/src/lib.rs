//! Model Context Protocol surface over the [`edda_daemon::Daemon`].
//!
//! Implements the `codegen.*`, `build.*`, `edit.*`, `typecheck.*`,
//! `inspect.*`, `layout.*`, and `client.*` namespaces per
//! `docs/tooling/mcp-protocol.md`.
//!
//! # Layering
//!
//! - [`wire`] — JSON-RPC 2.0 envelope types (request / response /
//!   notification). The wire shape is locked: it is what every external
//!   client serializes against.
//! - [`error`] — the locked error-class catalogue and the integer code
//!   table that maps each class onto a JSON-RPC `error.code`.
//! - [`methods`] — the locked method-name constants. Every operation
//!   the protocol admits has a `&'static str` here.
//! - [`handshake`] — `client.handshake` request / response shapes and
//!   the [`SessionFeatures`] negotiation result.
//! - [`diagnostic`] — wire form of an [`edda_diag::Diagnostic`].
//! - [`params`] / [`result`] — per-operation request and response shapes.
//! - [`session`] — per-connection state: handshake completion, feature
//!   set, in-flight request bookkeeping.
//! - [`server`] — [`McpServer`] entry point: holds an
//!   [`edda_daemon::Daemon`], dispatches one JSON-RPC message at a time
//!   to the right route, and emits a JSON-RPC response.
//! - [`transport`] — pluggable carrier ([`Transport`] trait) plus a
//!   blocking stdio implementation ([`StdioTransport`]).
//! - [`routes`] — per-namespace route modules. Each method maps a
//!   typed [`params::*`] into a typed [`result::*`] (or an
//!   [`error::McpError`]).
//!
//! # Current scope
//!
//! This is the first cut of the MCP wire layer. The daemon currently
//! only ships parse + import-resolve plus document overlays — so the
//! only operations that route end-to-end into
//! daemon-side work are `client.handshake`, `build.typecheck` (returns
//! the daemon's accumulated diagnostics for the open project), and the
//! position-bearing `typecheck.*` / `inspect.*` queries that the
//! `query` surface admits. Every other namespaced operation responds
//! with a structured `method_not_implemented` — its own locked
//! [`error::ErrorClass`] variant (JSON-RPC code -32601), see
//! [`error::ErrorClass`]).
//!
//! # Concurrency
//!
//! Per the [edda-daemon concurrency model] the daemon and its driver
//! are sync, [`parking_lot`]-guarded, and use [`std::thread`] for
//! parallelism. The MCP layer matches: no `tokio`, no `async fn`. A
//! [`Transport`] reads framed messages from a single carrier thread;
//! per-request worker threads will plug in later. The current
//! implementation is request-at-a-time on the carrier thread (sufficient for stdio).
//!
//! [edda-daemon concurrency model]: ../edda_daemon/index.html

pub mod diagnostic;
pub(crate) mod dispatch;
pub mod error;
pub mod handshake;
pub mod methods;
pub mod params;
pub mod result;
pub mod routes;
pub mod server;
pub mod session;
pub mod transport;
pub mod wire;

pub use error::{ErrorClass, ErrorCode, McpError};
pub use handshake::{SessionFeatures, NEGOTIATED_PROTOCOL_VERSION};
pub use server::{McpServer, ServerInfo};
pub use session::Session;
pub use transport::{StdioTransport, Transport, TransportError};
pub use wire::{ErrorObject, Notification, Request, Response, Version};
