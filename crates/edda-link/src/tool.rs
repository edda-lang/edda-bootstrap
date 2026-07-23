//! Unified handle over [`Linker`] and [`Archiver`].
//!
//! `Tool` is the value carried inside error variants and inside the
//! plan dispatch tables — it lets `invoke::run` and `LinkError` work
//! without caring whether the underlying tool is a linker or an
//! archiver.

use std::path::{Path, PathBuf};

use smol_str::SmolStr;

use crate::archiver::Archiver;
use crate::linker::Linker;

/// External tool the plan resolves to.
///
/// Linker variants produce executables and dynamic libraries; archiver
/// variants produce static archives. The split tracks the
/// `OutputKind` enum.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum Tool {
    /// A linker variant (`mold`, `ld.lld`, `ld64.lld`, `lld-link`,
    /// `wasm-ld`).
    Link(Linker),
    /// An archiver variant (`llvm-ar`, `llvm-lib`).
    Archive(Archiver),
}

impl Tool {
    /// Executable name searched on `$PATH`.
    pub const fn tool_name(self) -> &'static str {
        match self {
            Tool::Link(l) => l.tool_name(),
            Tool::Archive(a) => a.tool_name(),
        }
    }

    /// Resolve [`Self::tool_name`] against `$PATH`. Delegates to the
    /// underlying tool's `discover()` so the structured error binds
    /// the correct variant.
    pub fn discover(self) -> Result<PathBuf, crate::error::LinkError> {
        match self {
            Tool::Link(l) => l.discover(),
            Tool::Archive(a) => a.discover(),
        }
    }
}

/// Walk `$PATH` looking for `tool_name`. Returns the resolved path on
/// hit, or a SmolStr of the searched `$PATH` on miss for inclusion in
/// the structured `ToolNotFound` error. On Windows both the bare name
/// and `.exe`-suffixed form are accepted.
pub(crate) fn discover_on_path(tool_name: &str) -> Result<PathBuf, SmolStr> {
    let path_env = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path_env) {
        if let Some(found) = probe(&dir, tool_name) {
            return Ok(found);
        }
    }
    Err(SmolStr::from(path_env.to_string_lossy().as_ref()))
}

/// Test one directory for the executable. On Windows we accept the
/// bare name or the `.exe` form.
fn probe(dir: &Path, tool_name: &str) -> Option<PathBuf> {
    let bare = dir.join(tool_name);
    if is_executable_file(&bare) {
        return Some(bare);
    }
    if cfg!(windows) {
        let with_exe = dir.join(format!("{tool_name}.exe"));
        if is_executable_file(&with_exe) {
            return Some(with_exe);
        }
    }
    None
}

/// File-exists check that tolerates the broken-symlink case (we
/// cannot run a tool we cannot stat, so a broken link is a not-found).
fn is_executable_file(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_name_delegates_to_variant() {
        assert_eq!(Tool::Link(Linker::Mold).tool_name(), "mold");
        assert_eq!(Tool::Archive(Archiver::LlvmAr).tool_name(), "llvm-ar");
    }

    #[test]
    fn discover_on_path_returns_searched_value_on_miss() {
        let err = discover_on_path("__definitely_not_a_real_tool__").unwrap_err();
        assert!(!err.is_empty(), "searched PATH should be carried in the miss");
    }
}
