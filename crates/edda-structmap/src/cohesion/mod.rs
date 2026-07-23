//! Per-file cohesion lint — `file_low_cohesion`.
//!
//! Companion to [`crate::density`]: density measures whether a directory's
//! emitted `index.toon` is ingestible in one read; cohesion measures
//! whether an individual `.ea` file is ingestible. Raw LOC alone is a
//! bad split metric (a tightly-connected concern should stay one file
//! regardless of size), so the structural signal is what does the work.
//!
//! The analysis decides via three
//! sequential tests against the file's intra-file call graph:
//!
//! 1. **Disjoint-concerns test.** If the original undirected call graph
//!    has ≥ [`MIN_COMPONENT_COUNT`] weakly-connected components of
//!    ≥ [`MIN_COMPONENT_SIZE`] functions each, the file already mixes
//!    independent concerns. Fire. (Pre-existing behaviour.)
//!
//! 2. **Mutual-recursion veto.** Otherwise (everything is one WCC),
//!    compute the largest strongly-connected component. If it covers
//!    ≥ [`SCC_DOMINANCE_NUM`]/[`SCC_DOMINANCE_DEN`] of the file's
//!    functions, the file has a legitimate recursive core
//!    (`lower_expr ↔ lower_block ↔ lower_stmt ↔ ...`-style cycles).
//!    Splitting the cycle would force `error[import_cycle]`. Don't fire.
//!
//! 3. **Hub-and-spoke detection.** Otherwise (no recursive core), pick
//!    the function with maximum out-degree as the candidate dispatcher,
//!    remove it from the graph, and recount WCCs. If the residual has
//!    ≥ [`MIN_COMPONENT_COUNT`] components of
//!    ≥ [`MIN_RESIDUAL_COMPONENT_SIZE`] functions each, the file is a
//!    dispatcher routing to N independent concerns (`render_dir_body`
//!    routing to 7 `(render_X, count_X)` spoke pairs). Fire and name
//!    the dispatcher.
//!
//! Why not Girvan-Newman + modularity Q per the original design proposal:
//! Tarjan-based SCC dominance is deterministic by construction, runs in
//! O(V+E) instead of O(V³), and the threshold (3/10) is integer-valued
//! against a corpus that straddles it cleanly (mutual-recursion files
//! land >50%, hub-and-spoke <10%).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::model::{FunctionEntry, Tree};

mod algorithms;
mod graph;

#[cfg(test)]
mod tests;

use algorithms::{find_dispatcher_via_spoke_decomp, largest_scc_size, sized_components};
use graph::FileGraph;

/// File-LOC floor below which `file_low_cohesion` does not fire.
pub const LOC_FLOOR: usize = 400;

/// Minimum number of components that must clear the size gate.
pub const MIN_COMPONENT_COUNT: usize = 2;

/// Minimum size (in functions) of each step-1 component.
pub const MIN_COMPONENT_SIZE: usize = 3;

/// Function-count gate for the SCC + dispatcher analysis (steps 2 / 3).
pub const MIN_FUNCTION_COUNT: usize = 8;

/// Minimum size of each residual WCC after dispatcher removal (step 3).
pub const MIN_RESIDUAL_COMPONENT_SIZE: usize = 2;

/// SCC dominance threshold numerator (3/10 = 30%).
pub const SCC_DOMINANCE_NUM: usize = 3;

/// SCC dominance threshold denominator.
pub const SCC_DOMINANCE_DEN: usize = 10;

/// Absolute largest-SCC floor that vetoes (exempts) regardless of ratio.
pub const SCC_ABSOLUTE_FLOOR: usize = 20;

/// Minimum total cluster coverage (sum of qualifying cluster sizes) for
/// a fire to be considered material. Set so 3-pair shallow dispatchers
/// (`parse_fn_decl` over generics/params/body) — where each pair is only
/// 2 functions and splitting yields tiny sibling files — are filtered
/// out, while genuine architectural debt patterns
/// (`render_dir_body`'s 8 spoke pairs, `walk.fill_children`'s 3 cleanly
/// separated concerns) continue to fire.
pub const MIN_TOTAL_CLUSTER_COVERAGE: usize = 10;

