//! Package layout, dependency index, stdlib index — the read-only
//! context that drives `build-system.md` §4 path resolution without
//! doing I/O.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use edda_intern::Symbol;

use crate::path::ModulePath;

/// Describes one package's on-disk layout. Multiple of these stack
/// (one per dependency) inside a [`DepIndex`] when resolving imports
/// that cross package boundaries; the importing package's own layout
/// is passed separately so the resolver can short-circuit the common
/// in-package case.
#[derive(Clone, Debug)]
pub struct PackageLayout {
    /// Directory containing `package.toml`.
    pub root_dir: PathBuf,
    /// The `root_namespace` symbol — the dot-prefix the package's
    /// modules are exposed under.
    pub root_namespace: Symbol,
    /// The multi-segment canonical path `local.*` imports rebuild
    /// against. For user packages this is `[root_namespace]` (a
    /// single segment); for stdlib leaves discovered by autodiscovery
    /// (e.g. `<stdlib>/lib/os/process/`) this is the full registered
    /// path (`std.os.process`) so a `local.child_spec` inside the leaf
    /// rebuilds to `std.os.process.child_spec`. See
    /// [`crate::owning_layout_for_file`].
    pub canonical_root_path: ModulePath,
    /// The `package` name symbol — distinct from `root_namespace` per
    /// `build-system.md §3` (the package's distribution name vs. its
    /// in-source identifier).
    pub package_name: Symbol,
}

impl PackageLayout {
    /// Construct from raw fields. Callers typically build this from
    /// [`edda_manifest::PackageManifest`] at the driver layer
    /// via [`PackageLayout::from_namespace`]; the multi-segment form
    /// is reserved for stdlib autodiscovery, which knows the full
    /// `std.<...>` path of each leaf at construction time.
    pub fn new(
        root_dir: PathBuf,
        root_namespace: Symbol,
        canonical_root_path: ModulePath,
        package_name: Symbol,
    ) -> Self {
        Self {
            root_dir,
            root_namespace,
            canonical_root_path,
            package_name,
        }
    }

    /// Convenience constructor for the common user-package case where
    /// the canonical root path is just `[root_namespace]`. Mirrors the
    /// original single-segment shape of [`PackageLayout::new`].
    pub fn from_namespace(
        root_dir: PathBuf,
        root_namespace: Symbol,
        package_name: Symbol,
    ) -> Self {
        let canonical_root_path = ModulePath::new(vec![root_namespace]);
        Self::new(root_dir, root_namespace, canonical_root_path, package_name)
    }
}

/// Index of resolved third-party dependencies, keyed by their exposed
/// `root_namespace`. Third-party-package paths use `root_namespace`
/// per `build-system.md §4`, not `package`, so the resolver looks up
/// `head` of the import path here.
///
/// The driver builds this after dependency resolution; for v0.1 the
/// full dependency-resolution algorithm is reserved (`build-system.md
/// §3` open question), so this index is typically empty and any
/// non-`std`, non-`<root_namespace>` prefix produces
/// `import_resolution_error`.
#[derive(Clone, Debug, Default)]
pub struct DepIndex {
    by_namespace: HashMap<Symbol, PackageLayout>,
}

impl DepIndex {
    /// Construct an empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a dependency under its `root_namespace`. Returns the
    /// previous entry if one was already registered under the same
    /// namespace (which is itself a manifest-level configuration
    /// error — two deps cannot expose the same `root_namespace`).
    pub fn insert(&mut self, dep: PackageLayout) -> Option<PackageLayout> {
        self.by_namespace.insert(dep.root_namespace, dep)
    }

    /// Look up a dependency by its `root_namespace` symbol.
    pub fn get(&self, root_namespace: Symbol) -> Option<&PackageLayout> {
        self.by_namespace.get(&root_namespace)
    }

    /// `true` when no dependencies are registered.
    pub fn is_empty(&self) -> bool {
        self.by_namespace.is_empty()
    }

    /// Number of registered dependencies.
    pub fn len(&self) -> usize {
        self.by_namespace.len()
    }

    /// Iterate registered dependencies in arbitrary order.
    pub fn iter(&self) -> impl Iterator<Item = (&Symbol, &PackageLayout)> {
        self.by_namespace.iter()
    }
}

/// Index of compiler-bundled stdlib modules. `build-system.md §4`
/// locks `std.*` as the compiler-bundled standard library; the driver
/// populates this index by autodiscovery
/// against the stdlib source root chosen at startup. When no stdlib
/// root is located the index is constructed empty via
/// [`StdlibIndex::empty`] and every `std.*` import fails with a note
/// pointing the operator at `EDDA_STDLIB_ROOT`.
#[derive(Clone, Debug, Default)]
pub struct StdlibIndex {
    modules: HashMap<ModulePath, PathBuf>,
    by_file: HashMap<PathBuf, ModulePath>,
    leaf_layouts: Vec<PackageLayout>,
    enabled: bool,
}

impl StdlibIndex {
    /// Construct an empty (disabled) stdlib index — the v0.1 default.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Construct an enabled stdlib index with a known module set plus
    /// per-leaf [`PackageLayout`]s. Each `modules` entry maps a
    /// canonical [`ModulePath`] (`std.option`, etc.) to the source
    /// file's path on disk; each `leaf_layouts` entry describes one
    /// stdlib leaf package's filesystem root and full canonical path
    /// so [`owning_layout_for_file`] can attribute a stdlib file to
    /// its leaf when rebuilding `local.*` imports.
    pub fn enabled(
        modules: impl IntoIterator<Item = (ModulePath, PathBuf)>,
        leaf_layouts: impl IntoIterator<Item = PackageLayout>,
    ) -> Self {
        let modules: HashMap<ModulePath, PathBuf> = modules.into_iter().collect();
        let mut by_file: HashMap<PathBuf, ModulePath> = HashMap::with_capacity(modules.len() * 2);
        for (canonical, file) in &modules {
            by_file.insert(canonical_file_key(file), canonical.clone());
            // Also key by the raw path so consumers that did not run
            // `canonicalize()` (e.g. in-memory loaders, tests, or
            // platforms where canonicalisation is a no-op) still hit.
            by_file.insert(file.clone(), canonical.clone());
        }
        let leaf_layouts: Vec<PackageLayout> = leaf_layouts.into_iter().collect();
        Self {
            modules,
            by_file,
            leaf_layouts,
            enabled: true,
        }
    }

