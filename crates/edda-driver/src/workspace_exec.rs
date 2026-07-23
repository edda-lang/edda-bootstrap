//! Per-member subprocess isolation for workspace builds.
//!
//! `run_workspace` historically built every workspace member in-process,
//! one after another in a single address space. A from-scratch build of
//! a large workspace (e.g. `compiler/`, 35 members) fragments that
//! address space until one member's typecheck or LLVM codegen hits an
//! allocation-failure / OOM-class abort. Because the failing process
//! *is* the build driver, it dies mid-member with no diagnostic — a
//! silent non-zero exit.
//!
//! Isolating each member in a child process fixes both halves: the OS
//! reclaims a member's working set at child exit (so peak pressure is one
//! member, not all 35 fragmenting one heap), and a member that aborts
//! abnormally is now observable to the surviving parent as a non-success
//! [`ExitStatus`], surfaced as a terminal driver diagnostic naming the
//! member.
//!
//! The child is the same binary re-invoked with the *original argv* and
//! only `--manifest-path` swapped to the member's manifest — no flag is
//! reconstructed from [`BuildOptions`], so nothing silently drops when a
//! flag does not round-trip through the lowering. stdio and the
//! environment are inherited, so the launcher-supplied `EDDA_STDLIB_ROOT`
//! / `EDDA_RUNE_PATH` resolution flows through unchanged and each child
//! prints its own `build: starting <member>` + summary line exactly as
//! the in-process path did.

use std::path::{Path, PathBuf};
use std::process::{Command as ProcCommand, ExitStatus};

use edda_diag::{Diagnostic, DiagnosticClass, Severity};
use edda_span::Span;
use edda_structmap::{MemberSummary, PackageSummary};

use crate::command::{BuildOptions, Command};
use crate::context::Driver;
use crate::error::DriverError;
use crate::outcome::{ExitCode, Outcome};
use crate::workspace::{MemberHandle, WorkspaceResolution};

/// Env var the parent sets on each member child, carrying the path the
/// child writes its [`PackageSummary`] sidecar to. Inert in any process
/// the parent did not spawn (normal single-package builds, the daemon,
/// tests).
const MEMBER_SUMMARY_OUT: &str = "EDDA_MEMBER_SUMMARY_OUT";

/// Env var the parent sets on each member child, carrying the path the
/// child writes its per-directory descendant-body sidecar to.
/// Only set when the resolved `[structmap] descendant_tree` flag is
/// `true` — an ordinary isolated build (`compiler/`, `runes/`, `web/`,
/// none of which set `descendant_tree`) never pays the extra
/// serialization cost. Inert in any process the parent did not spawn.
const MEMBER_BODIES_OUT: &str = "EDDA_MEMBER_BODIES_OUT";

/// Env var marking a process as a spawned workspace-member child, so a
/// pathological re-entry into `run_workspace` (e.g. a member manifest
/// that itself declares `[workspace]`) never fans out a second layer of
/// children.
const WORKSPACE_CHILD: &str = "EDDA_WORKSPACE_CHILD";

/// Whether a workspace verb should build its members in isolated child
/// processes.
pub(crate) fn should_isolate(command: &Command) -> bool {
    matches!(
        command,
        Command::Build | Command::Check | Command::Test | Command::Bench | Command::Structmap
    )
}

/// Whether the current process is a spawned workspace-member child.
pub(crate) fn already_isolated() -> bool {
    std::env::var_os(WORKSPACE_CHILD).is_some()
}

