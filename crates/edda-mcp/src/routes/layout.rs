//! `layout.*` route handlers.
//!
//! Every leaf currently returns `method_not_implemented`. The
//! `edda-comptime` [`Layout`](edda_comptime::Layout) surface is
//! reachable through `edda-types`, but the daemon does not yet host
//! a per-project type-table accessor keyed by qualified name.

use serde_json::Value;

use crate::error::McpError;
use crate::methods;
use crate::routes::not_implemented;

/// `layout.size_of` route.
pub fn size_of(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::layout::SIZE_OF))
}

/// `layout.align_of` route.
pub fn align_of(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::layout::ALIGN_OF))
}

/// `layout.offset_of` route.
pub fn offset_of(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::layout::OFFSET_OF))
}

/// `layout.attributes_of` route.
pub fn attributes_of(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::layout::ATTRIBUTES_OF))
}

/// `layout.repr_of` route.
pub fn repr_of(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::layout::REPR_OF))
}

/// `layout.field_layout` route.
pub fn field_layout(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::layout::FIELD_LAYOUT))
}

/// `layout.abi_of` route.
pub fn abi_of(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::layout::ABI_OF))
}