/// One file's `file_low_cohesion` finding. The driver renders this into
/// an `error[file_low_cohesion]` diagnostic naming the smallest cluster
/// as the suggested extraction candidate, plus (when present) the
/// dispatcher whose removal exposed the spoke clusters.
#[derive(Debug, Clone)]
pub struct FileCohesionReport {
    /// Absolute path of the file (`<dir>/<basename>.ea`).
    pub file: PathBuf,
    /// Source line count of the file.
    pub line_count: usize,
    /// Non-trivial components, ordered by size descending. Each entry is
    /// the list of qualified names in that cluster, sorted
    /// lexicographically for deterministic output.
    pub components: Vec<Vec<String>>,
    /// `Some(name)` iff the step-3 hub-and-spoke path produced this
    /// report; the named function is the dispatcher whose removal
    /// exposed the spoke clusters held in [`Self::components`].
    pub dispatcher: Option<String>,
}

/// Compute `file_low_cohesion` findings for every file in `tree`. The
/// `read_file_lines` callback returns the source line count for an
/// absolute path; the driver wires this to `std::fs::read_to_string`.
pub(crate) fn analyze_file_cohesion(
    tree: &Tree,
    read_file_lines: &dyn Fn(&Path) -> Option<usize>,
) -> Vec<FileCohesionReport> {
    let mut reports = Vec::new();
    for (dir_path, entry) in &tree.dirs {
        let mut per_file: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (idx, f) in entry.functions.iter().enumerate() {
            per_file.entry(f.file.as_str()).or_default().push(idx);
        }
        for (file_rel, fn_idxs) in &per_file {
            // Cheap pre-gate: can't possibly clear MIN_COMPONENT_COUNT * MIN_COMPONENT_SIZE.
            if fn_idxs.len() < MIN_COMPONENT_COUNT * MIN_COMPONENT_SIZE {
                continue;
            }
            let file_path = dir_path.join(file_rel);
            let line_count = match read_file_lines(&file_path) {
                Some(n) => n,
                None => continue,
            };
            if line_count < LOC_FLOOR {
                continue;
            }
            if let Some(report) =
                classify_file(&entry.functions, fn_idxs, file_path, line_count)
            {
                reports.push(report);
            }
        }
    }
    reports
}

/// The seam a separable file can be cut at — the reusable result of the
/// 3-step call-graph analysis, consumed by both the cohesion lint and
/// the structure-budget partition classifier.
#[derive(Debug, Clone)]
pub(crate) struct CallGraphSeam {
    /// Cluster(s) of qualified names that can be split apart.
    pub(crate) components: Vec<Vec<String>>,
    /// The dispatcher whose removal exposed the clusters (step-3 only).
    pub(crate) dispatcher: Option<String>,
}

