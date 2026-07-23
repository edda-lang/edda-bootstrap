//! Per-directory filename-cluster detection — flags two related
//! antipatterns that resolve to the same lint key
//! (`filename_encodes_hierarchy`) per codex §5.6.2:
//!
//! 1. **Shared leading token.** ≥2 `.ea` files in one directory share a
//!    non-trivial leading token (text before the first `_`, or the
//!    whole stem if no `_` is present). Catches both
//!    `scenarios_v2_audit.ea` + `scenarios_v2_cancel.ea` (shared
//!    token `scenarios`) and `queue.ea` + `queue_error.ea` (stem `queue`
//!    is the leading token of both).
//!
//! 2. **Underscore in name.** Any `.ea` filename whose stem contains
//!    `_` is itself a smell — `_` in an Edda filename almost always
//!    encodes a hierarchy boundary that should be a directory. Fires
//!    even for a single file with no siblings.
//!
//! Both conditions report through one diagnostic class; this module
//! returns them as separate fields on [`ClusterReport`] so the driver
//! can produce per-condition `help:` text.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::model::Tree;

/// One directory's filename-clustering findings.
#[derive(Debug, Clone)]
pub struct ClusterReport {
    /// Absolute directory path the analysis covers.
    pub directory: PathBuf,
    /// Groups of ≥2 `.ea` files in this directory that share a leading
    /// token, sorted by leading-token text.
    pub clusters: Vec<FilenameCluster>,
    /// `.ea` basenames in this directory whose stem contains `_`. The
    /// underscore-in-name signal is independent of clustering — a
    /// single underscored filename with no siblings still fires.
    pub underscore_filenames: Vec<String>,
}

impl ClusterReport {
    /// `true` when this report has nothing to surface.
    pub fn is_empty(&self) -> bool {
        self.clusters.is_empty() && self.underscore_filenames.is_empty()
    }
}

/// One group of `.ea` files in the same directory that share a leading
/// token. The remediation: extract these into `<leading_token>/` and
/// rename to drop the redundant prefix.
#[derive(Debug, Clone)]
pub struct FilenameCluster {
    /// Shared leading token (text before the first `_` in every
    /// member's stem, or the entire stem when no `_` is present).
    /// At least 2 characters.
    pub leading_token: String,
    /// `.ea` basenames sharing the cluster, in lexical order. Always
    /// has length ≥ 2.
    pub members: Vec<String>,
}

/// Compute filename-cluster findings for every directory in `tree`.
/// Empty reports (no clusters, no underscored names) are still returned
/// so the caller can index into the result by directory.
pub(crate) fn analyze_clusters(tree: &Tree) -> Vec<ClusterReport> {
    let mut reports = Vec::with_capacity(tree.dirs.len());
    for (dir_path, entry) in &tree.dirs {
        let mut basenames: Vec<String> = entry
            .modules
            .iter()
            .filter_map(|m| {
                std::path::Path::new(&m.file)
                    .file_name()
                    .map(|os| os.to_string_lossy().into_owned())
            })
            .filter(|name| name.ends_with(".ea") || name.ends_with(".edda"))
            .collect();
        basenames.sort();
        basenames.dedup();

        let clusters = cluster_by_leading_token(&basenames);
        let underscore_filenames = collect_underscored(&basenames);

        reports.push(ClusterReport {
            directory: dir_path.clone(),
            clusters,
            underscore_filenames,
        });
    }
    reports
}

fn cluster_by_leading_token(basenames: &[String]) -> Vec<FilenameCluster> {
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for name in basenames {
        let stem = strip_ea_ext(name);
        let leading_token = stem
            .split_once('_')
            .map(|(head, _)| head.to_string())
            .unwrap_or_else(|| stem.to_string());
        if leading_token.len() < 2 {
            continue;
        }
        groups.entry(leading_token).or_default().push(name.clone());
    }
    let mut clusters: Vec<FilenameCluster> = groups
        .into_iter()
        .filter(|(_, members)| members.len() >= 2)
        .map(|(leading_token, members)| FilenameCluster {
            leading_token,
            members,
        })
        .collect();
    clusters.sort_by(|a, b| a.leading_token.cmp(&b.leading_token));
    clusters
}

fn collect_underscored(basenames: &[String]) -> Vec<String> {
    basenames
        .iter()
        .filter(|n| strip_ea_ext(n).contains('_'))
        .cloned()
        .collect()
}

fn strip_ea_ext(name: &str) -> &str {
    name.strip_suffix(".ea")
        .or_else(|| name.strip_suffix(".edda"))
        .unwrap_or(name)
}
