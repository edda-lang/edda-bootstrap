//! StdlibIndex construction: walk the chosen root, peek each package.toml,
//! and register every leaf package (and its src/ siblings) as std.<path>.

use std::path::{Path, PathBuf};

use edda_diag::{Diagnostics, LintConfig};
use edda_intern::Interner;
use edda_manifest::LoadedManifest;
use edda_resolve::{ModulePath, PackageLayout, StdlibIndex};
use edda_span::SourceMap;

use crate::workspace::member_dirs_for_workspace;
use super::StdlibSource;

/// Build the [`StdlibIndex`] by walking the root carried in `source` and
/// registering every leaf package as `std.<relative_dotted_path>`. A
/// leading `lib/` segment in the relative path is elided so
/// workspace-layout stdlibs (`<root>/lib/<X>/`) and flat-layout stdlibs
/// (`<root>/<X>/`) both register as `std.<X>`. For
/// [`StdlibSource::None`], returns [`StdlibIndex::empty`] without
/// walking anything.
pub(crate) fn build_stdlib_index(interner: &Interner, source: &StdlibSource) -> StdlibIndex {
    let Some(root) = source.root() else {
        return StdlibIndex::empty();
    };
    let mut modules: Vec<(ModulePath, PathBuf)> = Vec::new();
    let mut leaf_layouts: Vec<PackageLayout> = Vec::new();
    walk_stdlib_packages(root, root, &mut modules, &mut leaf_layouts, interner);
    StdlibIndex::enabled(modules, leaf_layouts)
}

/// Recursive worker for [`build_stdlib_index`].
///
/// At each directory: if `cur/package.toml` exists, peek at it.
/// - **Workspace manifest** (B-012) — resolve members via
///   [`member_dirs_for_workspace`] and recurse into each. The workspace
///   directory itself is NOT a leaf and is not registered.
/// - **Package manifest** — register as a leaf under `std.<rel-dotted>`
///   and stop descending. The leading `lib/` is elided so workspace-layout
///   and flat-layout stdlibs both register at the same canonical path.
/// - **Unparseable** — silently skip (stdlib discovery must not abort
///   compiler startup on a mid-edit manifest).
///
/// At every other directory (no `package.toml`) the walker recurses into
/// every subdirectory.
fn walk_stdlib_packages(
    root: &Path,
    cur: &Path,
    out: &mut Vec<(ModulePath, PathBuf)>,
    leaf_layouts: &mut Vec<PackageLayout>,
    interner: &Interner,
) {
    let manifest_path = cur.join("package.toml");
    if manifest_path.is_file() {
        match peek_stdlib_manifest_shape(&manifest_path) {
            ManifestShape::Workspace(member_dirs) => {
                // B-012: workspace root is not a leaf — recurse into its members.
                for member_dir in member_dirs {
                    walk_stdlib_packages(root, &member_dir, out, leaf_layouts, interner);
                }
                return;
            }
            ManifestShape::Package => {
                register_stdlib_leaf(root, cur, out, leaf_layouts, interner);
                return;
            }
            ManifestShape::Unparseable => {
                // Bad manifest — skip both registration and recursion.
                return;
            }
        }
    }
    let Ok(entries) = std::fs::read_dir(cur) else { return };
    let mut subdirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    subdirs.sort();
    for sub in &subdirs {
        walk_stdlib_packages(root, sub, out, leaf_layouts, interner);
    }
}

/// Result of peeking at a stdlib-directory `package.toml`.
enum ManifestShape {
    /// Single-package manifest (`[package]` present). The caller registers
    /// the directory as a `std.<…>` leaf.
    Package,
    /// Workspace-only manifest (`[workspace]` present, `[package]` absent).
    /// The caller recurses into each member directory.
    Workspace(Vec<PathBuf>),
    /// Manifest failed to parse or is missing required fields. The caller
    /// skips the directory entirely.
    Unparseable,
}

/// Peek at a stdlib-directory `package.toml` and classify it. Uses a
/// private [`SourceMap`] so the peeked diagnostics never enter the
/// driver's own take — this walker runs at compiler startup before any
/// build-state diagnostics machinery exists.
fn peek_stdlib_manifest_shape(manifest_path: &Path) -> ManifestShape {
    let source_map = SourceMap::new();
    let mut diags = Diagnostics::new();
    let lint_cfg = LintConfig::new();
    let Some(loaded) = edda_manifest::load_any(manifest_path, &source_map, &mut diags, &lint_cfg)
    else {
        return ManifestShape::Unparseable;
    };
    match loaded {
        LoadedManifest::Package(_) => ManifestShape::Package,
        LoadedManifest::WorkspaceOnly(ws) => {
            let manifest_dir = manifest_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            match member_dirs_for_workspace(&manifest_dir, &ws.workspace) {
                Ok(member_dirs) => ManifestShape::Workspace(member_dirs),
                // A discovery walk failure (root missing, unreadable) for a
                // stdlib workspace shouldn't abort compiler startup — treat
                // as an empty workspace.
                Err(_) => ManifestShape::Workspace(Vec::new()),
            }
        }
    }
}