    /// Borrow the per-leaf [`PackageLayout`] records. Used by
    /// [`owning_layout_for_file`] to attribute a stdlib file's
    /// `local.*` import to the leaf that owns the file rather than
    /// the user package whose driver is active.
    pub fn leaf_layouts(&self) -> &[PackageLayout] {
        &self.leaf_layouts
    }

    /// Whether `std.*` imports have any chance of resolving.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Number of stdlib modules registered in this index. Used by the
    /// driver's stdlib-source diagnostic emitter to detect the "selected
    /// a live root but the walk yielded nothing" case
    /// — otherwise such a silent empty
    /// index produces only the per-import "the bundled stdlib does not
    /// expose this module" cascade with no signal pointing at the root
    /// cause.
    pub fn modules_len(&self) -> usize {
        self.modules.len()
    }

    /// Look up a stdlib module by its canonical path. Returns `None`
    /// for any path when [`StdlibIndex::is_enabled`] is `false`.
    pub fn get(&self, path: &ModulePath) -> Option<&PathBuf> {
        if !self.enabled {
            return None;
        }
        self.modules.get(path)
    }

    /// Look up the canonical [`ModulePath`] for a stdlib source file
    /// by its on-disk path. Tries the path verbatim first, then a
    /// `canonicalize()`d form (to defeat Windows extended-length
    /// prefixes / symlink differences between the path the driver
    /// loaded and the one stored at index-build time). Returns `None`
    /// for any path when [`StdlibIndex::is_enabled`] is `false`.
    ///
    /// This is the bypass route for stdlib files whose source omits
    /// `module std.x.y.z` at the top: the path-derived identity rule
    /// in [`crate::module_identity_for_file`] runs against the
    /// importing package's `root_dir`, which is the user package's
    /// root — not the stdlib's — so it cannot produce a valid result.
    /// Looking the path up here short-circuits that rule.
    pub fn lookup_by_file(&self, file: &Path) -> Option<&ModulePath> {
        if !self.enabled {
            return None;
        }
        if let Some(path) = self.by_file.get(file) {
            return Some(path);
        }
        let key = canonical_file_key(file);
        self.by_file.get(&key)
    }
}

/// Normalise a filesystem path for use as a `HashMap` key inside
/// [`StdlibIndex::by_file`]. Mirrors the policy used by the source
/// graph driver's `canonical_key` so reverse-lookup hits regardless of
/// which spelling the caller passes in.
fn canonical_file_key(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Context describing the file at which an `import` statement appears.
/// Carries the two pieces of information `resolve_import_path` needs
/// to implement the sibling-leaf rule (§301) and to compute the
/// canonical path of a sibling import.
#[derive(Clone, Debug)]
pub struct ImporterContext {
    /// Directory the importing source file lives in.
    pub importer_dir: PathBuf,
    /// Canonical module path of the importing file.
    pub importer_module: ModulePath,
}

/// Pick the [`PackageLayout`] that owns `file_path`. Used by
/// resolver paths whose semantics depend on the file's host package
/// rather than on the active driver's package (B-003: `local.*`
/// resolves under the file's owning package).
///
/// In a workspace build, the active driver's `layout` is one member
/// and the other members appear in `deps`. A file enqueued via a
/// cross-member dot-path lives under its member's `root_dir`, so the
/// longest-prefix match in `deps` identifies that member. The active
/// `layout` wins when no dep is a longer prefix — covering both the
/// single-package case (deps empty) and the common cross-member case
/// where the file belongs to the active driver itself.
///
/// Stdlib leaves form a third candidate source — when autodiscovery
/// pulled a stdlib leaf (e.g. `<stdlib>/lib/os/process/`) into the
/// build, `local.<...>` imports inside that leaf must rebuild
/// against the leaf's full `std.<...>` path, not the user package's
/// `root_namespace`. `stdlib.leaf_layouts()` carries one
/// [`PackageLayout`] per discovered leaf for exactly this lookup.
pub fn owning_layout_for_file<'a>(
    file_path: &Path,
    active: &'a PackageLayout,
    deps: &'a DepIndex,
    stdlib: &'a StdlibIndex,
) -> &'a PackageLayout {
    let mut best: &PackageLayout = active;
    let mut best_len = match prefix_components(file_path, &active.root_dir) {
        Some(n) => n,
        None => 0,
    };
    for (_, dep) in deps.iter() {
        let Some(n) = prefix_components(file_path, &dep.root_dir) else {
            continue;
        };
        if n > best_len {
            best = dep;
            best_len = n;
        }
    }
    for leaf in stdlib.leaf_layouts() {
        let Some(n) = prefix_components(file_path, &leaf.root_dir) else {
            continue;
        };
        if n > best_len {
            best = leaf;
            best_len = n;
        }
    }
    best
}

fn prefix_components(file_path: &Path, root: &Path) -> Option<usize> {
    if !file_path.starts_with(root) {
        return None;
    }
    Some(root.components().count())
}
