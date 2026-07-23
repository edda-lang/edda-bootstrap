//! `edit.*` route handlers.
//!
//! Every leaf currently returns `method_not_implemented`. The
//! `structural-edits.md` grammar is locked in the spec, but the
//! daemon does not yet host a structural-edit applicator.

use serde_json::Value;

use crate::error::McpError;
use crate::methods;
use crate::routes::not_implemented;

/// `edit.transaction` route.
pub fn transaction(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::edit::TRANSACTION))
}

/// `edit.declaration.rename` route.
pub fn declaration_rename(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::edit::DECLARATION_RENAME))
}

/// `edit.signature.parameter.add` route.
pub fn signature_parameter_add(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::edit::SIGNATURE_PARAMETER_ADD))
}

/// `edit.signature.parameter.remove` route.
pub fn signature_parameter_remove(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::edit::SIGNATURE_PARAMETER_REMOVE))
}

/// `edit.signature.return_type.set` route.
pub fn signature_return_type_set(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::edit::SIGNATURE_RETURN_TYPE_SET))
}

/// `edit.effect_row.add` route.
pub fn effect_row_add(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::edit::EFFECT_ROW_ADD))
}

/// `edit.effect_row.remove` route.
pub fn effect_row_remove(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::edit::EFFECT_ROW_REMOVE))
}

/// `edit.refactor.rename_with_cascade` route.
pub fn refactor_rename_with_cascade(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::edit::REFACTOR_RENAME_WITH_CASCADE))
}

/// `edit.refactor.extract_function` route.
pub fn refactor_extract_function(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::edit::REFACTOR_EXTRACT_FUNCTION))
}

/// `edit.refactor.inline_function` route.
pub fn refactor_inline_function(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::edit::REFACTOR_INLINE_FUNCTION))
}