/// Build every workspace member in an isolated child process and
/// aggregate the outcomes, then emit the workspace aggregator
/// `index.toon` files from the per-member summaries the children wrote.
pub(crate) fn run_isolated(
    self_exe: PathBuf,
    resolution: &WorkspaceResolution,
    options: &BuildOptions,
) -> Result<Outcome, DriverError> {
    let base_args = child_base_args();
    let mut aggregate = Outcome::empty();
    let mut member_summaries: Vec<MemberSummary> = Vec::new();
    let mut member_bodies: std::collections::BTreeMap<PathBuf, String> =
        std::collections::BTreeMap::new();

    for (idx, member) in resolution.members.iter().enumerate() {
        let member_name = member_label(member);
        let summary_out = sidecar_path(idx);
        let bodies_out = bodies_sidecar_path(idx);
        let manifest = absolute_manifest(&member.manifest_path);

        let mut cmd = ProcCommand::new(&self_exe);
        cmd.args(&base_args)
            .arg("--manifest-path")
            .arg(&manifest)
            .env(WORKSPACE_CHILD, "1")
            .env(MEMBER_SUMMARY_OUT, &summary_out);
        if resolution.descendant_tree {
            cmd.env(MEMBER_BODIES_OUT, &bodies_out);
        }
        let status = cmd.status();

        let (exit_code, diag) = classify_status(&member_name, &status);
        aggregate.exit_code = aggregate.exit_code.worst(exit_code);
        if let Some(d) = diag {
            aggregate.diagnostics.push(d);
        }
        if let Some(summary) = read_member_summary(&summary_out, &member.manifest_dir) {
            member_summaries.push(summary);
        }
        let _ = std::fs::remove_file(&summary_out);

        if resolution.descendant_tree {
            member_bodies.extend(read_member_bodies(&bodies_out));
            let _ = std::fs::remove_file(&bodies_out);
        }
    }

    let all_member_dirs: Vec<PathBuf> = resolution
        .members
        .iter()
        .map(|m| m.manifest_dir.clone())
        .collect();
    let stale = crate::write_workspace_aggregator(
        &options.manifest_path,
        &all_member_dirs,
        &member_summaries,
        options.structmap_check,
        resolution.descendant_tree,
        &member_bodies,
    );
    if options.structmap_check {
        for path in stale {
            crate::structmap::push_structmap_stale(&mut aggregate.diagnostics, &path);
        }
        if aggregate.diagnostics.has_errors() {
            aggregate.exit_code = aggregate.exit_code.worst(ExitCode::BuildError);
        }
    }
    Ok(aggregate)
}

/// Child-side hook: if this process is a spawned member child, persist
/// its package summary to the sidecar path the parent supplied.
pub(crate) fn write_member_summary_sidecar(driver: &Driver) {
    let Some(out) = std::env::var_os(MEMBER_SUMMARY_OUT) else {
        return;
    };
    let Some(summary) = driver.structmap_summary.as_ref() else {
        return;
    };
    let _ = std::fs::write(PathBuf::from(out), encode_summary(summary));
}

/// Child-side hook: if this process is a spawned member child building
/// under a `descendant_tree` workspace, persist its per-directory body
/// text to the sidecar path the parent supplied.
pub(crate) fn write_member_bodies_sidecar(driver: &Driver) {
    let Some(out) = std::env::var_os(MEMBER_BODIES_OUT) else {
        return;
    };
    let _ = std::fs::write(PathBuf::from(out), encode_bodies(&driver.descendant_bodies));
}

/// The argv to forward to each member child: this process's own argv
/// with `argv[0]` and any caller-supplied `--manifest-path` removed.
fn child_base_args() -> Vec<String> {
    let mut out = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        if arg == "--manifest-path" {
            let _ = it.next();
            continue;
        }
        if arg.starts_with("--manifest-path=") {
            continue;
        }
        out.push(arg);
    }
    out
}

/// Project a member child's [`ExitStatus`] into the worst-of exit code
/// plus an optional terminal diagnostic naming the member.
fn classify_status(
    member: &str,
    status: &std::io::Result<ExitStatus>,
) -> (ExitCode, Option<Diagnostic>) {
    match status {
        Ok(s) if s.success() => (ExitCode::Success, None),
        Ok(s) => match s.code() {
            Some(1) => (ExitCode::BuildError, None),
            Some(2) => (ExitCode::SystemError, None),
            Some(code) => (
                ExitCode::SystemError,
                Some(abnormal_diag(member, &format!("exited with code {code}"))),
            ),
            None => (
                ExitCode::SystemError,
                Some(abnormal_diag(member, &signal_phrase(s))),
            ),
        },
        Err(e) => (
            ExitCode::SystemError,
            Some(member_diag(
                member,
                &format!("could not be launched as a child process: {e}"),
            )),
        ),
    }
}

