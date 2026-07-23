//! Per-namespace route handlers.
//!
//! Each module here owns the leaves of one `mcp-protocol.md` §4
//! namespace. The dispatch entry point ([`crate::server::McpServer`])
//! routes by method name, lowers the inbound `params: Value` to the
//! per-leaf typed struct from [`crate::params`], invokes the route,
//! and projects the typed [`crate::result`] back to JSON.
//!
//! Routing rule: if the daemon's underlying query surface
//! does not yet admit an operation end-to-end, the route returns
//! [`crate::error::ErrorClass::MethodNotImplemented`]. The wire
//! shape stays committed even when the implementation does not.

pub mod build;
pub mod client_ops;
pub mod codegen;
pub mod edit;
pub mod inspect;
pub mod layout;
pub mod typecheck;

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::{ErrorClass, McpError};

/// Decode an inbound `Value` into the operation's typed `params`.
///
/// Any deserialisation failure projects to
/// [`ErrorClass::ArgShapeInvalid`] so unrecognised fields, missing
/// required fields, and wrong-type fields surface uniformly per
/// `mcp-protocol.md` §2.
pub(crate) fn decode_params<P: DeserializeOwned>(
    method: &str,
    params: Option<Value>,
) -> Result<P, McpError> {
    let v = params.unwrap_or(Value::Object(Default::default()));
    serde_json::from_value(v).map_err(|err| {
        McpError::new(
            ErrorClass::ArgShapeInvalid,
            format!("invalid params for {method}: {err}"),
        )
    })
}

/// Construct the canonical `method_not_implemented` error for a route
/// that the daemon does not yet implement end-to-end.
pub(crate) fn not_implemented(method: &str) -> McpError {
    McpError::new(
        ErrorClass::MethodNotImplemented,
        format!("{method}: not implemented in this daemon build"),
    )
}