/// Pure 3-step call-graph classification (no LOC floor, no file-size
/// gate). The structure-budget partition classifier reuses this directly;
/// [`classify_file`] wraps it with the file's path/line-count for the
/// lint. One engine, read from two ends (the handoff §4 unification).
pub(crate) fn classify_call_graph(
    functions: &[FunctionEntry],
    fn_idxs: &[usize],
) -> Option<CallGraphSeam> {
    let graph = FileGraph::build(functions, fn_idxs);
    let n = graph.names.len();

    // Step 1: disjoint concerns at the original-WCC granularity.
    let wccs = sized_components(&graph.undirected, None, MIN_COMPONENT_SIZE);
    let wcc_coverage: usize = wccs.iter().map(|c| c.len()).sum();
    if wccs.len() >= MIN_COMPONENT_COUNT && wcc_coverage >= MIN_TOTAL_CLUSTER_COVERAGE {
        return Some(CallGraphSeam {
            components: graph.names_for_components(&wccs),
            dispatcher: None,
        });
    }

    // Step 2: mutual-recursion veto. Requires enough functions for the
    // dispatcher analysis to mean anything; below MIN_FUNCTION_COUNT the
    // graph is too sparse to call hub-and-spoke meaningfully.
    if n < MIN_FUNCTION_COUNT {
        return None;
    }
    let largest_scc = largest_scc_size(&graph.directed);
    if largest_scc >= SCC_ABSOLUTE_FLOOR
        || largest_scc * SCC_DOMINANCE_DEN >= n * SCC_DOMINANCE_NUM
    {
        return None;
    }

    // Step 3: hub-and-spoke via spoke decomposition.
    let (dispatcher_idx, residual) = find_dispatcher_via_spoke_decomp(&graph)?;
    Some(CallGraphSeam {
        components: graph.names_for_components(&residual),
        dispatcher: Some(graph.names[dispatcher_idx].clone()),
    })
}

/// Run the 3-step decision against one file's call graph, packaging the
/// seam (if any) into a [`FileCohesionReport`] for the lint.
fn classify_file(
    functions: &[FunctionEntry],
    fn_idxs: &[usize],
    file_path: PathBuf,
    line_count: usize,
) -> Option<FileCohesionReport> {
    classify_call_graph(functions, fn_idxs).map(|seam| FileCohesionReport {
        file: file_path,
        line_count,
        components: seam.components,
        dispatcher: seam.dispatcher,
    })
}

/// Partition a directory's `.ea` files by intra-directory call
/// connectivity. The budget gate's relocate classifier consumes this:
/// ≥2 components means a group of files can be `git mv`'d into a
/// subdirectory without creating any cross-directory call edge.
pub(crate) fn dir_file_components(functions: &[FunctionEntry]) -> Vec<Vec<String>> {
    let mut files: Vec<&str> = Vec::new();
    let mut file_idx: BTreeMap<&str, usize> = BTreeMap::new();
    for f in functions {
        if !file_idx.contains_key(f.file.as_str()) {
            file_idx.insert(f.file.as_str(), files.len());
            files.push(f.file.as_str());
        }
    }
    let n = files.len();
    let mut name_to_file: BTreeMap<&str, usize> = BTreeMap::new();
    for f in functions {
        name_to_file.insert(f.qualified_name.as_str(), file_idx[f.file.as_str()]);
    }
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for f in functions {
        let src = file_idx[f.file.as_str()];
        let caller_module = f
            .qualified_name
            .rsplit_once('.')
            .map(|(m, _)| m)
            .unwrap_or("");
        for callee in &f.calls {
            if let Some(dst) = resolve_callee_file(callee, caller_module, &name_to_file) {
                if dst != src && !adj[src].contains(&dst) {
                    adj[src].push(dst);
                    adj[dst].push(src);
                }
            }
        }
    }
    let comps = sized_components(&adj, None, 1);
    let mut out: Vec<Vec<String>> = comps
        .iter()
        .map(|c| {
            let mut names: Vec<String> = c.iter().map(|&i| files[i].to_string()).collect();
            names.sort();
            names
        })
        .collect();
    out.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a[0].cmp(&b[0])));
    out
}

/// Resolve a callee to the index of the file declaring it (same bare-name
/// qualification as the per-file `resolve_callee`).
fn resolve_callee_file(
    callee: &str,
    caller_module: &str,
    name_to_file: &BTreeMap<&str, usize>,
) -> Option<usize> {
    if callee.starts_with('.') {
        return None;
    }
    if let Some(&i) = name_to_file.get(callee) {
        return Some(i);
    }
    if callee.contains('.') || caller_module.is_empty() {
        return None;
    }
    let qualified = format!("{}.{}", caller_module, callee);
    name_to_file.get(qualified.as_str()).copied()
}
