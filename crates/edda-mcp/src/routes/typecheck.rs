//! `typecheck.*` route handlers.
//!
//! Every leaf currently returns `method_not_implemented`. The
//! daemon's cascade stops before typecheck (it ships parse +
//! import-resolve), so the per-position type queries cannot be served
//! until the daemon admits the typed HIR. The wire shape is
//! committed.

use serde_json::Value;

use crate::error::McpError;
use crate::methods;
use crate::routes::not_implemented;

/// `typecheck.type_at` route.
pub fn type_at(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::typecheck::TYPE_AT))
}

/// `typecheck.mode_at` route.
pub fn mode_at(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::typecheck::MODE_AT))
}

/// `typecheck.effect_row_at` route.
pub fn effect_row_at(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::typecheck::EFFECT_ROW_AT))
}

/// `typecheck.refinement_obligations_at` route.
pub fn refinement_obligations_at(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::typecheck::REFINEMENT_OBLIGATIONS_AT))
}

/// `typecheck.trust_points_in_scope` route.
pub fn trust_points_in_scope(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::typecheck::TRUST_POINTS_IN_SCOPE))
}

/// `typecheck.comptime_pure_status` route.
pub fn comptime_pure_status(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::typecheck::COMPTIME_PURE_STATUS))
}

/// `typecheck.discharged_refinements` route.
pub fn discharged_refinements(_params: Option<Value>) -> Result<Value, McpError> {
    Err(not_implemented(methods::typecheck::DISCHARGED_REFINEMENTS))
}
