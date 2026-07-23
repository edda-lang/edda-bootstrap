//! Walker — turns ([`ResolvedPackage`], [`TypedPackage`]) into a per-directory
//! [`Tree`] of [`DirEntry`] populated with codex-schema items.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::EmitInput;
use crate::model::{ChildIndex, DirEntry, Tree, Visibility};

mod calls;
mod items;
mod render;

use items::{is_under_build_cache, push_module};

pub(crate) fn build_tree(input: &EmitInput) -> Tree {
    // An empty root (relative manifest path whose parent() is "") defeats
    // the starts_with filter below — every path starts_with("") — so the
    // walk ingests out-of-package trees (resolved stdlib bundles) and
    // ensure_ancestors climbs to the drive root.
    debug_assert!(
        !input.package_root.as_os_str().is_empty(),
        "EmitInput::package_root must be a non-empty canonical package directory"
    );
    let mut dirs: BTreeMap<PathBuf, DirEntry> = BTreeMap::new();
    // Always have the package_root entry.
    dirs.entry(input.package_root.to_path_buf()).or_default();

    for module in input.resolved.modules() {
        let entry = input.resolved.module_entry(module.id);
        // The resolver records each module's file path in whatever form the
        // SourceLoader returned it — typically workspace-relative for the
        // FsLoader. Use it verbatim and only treat the `package_root` as a
        // boundary for the ancestor-fill below.
        let file_path = entry.file_path.clone();
        // Skip modules that live outside the active package (stdlib,
        // sibling workspace members, ...). Without this filter a user-side
        // `edda structmap` would write `index.toon` files into the
        // stdlib tree.
        if !file_path.starts_with(input.package_root) {
            continue;
        }
        // Skip spec-instantiated artifacts the bootstrap stages under
        // `<package_root>/.edda/cache/codegen/...`. That directory is a
        // per-machine build-cache (the Edda equivalent of `target/`); walking
        // it as part of the package's structure map both fires
        // `structure_map_too_dense` on cache-volume noise and lists `.edda`
        // as a child of every crate root.
        if is_under_build_cache(&file_path) {
            continue;
        }
        let dir = match file_path.parent() {
            Some(p) => p.to_path_buf(),
            None => continue,
        };
        let dir_entry = dirs.entry(dir.clone()).or_default();
        push_module(
            dir_entry,
            input,
            &entry.ast,
            &file_path,
            &dir,
            entry.file_id,
        );
        // Ensure every ancestor up to package_root is present.
        ensure_ancestors(&mut dirs, &dir, input.package_root);
    }

    // After populating leaves, fill children[] on every parent. Walk the keys
    // in deepest-first order so we know each child's totals before its parent
    // reads them.
    fill_children(&mut dirs, input.package_root);

    Tree { dirs }
}

fn ensure_ancestors(
    dirs: &mut BTreeMap<PathBuf, DirEntry>,
    dir: &Path,
    root: &Path,
) {
    let mut cur = dir.to_path_buf();
    while cur != root && cur.starts_with(root) {
        let parent = match cur.parent() {
            Some(p) => p.to_path_buf(),
            None => break,
        };
        dirs.entry(parent.clone()).or_default();
        if parent == root || !parent.starts_with(root) {
            break;
        }
        cur = parent;
    }
}

/// Cap on the number of public item names listed in a child's `public`
/// headline before truncating with `…`.
const CHILD_PUBLIC_HEADLINE_CAP: usize = 12;

fn fill_children(dirs: &mut BTreeMap<PathBuf, DirEntry>, root: &Path) {
    // Precompute every dir's own counts + public headline up front so the
    // mutation loop below can read a child's summary without aliasing the
    // `&mut dirs` it writes the parent's children[] into.
    let summaries: BTreeMap<PathBuf, (u32, u32, String)> = dirs
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                (
                    v.types.len() as u32,
                    v.functions.len() as u32,
                    public_headline(v),
                ),
            )
        })
        .collect();

    // Compute children relationships: for each (parent, child) where child's
    // parent is parent, and parent is in `dirs`, add a ChildIndex.
    let mut adjacency: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for key in dirs.keys() {
        if key == root {
            continue;
        }
        if let Some(parent) = key.parent() {
            if dirs.contains_key(parent) {
                adjacency
                    .entry(parent.to_path_buf())
                    .or_default()
                    .push(key.clone());
            }
        }
    }

    for (parent, kids) in adjacency {
        let entry = dirs.get_mut(&parent).expect("adjacency parent missing");
        for child in kids {
            let rel = match child.strip_prefix(&parent) {
                Ok(r) => r.to_string_lossy().replace('\\', "/"),
                Err(_) => child.to_string_lossy().into_owned(),
            };
            let (t, f, public) = summaries
                .get(&child)
                .cloned()
                .unwrap_or((0, 0, String::new()));
            entry.children.push(ChildIndex {
                path: rel,
                types: t,
                functions: f,
                public,
            });
        }
        // Stable sort by relative path so output is deterministic.
        entry.children.sort_by(|a, b| a.path.cmp(&b.path));
    }
}

/// Space-joined headline of a directory's public item names — the
/// table-of-contents column carried in a parent's `children[]` row.
pub(super) fn public_headline(entry: &DirEntry) -> String {
    let mut names: Vec<&str> = Vec::new();
    for t in &entry.types {
        if t.visibility == Visibility::Public {
            names.push(t.name.as_str());
        }
    }
    for f in &entry.functions {
        if f.visibility == Visibility::Public {
            let bare = f
                .qualified_name
                .rsplit_once('.')
                .map(|(_, leaf)| leaf)
                .unwrap_or(f.qualified_name.as_str());
            names.push(bare);
        }
    }
    if names.len() > CHILD_PUBLIC_HEADLINE_CAP {
        let head = names[..CHILD_PUBLIC_HEADLINE_CAP].join(" ");
        format!("{head} …")
    } else {
        names.join(" ")
    }
}