/// Diagnostic for a member child that terminated abnormally with no
/// output — the silent-abort signature of an OOM-under-pressure abort.
fn abnormal_diag(member: &str, how: &str) -> Diagnostic {
    member_diag(
        member,
        &format!(
            "build {how} with no diagnostic — an abnormal termination (commonly an \
             out-of-memory abort under full-workspace address-space pressure)"
        ),
    )
}

/// Build a `parse_error`-class member-failure diagnostic at `Span::DUMMY`
/// (no source span is in scope at the workspace-dispatch layer).
fn member_diag(member: &str, detail: &str) -> Diagnostic {
    Diagnostic::new(
        DiagnosticClass::ParseError,
        Severity::Error,
        Span::DUMMY,
        format!("workspace member `{member}` {detail}"),
    )
}

/// Describe a signal-termination (Unix) or fall back to a generic
/// abnormal-termination phrase (Windows has no signal channel — a crash
/// there always carries a numeric exit code, handled by the `Some(code)`
/// arm).
#[cfg(unix)]
fn signal_phrase(status: &ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match status.signal() {
        Some(sig) => format!("was terminated by signal {sig}"),
        None => "terminated abnormally".to_string(),
    }
}

#[cfg(not(unix))]
fn signal_phrase(_status: &ExitStatus) -> String {
    "terminated abnormally".to_string()
}

/// Human-readable member name for diagnostics.
fn member_label(member: &MemberHandle) -> String {
    member
        .manifest_dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| member.manifest_dir.to_string_lossy().into_owned())
}

/// Absolute form of a member manifest path for the child `--manifest-path`.
fn absolute_manifest(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Temp path for one member's summary sidecar.
fn sidecar_path(idx: usize) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("edda-member-summary-{}-{idx}.txt", std::process::id()));
    p
}

/// Temp path for one member's descendant-body sidecar.
fn bodies_sidecar_path(idx: usize) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("edda-member-bodies-{}-{idx}.bin", std::process::id()));
    p
}

/// Serialise a [`PackageSummary`] for the member sidecar.
fn encode_summary(s: &PackageSummary) -> String {
    format!("{}\n{}\n{}", s.types, s.functions, s.public)
}

/// Read back a member's [`MemberSummary`] from its sidecar, attributing
/// it to the member's manifest directory.
fn read_member_summary(path: &Path, dir: &Path) -> Option<MemberSummary> {
    let body = std::fs::read_to_string(path).ok()?;
    let mut parts = body.splitn(3, '\n');
    let types = parts.next()?.trim().parse::<u32>().ok()?;
    let functions = parts.next()?.trim().parse::<u32>().ok()?;
    let public = parts.next().unwrap_or("").to_string();
    Some(MemberSummary {
        dir: dir.to_path_buf(),
        types,
        functions,
        public,
    })
}

