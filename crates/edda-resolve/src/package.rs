//! Bridge between [`edda_manifest::PackageManifest`] and the
//! resolver's read-only [`PackageLayout`] context.

use std::path::PathBuf;

use edda_intern::Interner;
use edda_manifest::PackageManifest;

use crate::layout::PackageLayout;

/// Build a [`PackageLayout`] from a parsed [`PackageManifest`] and
/// the directory containing its `package.toml`. The driver typically
/// calls this once at the start of import resolution.
pub fn package_layout_from_manifest(
    manifest: &PackageManifest,
    package_root: PathBuf,
    interner: &Interner,
) -> PackageLayout {
    PackageLayout::from_namespace(
        package_root,
        interner.intern(&manifest.root_namespace),
        interner.intern(&manifest.package),
    )
}
