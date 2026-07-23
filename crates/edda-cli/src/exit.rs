//! Process exit codes for the `edda` binary, per `build-system.md` §10.

use std::process::ExitCode;

/// Build succeeded with no errors.
pub const SUCCESS: u8 = 0;

/// The build ran but produced one or more diagnostics escalated to error class.
pub const BUILD_ERROR: u8 = 1;

/// System-level failure: manifest unparseable, missing file, IO failure,
/// malformed CLI invocation, or unimplemented verb. Driver refusal to start
/// (per `build-system.md` §3) maps here.
pub const SYSTEM_ERROR: u8 = 2;

/// Project a [`u8`] exit code into [`std::process::ExitCode`].
pub fn code(value: u8) -> ExitCode {
    ExitCode::from(value)
}
