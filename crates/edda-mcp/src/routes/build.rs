//! `build.*` route handlers.
//!
//! Every leaf currently returns `method_not_implemented`. The
//! wire shape is committed; the daemon's `edda_driver` integration
//! does not yet expose a typecheck-only entry point that runs against
//! an already-open project, and re-running the whole cascade from
//! scratch from this surface would duplicate `client.open_project`'s
//! work.
//!
//! When the daemon grows a "rerun typecheck against the current
//! project state" entry point, this module wires it.

use serde_json::Value;

use crate::error::McpError;
use crate::methods;
use crate::params::BuildCommonParams;
use crate::routes::{decode_params, not_implemented};

/// `build.compile` route.
pub fn compile(params: Option<Value>) -> Result<Value, McpError> {
    let _: BuildCommonParams = decode_params(methods::build::COMPILE, params)?;
    Err(not_implemented(methods::build::COMPILE))
}

/// `build.typecheck` route.
pub fn typecheck(params: Option<Value>) -> Result<Value, McpError> {
    let _: BuildCommonParams = decode_params(methods::build::TYPECHECK, params)?;
    Err(not_implemented(methods::build::TYPECHECK))
}

/// `build.run` route.
pub fn run(params: Option<Value>) -> Result<Value, McpError> {
    let _: BuildCommonParams = decode_params(methods::build::RUN, params)?;
    Err(not_implemented(methods::build::RUN))
}

/// `build.test` route.
pub fn test(params: Option<Value>) -> Result<Value, McpError> {
    let _: BuildCommonParams = decode_params(methods::build::TEST, params)?;
    Err(not_implemented(methods::build::TEST))
}

/// `build.bench` route.
pub fn bench(params: Option<Value>) -> Result<Value, McpError> {
    let _: BuildCommonParams = decode_params(methods::build::BENCH, params)?;
    Err(not_implemented(methods::build::BENCH))
}

/// `build.format` route.
pub fn format(params: Option<Value>) -> Result<Value, McpError> {
    let _: BuildCommonParams = decode_params(methods::build::FORMAT, params)?;
    Err(not_implemented(methods::build::FORMAT))
}

/// `build.lint` route.
pub fn lint(params: Option<Value>) -> Result<Value, McpError> {
    let _: BuildCommonParams = decode_params(methods::build::LINT, params)?;
    Err(not_implemented(methods::build::LINT))
}

/// `build.clean` route.
pub fn clean(params: Option<Value>) -> Result<Value, McpError> {
    let _: BuildCommonParams = decode_params(methods::build::CLEAN, params)?;
    Err(not_implemented(methods::build::CLEAN))
}
