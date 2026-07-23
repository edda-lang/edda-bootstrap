//! Per-directory density projection — counts the rendered `index.toon`
//! LOC and identifies which source module contributes the largest share
//! of rows. Surfaces as `structure_map_too_dense` diagnostics emitted by
//! the driver during cascade passes.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::INDEX_FILENAME;
use crate::model::{DirEntry, Tree};

/// Default warn-at threshold. The codex §5.6.3 stake — aggressive on
/// purpose so the language pulls toward depth-first organisation from
/// the first file written.
pub const DEFAULT_DENSITY_WARN_LOC: usize = 250;

/// One directory's density projection.
#[derive(Debug, Clone)]
pub struct DensityReport {
    /// Absolute directory path the projection covers.
    pub directory: PathBuf,
    /// Rendered `index.toon` line count (newline-terminated).
    pub projected_loc: usize,
    /// The source module that contributes the most rows to this
    /// directory's `index.toon`, or `None` for parent-link-only
    /// directories with no local modules.
    pub top_contributor: Option<TopContributor>,
}

/// The largest single source-file contributor to a directory's
/// `index.toon` row count. Identifies the candidate to extract into a
/// sibling subdirectory.
#[derive(Debug, Clone)]
pub struct TopContributor {
    /// Bare module name — the file's stem without its `.ea` / `.edda`
    /// extension. Used in remediation messages: "extract `<name>` into
    /// `<name>/`".
    pub module_name: String,
    /// Source-file basename — kept around so the diagnostic can print
    /// the exact filename the user wrote (e.g. `queue_error.ea`).
    pub file_basename: String,
    /// Number of `functions[]` rows the renderer attributes to this
    /// module.
    pub function_rows: usize,
    /// Number of `types[]` rows the renderer attributes to this module.
    pub type_rows: usize,
}

impl TopContributor {
    /// Combined row count — used to pick the dominant contributor and
    /// rendered into the remediation message.
    pub fn total_rows(&self) -> usize {
        self.function_rows + self.type_rows
    }
}

/// Compute density projections for every directory in `tree`. `files`
/// is the rendered path → TOON text mapping; line counts come from
/// there.
pub(crate) fn compute_density(
    tree: &Tree,
    files: &BTreeMap<std::path::PathBuf, String>,
) -> Vec<DensityReport> {
    let mut reports = Vec::with_capacity(tree.dirs.len());
    for (dir_path, entry) in &tree.dirs {
        let index_path = dir_path.join(INDEX_FILENAME);
        let projected_loc = files
            .get(&index_path)
            .map(|text| text.lines().count())
            .unwrap_or(0);
        let top_contributor = compute_top_contributor(entry);
        reports.push(DensityReport {
            directory: dir_path.clone(),
            projected_loc,
            top_contributor,
        });
    }
    reports
}

fn compute_top_contributor(entry: &DirEntry) -> Option<TopContributor> {
    let mut per_module: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for f in &entry.functions {
        per_module.entry(f.file.as_str()).or_default().0 += 1;
    }
    for t in &entry.types {
        per_module.entry(t.file.as_str()).or_default().1 += 1;
    }
    let mut top: Option<(&str, usize, usize)> = None;
    for (file, (fns, tys)) in &per_module {
        let total = fns + tys;
        let take = match top {
            None => true,
            Some((_, t_fns, t_tys)) => total > t_fns + t_tys,
        };
        if take {
            top = Some((file, *fns, *tys));
        }
    }
    let (file, function_rows, type_rows) = top?;
    let file_basename = std::path::Path::new(file)
        .file_name()
        .map(|os| os.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.to_string());
    let module_name = file_basename
        .strip_suffix(".ea")
        .or_else(|| file_basename.strip_suffix(".edda"))
        .unwrap_or(&file_basename)
        .to_string();
    Some(TopContributor {
        module_name,
        file_basename,
        function_rows,
        type_rows,
    })
}
