//! `codegen.*` route handlers.
//!
//! Every leaf currently returns `method_not_implemented`. The
//! daemon does not yet route into `edda-codegen` for promote /
//! demote / regenerate / gc, and `codegen.full_hash` needs an
//! `edda-cache`-backed lookup the daemon does not yet expose.

use serde_json::Value;

use crate::error::McpError;
use crate::methods;
use crate::routes::not_implemented;

/// `codegen.promote` route.
pub fn promote(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::codegen::PROMOTE))
}

/// `codegen.demote` route.
pub fn demote(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::codegen::DEMOTE))
}

/// `codegen.regenerate` route.
pub fn regenerate(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::codegen::REGENERATE))
}

/// `codegen.gc` route.
pub fn gc(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::codegen::GC))
}

/// `codegen.full_hash` route.
pub fn full_hash(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::codegen::FULL_HASH))
}