/// Register `cur` as a stdlib leaf package at `std.<rel-dotted>`, plus
/// every additional `.ea` file under `cur/src/` as
/// `std.<rel-dotted>.<src-relative-dotted-stem>`, plus one
/// [`PackageLayout`] entry describing the leaf's filesystem root and
/// full canonical path. Skips silently when the relative path is
/// empty (the stdlib root itself somehow has a `[package]` manifest —
/// pathological but harmless) or when the conventional
/// `src/<leaf>.ea` entry file is missing.
fn register_stdlib_leaf(
    root: &Path,
    cur: &Path,
    out: &mut Vec<(ModulePath, PathBuf)>,
    leaf_layouts: &mut Vec<PackageLayout>,
    interner: &Interner,
) {
    let Ok(rel) = cur.strip_prefix(root) else { return };
    let mut segments: Vec<String> = rel
        .iter()
        .filter_map(|s| s.to_str().map(str::to_owned))
        .collect();
    // Elide a leading `lib/` so `stdlib/lib/net/ip/` and `stdlib/net/ip/`
    // both register as `std.net.ip`. The `leaf` (final segment) used to
    // locate `src/<leaf>.ea` is unaffected — it always sits in the last
    // position regardless of the prefix.
    if segments.first().map(String::as_str) == Some("lib") {
        segments.remove(0);
    }
    let Some(leaf) = segments.last().cloned() else { return };
    let src_dir = cur.join("src");
    let leaf_file = src_dir.join(format!("{leaf}.ea"));
    if !leaf_file.is_file() {
        return;
    }
    let mut path_segs: Vec<edda_intern::Symbol> = Vec::with_capacity(segments.len() + 1);
    path_segs.push(interner.intern("std"));
    for seg in &segments {
        path_segs.push(interner.intern(seg));
    }
    let canonical_root_path = ModulePath::new(path_segs.clone().into_boxed_slice());
    out.push((canonical_root_path.clone(), leaf_file.clone()));
    // Synthesise a per-leaf PackageLayout so owning_layout_for_file can
    // attribute stdlib files to their leaf rather than the active
    // driver's user package.
    let leaf_symbol = path_segs.last().copied().unwrap_or_else(|| interner.intern("std"));
    leaf_layouts.push(PackageLayout::new(
        cur.to_path_buf(),
        leaf_symbol,
        canonical_root_path,
        leaf_symbol,
    ));
    register_stdlib_siblings(&src_dir, &src_dir, &leaf_file, &path_segs, out, interner);
}

/// Recursive walker over `src/` that registers every additional `.ea`
/// file under the package's synthesised stdlib namespace. Each match
/// is registered as `<prefix>.<src-relative-dotted-stem>` where the
/// stem strips the trailing `.ea` extension.
fn register_stdlib_siblings(
    src_root: &Path,
    cur: &Path,
    leaf_file: &Path,
    prefix: &[edda_intern::Symbol],
    out: &mut Vec<(ModulePath, PathBuf)>,
    interner: &Interner,
) {
    let Ok(entries) = std::fs::read_dir(cur) else { return };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for entry in paths {
        if entry.is_dir() {
            register_stdlib_siblings(src_root, &entry, leaf_file, prefix, out, interner);
            continue;
        }
        if !entry.is_file() {
            continue;
        }
        if entry.extension().and_then(|e| e.to_str()) != Some("ea") {
            continue;
        }
        if entry == leaf_file {
            continue;
        }
        let Ok(rel) = entry.strip_prefix(src_root) else { continue };
        let components: Vec<String> = rel
            .iter()
            .filter_map(|s| s.to_str().map(str::to_owned))
            .collect();
        if components.is_empty() {
            continue;
        }
        let mut segs: Vec<edda_intern::Symbol> =
            Vec::with_capacity(prefix.len() + components.len());
        segs.extend_from_slice(prefix);
        let last_idx = components.len() - 1;
        let stem = components[last_idx].strip_suffix(".ea").unwrap_or(&components[last_idx]);
        let leaf_named_dir = last_idx > 0 && components[last_idx - 1] == stem;
        for (i, c) in components.iter().enumerate() {
            if leaf_named_dir && i == last_idx {
                continue;
            }
            let text: &str = if i == last_idx { stem } else { c.as_str() };
            segs.push(interner.intern(text));
        }
        out.push((ModulePath::new(segs.into_boxed_slice()), entry));
    }
}
