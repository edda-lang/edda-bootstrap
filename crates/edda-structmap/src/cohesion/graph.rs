//! Per-file call graph (directed + undirected views) and callee resolution.

use std::collections::BTreeMap;

use crate::model::FunctionEntry;

/// Per-file call graph with directed (caller → callee) and undirected
/// (for WCC) views over a local 0..n index space.
pub(super) struct FileGraph {
    /// local index → qualified name. Built in `fn_idxs` order so local
    /// indices are stable wrt the caller's slice.
    pub(super) names: Vec<String>,
    /// Directed adjacency — `directed[i]` lists callees of function i.
    /// Self-loops and duplicates are removed.
    pub(super) directed: Vec<Vec<usize>>,
    /// Undirected adjacency — each intra-file edge appears in both
    /// endpoints' lists. Used for WCC enumeration.
    pub(super) undirected: Vec<Vec<usize>>,
}

impl FileGraph {
    pub(super) fn build(functions: &[FunctionEntry], fn_idxs: &[usize]) -> Self {
        let names: Vec<String> = fn_idxs
            .iter()
            .map(|&g| functions[g].qualified_name.clone())
            .collect();
        let name_to_local: BTreeMap<&str, usize> = names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), i))
            .collect();
        let n = names.len();
        let mut directed: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut undirected: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (local, &g) in fn_idxs.iter().enumerate() {
            let caller_module = functions[g]
                .qualified_name
                .rsplit_once('.')
                .map(|(m, _)| m)
                .unwrap_or("");
            for callee in &functions[g].calls {
                if let Some(other) = resolve_callee(callee, caller_module, &name_to_local)
                {
                    if other != local && !directed[local].contains(&other) {
                        directed[local].push(other);
                        if !undirected[local].contains(&other) {
                            undirected[local].push(other);
                            undirected[other].push(local);
                        }
                    }
                }
            }
        }
        Self { names, directed, undirected }
    }

    /// Translate a component-of-local-indices into the report's
    /// component-of-qualified-names, sorted lexicographically within
    /// each component and components sorted by (size desc, lead-name).
    pub(super) fn names_for_components(&self, components: &[Vec<usize>]) -> Vec<Vec<String>> {
        let mut out: Vec<Vec<String>> = components
            .iter()
            .map(|c| {
                let mut names: Vec<String> =
                    c.iter().map(|&i| self.names[i].clone()).collect();
                names.sort();
                names
            })
            .collect();
        out.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a[0].cmp(&b[0])));
        out
    }
}

fn resolve_callee(
    callee: &str,
    caller_module: &str,
    name_to_local: &BTreeMap<&str, usize>,
) -> Option<usize> {
    if callee.starts_with('.') {
        return None;
    }
    if let Some(&idx) = name_to_local.get(callee) {
        return Some(idx);
    }
    if callee.contains('.') || caller_module.is_empty() {
        return None;
    }
    let qualified = format!("{}.{}", caller_module, callee);
    name_to_local.get(qualified.as_str()).copied()
}
