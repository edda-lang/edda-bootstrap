//! PE import-library synthesis for `extern "sym" from "dll"` declarations.
//!
//! An `extern "S" from "D"` function must
//! resolve through a PE `.idata` import from DLL `D` rather than static
//! `edda_rt.lib` resolution. lld-link materialises `.idata` (import
//! directory + IAT) from short-import-format archive members, so this
//! module synthesises one import library per DLL: it writes a module
//! definition (`.def`) file naming the DLL and its imported symbols,
//! then drives `llvm-lib /DEF:` (the same archiver `Archiver::for_target`
//! selects for COFF) to produce the `.lib` the driver appends to the
//! link inputs.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::archiver::Archiver;
use crate::error::{LinkError, make_stderr_excerpt};
use crate::tool::Tool;

/// One DLL's worth of `.idata` imports: the importing-DLL name as it
/// appears in the `from "..."` clause plus every extern symbol declared
/// against it.
pub struct ImportLibSpec<'a> {
    /// DLL name verbatim from source (e.g. `ntdll.dll`).
    pub dll: &'a str,
    /// Linker-visible symbols imported from `dll`, undecorated.
    pub symbols: &'a [String],
}

/// Synthesise a short-import-format `.lib` for one DLL's import set.
///
/// Writes `<out_dir>/<dll-stem>.def` (`LIBRARY` + `EXPORTS` form), then
/// runs `llvm-lib /MACHINE:<machine> /DEF:<def> /OUT:<lib>` and returns
/// the produced `<out_dir>/<dll-stem>_imports.lib` path. `machine` is
/// the lld-link `/MACHINE` spelling for the active target arch (`x64`,
/// `arm64`). Filesystem failures writing the `.def` surface as
/// [`LinkError::IoDuringInvoke`]; tool failures surface as the same
/// spawn / exit variants [`crate::run`] produces.
pub fn synthesize_import_lib(
    spec: &ImportLibSpec<'_>,
    out_dir: &Path,
    machine: &str,
) -> Result<PathBuf, LinkError> {
    let stem = dll_stem(spec.dll);
    let def_path = out_dir.join(format!("{stem}.def"));
    let lib_path = out_dir.join(format!("{stem}_imports.lib"));

    let mut def_text = String::with_capacity(64 + spec.symbols.len() * 32);
    def_text.push_str(&format!("LIBRARY {}\nEXPORTS\n", spec.dll));
    for symbol in spec.symbols {
        def_text.push_str(symbol);
        def_text.push('\n');
    }
    std::fs::write(&def_path, def_text).map_err(LinkError::IoDuringInvoke)?;

    let tool = Tool::Archive(Archiver::LlvmLib);
    let tool_path = tool.discover()?;
    let mut command = Command::new(&tool_path);
    command
        .arg(format!("/MACHINE:{machine}"))
        .arg(format!("/DEF:{}", def_path.display()))
        .arg(format!("/OUT:{}", lib_path.display()))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null());

    let child = command
        .spawn()
        .map_err(|source| LinkError::SpawnFailed { tool, source })?;
    let output = child.wait_with_output().map_err(LinkError::IoDuringInvoke)?;
    if !output.status.success() {
        return Err(LinkError::ToolExitedNonZero {
            tool,
            status: output.status,
            stderr_excerpt: make_stderr_excerpt(&output.stderr),
        });
    }
    Ok(lib_path)
}

/// Filename stem for a DLL name: extension dropped, non-filename
/// characters replaced with `_`.
fn dll_stem(dll: &str) -> String {
    let base = dll.rsplit(['/', '\\']).next().unwrap_or(dll);
    let stem = base.strip_suffix(".dll").or_else(|| base.strip_suffix(".DLL")).unwrap_or(base);
    stem.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::dll_stem;

    #[test]
    fn dll_stem_drops_extension_and_sanitizes() {
        assert_eq!(dll_stem("ntdll.dll"), "ntdll");
        assert_eq!(dll_stem("api-ms-win-core.DLL"), "api-ms-win-core");
        assert_eq!(dll_stem("dir/evil name.dll"), "evil_name");
    }
}
