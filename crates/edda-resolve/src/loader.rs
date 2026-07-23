//! Source-file loader abstraction.
//!
//! The source-import graph driver reads `.ea` files via this trait so the
//! daemon (Charter Article VIII; `build-system.md §5a`) can swap in
//! its in-memory file mirror without touching the resolver.

use std::io;
use std::path::Path;

/// Reads `.ea` source files for the source-graph driver.
///
/// The graph driver only consumes [`SourceLoader::load`] and never
/// touches the filesystem directly; concrete implementations include
/// [`FsLoader`] (reads via `std::fs::read_to_string`) and the
/// daemon's structural-index mirror (lands with `edda-daemon`).
pub trait SourceLoader {
    /// Load `path`'s contents as UTF-8. The path is exactly the
    /// `expected_file` produced by `resolve_import_path` for
    /// a successfully-resolved import, or one of the entry files
    /// supplied to the graph driver.
    fn load(&self, path: &Path) -> io::Result<String>;
}

/// On-disk loader — reads via `std::fs::read_to_string`. The default
/// loader for CLI builds (`edda build`, `edda check`).
#[derive(Copy, Clone, Debug, Default)]
pub struct FsLoader;

impl SourceLoader for FsLoader {
    fn load(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }
}
