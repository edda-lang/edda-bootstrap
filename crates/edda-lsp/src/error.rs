//! LSP-side error type: maps daemon errors and URI errors onto JSON-RPC
//! response errors.
//!
//! [`LspError`] is the unified failure type returned by every request
//! handler. The server's main loop projects it onto an
//! `lsp_server::ResponseError` payload before sending it back to the
//! client. Notifications cannot return errors; they are logged via
//! `window/logMessage` and dropped.

use edda_daemon::DaemonError;
use lsp_server::{ErrorCode, ResponseError};

use crate::uri::UriError;

/// Failure mode for an LSP request handler.
#[derive(Debug)]
pub enum LspError {
    /// The daemon refused or failed the underlying operation.
    Daemon(DaemonError),
    /// The request's URI could not be converted to a path (or vice versa).
    Uri(UriError),
    /// The client sent malformed parameters — e.g. a request for a file
    /// that has no open overlay where one was required.
    InvalidRequest(String),
    /// The feature is registered as a server capability but the request
    /// arrived before the daemon was ready (no project open).
    NotReady(String),
}

impl std::fmt::Display for LspError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Daemon(e) => write!(f, "daemon: {e}"),
            Self::Uri(e) => write!(f, "uri: {e}"),
            Self::InvalidRequest(s) => write!(f, "invalid request: {s}"),
            Self::NotReady(s) => write!(f, "not ready: {s}"),
        }
    }
}

impl std::error::Error for LspError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Daemon(e) => Some(e),
            Self::Uri(e) => Some(e),
            _ => None,
        }
    }
}

impl From<DaemonError> for LspError {
    fn from(e: DaemonError) -> Self {
        Self::Daemon(e)
    }
}

impl From<UriError> for LspError {
    fn from(e: UriError) -> Self {
        Self::Uri(e)
    }
}

impl LspError {
    /// Project to a JSON-RPC `ResponseError` payload.
    pub fn to_response_error(&self) -> ResponseError {
        let (code, message) = match self {
            Self::Daemon(_) => (ErrorCode::InternalError, self.to_string()),
            Self::Uri(_) => (ErrorCode::InvalidParams, self.to_string()),
            Self::InvalidRequest(_) => (ErrorCode::InvalidRequest, self.to_string()),
            Self::NotReady(_) => (ErrorCode::ServerNotInitialized, self.to_string()),
        };
        ResponseError {
            code: code as i32,
            message,
            data: None,
        }
    }
}