/// Serialise a member's per-directory descendant bodies for the sidecar.
/// Length-prefixed rather than line-delimited: a
/// rendered directory body is arbitrary TOON text and may itself
/// contain newlines.
fn encode_bodies(bodies: &std::collections::BTreeMap<PathBuf, String>) -> Vec<u8> {
    let mut out = Vec::new();
    for (path, body) in bodies {
        let path_bytes = path.to_string_lossy();
        let path_bytes = path_bytes.as_bytes();
        let body_bytes = body.as_bytes();
        out.extend_from_slice(&(path_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(path_bytes);
        out.extend_from_slice(&(body_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(body_bytes);
    }
    out
}

/// Read back a member's descendant bodies from its sidecar.
/// Returns an empty map (never fails) when the sidecar
/// is absent, empty, or unreadable — the caller treats "no bodies" as
/// "this member's directories are absent from the inlined tree", not a
/// build failure.
fn read_member_bodies(path: &Path) -> std::collections::BTreeMap<PathBuf, String> {
    let Ok(bytes) = std::fs::read(path) else {
        return std::collections::BTreeMap::new();
    };
    let mut out = std::collections::BTreeMap::new();
    let mut cursor: &[u8] = &bytes;
    while let Some((path_str, after_path)) = read_length_prefixed(cursor) {
        let Some((body_str, after_body)) = read_length_prefixed(after_path) else {
            break;
        };
        out.insert(PathBuf::from(path_str), body_str);
        cursor = after_body;
    }
    out
}

/// Read one `(len: u32 LE, bytes)` field from `buf`, returning the
/// decoded (lossy) `String` plus the remaining tail.
fn read_length_prefixed(buf: &[u8]) -> Option<(String, &[u8])> {
    if buf.len() < 4 {
        return None;
    }
    let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    let rest = &buf[4..];
    if rest.len() < len {
        return None;
    }
    let s = String::from_utf8_lossy(&rest[..len]).into_owned();
    Some((s, &rest[len..]))
}

#[cfg(test)]
mod bodies_sidecar_tests {
    use super::*;

    #[test]
    fn roundtrips_empty_map() {
        let bodies: std::collections::BTreeMap<PathBuf, String> = std::collections::BTreeMap::new();
        let encoded = encode_bodies(&bodies);
        assert!(encoded.is_empty());
        let temp = std::env::temp_dir().join("edda-bodies-sidecar-test-empty.bin");
        std::fs::write(&temp, &encoded).unwrap();
        let decoded = read_member_bodies(&temp);
        let _ = std::fs::remove_file(&temp);
        assert!(decoded.is_empty());
    }

    #[test]
    fn roundtrips_multiple_entries_with_embedded_newlines() {
        let mut bodies: std::collections::BTreeMap<PathBuf, String> = std::collections::BTreeMap::new();
        bodies.insert(
            PathBuf::from("/ws/lib/cache"),
            "types:\n  Store,struct,src/store.rs,1,pub\nfunctions:\n  hash_bytes\n".to_string(),
        );
        bodies.insert(PathBuf::from("/ws/lib/compile"), "functions:\n  compile\n".to_string());
        bodies.insert(PathBuf::from("/ws/lib/syntax"), String::new());

        let encoded = encode_bodies(&bodies);
        let temp = std::env::temp_dir().join(format!(
            "edda-bodies-sidecar-test-{}.bin",
            std::process::id()
        ));
        std::fs::write(&temp, &encoded).unwrap();
        let decoded = read_member_bodies(&temp);
        let _ = std::fs::remove_file(&temp);

        assert_eq!(decoded, bodies);
    }

    #[test]
    fn missing_sidecar_degrades_to_empty_map() {
        let missing = std::env::temp_dir().join("edda-bodies-sidecar-does-not-exist.bin");
        let _ = std::fs::remove_file(&missing);
        assert!(read_member_bodies(&missing).is_empty());
    }

    #[test]
    fn truncated_tail_is_dropped_not_panicking() {
        let mut bodies: std::collections::BTreeMap<PathBuf, String> = std::collections::BTreeMap::new();
        bodies.insert(PathBuf::from("/ws/lib/a"), "body-a".to_string());
        bodies.insert(PathBuf::from("/ws/lib/b"), "body-b".to_string());
        let mut encoded = encode_bodies(&bodies);
        encoded.truncate(encoded.len() - 3);

        let temp = std::env::temp_dir().join(format!(
            "edda-bodies-sidecar-test-truncated-{}.bin",
            std::process::id()
        ));
        std::fs::write(&temp, &encoded).unwrap();
        let decoded = read_member_bodies(&temp);
        let _ = std::fs::remove_file(&temp);

        assert_eq!(decoded.len(), 1);
        assert_eq!(decoded.get(&PathBuf::from("/ws/lib/a")), Some(&"body-a".to_string()));
    }
}
