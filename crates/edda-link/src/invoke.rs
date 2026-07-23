//! Tool process invocation and outcome capture.
//!
//! [`run`] spawns the tool resolved by [`crate::Tool::discover`],
//! passes the argv built by [`crate::LinkPlan::argv`], waits for the
//! child, and returns a [`LinkOutcome`] carrying the raw captured
//! stdout/stderr. Non-zero exit status is *not* an error at this layer
//! — callers project the outcome to a `Result` via
//! [`LinkOutcome::into_success`].

use std::process::{Command, ExitStatus, Stdio};

use crate::error::{LinkError, make_stderr_excerpt};
use crate::plan::LinkPlan;
use crate::tool::Tool;

/// Result of one tool invocation.
///
/// `status` is the tool's exit status; `stdout` and `stderr` are the
/// captured streams in full. Non-zero exit is preserved here verbatim
/// so callers can decide whether to surface it as an error or as a
/// diagnostic.
#[derive(Debug)]
pub struct LinkOutcome {
    /// Tool exit status.
    pub status: ExitStatus,
    /// Captured stdout.
    pub stdout: Vec<u8>,
    /// Captured stderr.
    pub stderr: Vec<u8>,
}

impl LinkOutcome {
    /// Project this outcome to a `Result`. Returns
    /// [`LinkError::ToolExitedNonZero`] for any non-zero exit; the
    /// driver maps the error to backend exit code 67
    /// (`backend-choice.md` §6.8) and/or to a `Diagnostic` via
    /// [`LinkError::to_diagnostic`].
    pub fn into_success(self, tool: Tool) -> Result<(), LinkError> {
        if self.status.success() {
            return Ok(());
        }
        Err(LinkError::ToolExitedNonZero {
            tool,
            status: self.status,
            stderr_excerpt: make_stderr_excerpt(&self.stderr),
        })
    }
}

/// Resolve the plan's tool, build its argv, spawn it, and capture the
/// result.
///
/// On `Err`, no child process is left running — either spawn failed
/// or `wait_with_output` completed (which always reaps the child).
pub fn run(plan: &LinkPlan<'_>) -> Result<LinkOutcome, LinkError> {
    let tool = plan.tool()?;
    let argv = plan.argv()?;
    let tool_path = tool.discover()?;

    let mut command = Command::new(&tool_path);
    command
        .args(&argv)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    let child = command.spawn().map_err(|source| LinkError::SpawnFailed { tool, source })?;
    let output = child.wait_with_output().map_err(LinkError::IoDuringInvoke)?;

    Ok(LinkOutcome {
        status: output.status,
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archiver::Archiver;
    use crate::linker::Linker;
    use std::os::raw::c_int;

    #[cfg(unix)]
    fn fabricate_status(code: c_int) -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(code << 8)
    }

    #[cfg(windows)]
    fn fabricate_status(code: c_int) -> ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        ExitStatus::from_raw(code as u32)
    }

    #[test]
    fn into_success_returns_ok_for_zero_exit() {
        let outcome = LinkOutcome {
            status: fabricate_status(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        outcome.into_success(Tool::Link(Linker::Mold)).unwrap();
    }

    #[test]
    fn into_success_returns_err_for_nonzero_linker_exit() {
        let outcome = LinkOutcome {
            status: fabricate_status(67),
            stdout: Vec::new(),
            stderr: b"undefined reference to `main'".to_vec(),
        };
        let err = outcome.into_success(Tool::Link(Linker::Mold)).unwrap_err();
        match err {
            LinkError::ToolExitedNonZero { tool, stderr_excerpt, .. } => {
                assert_eq!(tool, Tool::Link(Linker::Mold));
                assert!(stderr_excerpt.contains("undefined reference"));
            }
            other => panic!("expected ToolExitedNonZero, got {other:?}"),
        }
    }

    #[test]
    fn into_success_carries_archiver_tool() {
        let outcome = LinkOutcome {
            status: fabricate_status(2),
            stdout: Vec::new(),
            stderr: b"llvm-ar: error: no such file".to_vec(),
        };
        let err = outcome
            .into_success(Tool::Archive(Archiver::LlvmAr))
            .unwrap_err();
        match err {
            LinkError::ToolExitedNonZero { tool, .. } => {
                assert_eq!(tool, Tool::Archive(Archiver::LlvmAr));
            }
            other => panic!("expected ToolExitedNonZero, got {other:?}"),
        }
    }
}
