//! Source-file discovery — walks `<package_root>/src/` for `*.ea`.
//!
//! Only `src/` is walked here. The §4 sibling subtrees (`tests/`,
//! `bench/`, `examples/`, `codegen/`) are reached on demand by the
//! corresponding [`crate::command::Command`] verb.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const MAX_DEPTH: u32 = 64;
const SOURCE_EXTENSION: &str = "ea";

/// Recursively collect `*.ea` files under `src_root`.
///
/// Returns `Ok(empty)` when `src_root` exists but contains no `.ea`
/// files — a legitimate empty package. Returns `Err` when `src_root`
/// itself is missing or unreadable; the driver maps that to
/// [`crate::error::DriverError::SourceRootMissing`].
///
/// The returned vector is sorted lexicographically so the cascade's
/// "build is a pure function of (source tree, manifest, target)"
/// guarantee (`build-system.md` §1) is not contingent on filesystem
/// enumeration order.
pub fn discover_source_files(src_root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk(src_root, &mut out, 0)?;
    out.sort();
    Ok(out)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: u32) -> io::Result<()> {
    if depth >= MAX_DEPTH {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            walk(&path, out, depth + 1)?;
        } else if file_type.is_file() && has_edda_extension(&path) {
            out.push(path);
        }
    }
    Ok(())
}

fn has_edda_extension(path: &Path) -> bool {
    path.extension().and_then(|s| s.to_str()) == Some(SOURCE_EXTENSION)
}
