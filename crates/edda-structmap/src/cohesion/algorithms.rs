//! Graph algorithms for the cohesion decision: WCC enumeration, Tarjan
//! SCC sizing, and the hub-and-spoke dispatcher decomposition.

use super::graph::FileGraph;
use super::{MIN_COMPONENT_COUNT, MIN_RESIDUAL_COMPONENT_SIZE, MIN_TOTAL_CLUSTER_COVERAGE};

/// Enumerate weakly-connected components of `undirected`, optionally
/// excluding a single vertex (the dispatcher) from traversal entirely.
/// Returns components whose size is `>= min_size`. Component order is
/// indeterminate; the caller is responsible for any further sorting.
pub(super) fn sized_components(
    undirected: &[Vec<usize>],
    excluded: Option<usize>,
    min_size: usize,
) -> Vec<Vec<usize>> {
    let n = undirected.len();
    let mut seen = vec![false; n];
    if let Some(e) = excluded {
        seen[e] = true;
    }
    let mut out: Vec<Vec<usize>> = Vec::new();
    for start in 0..n {
        if seen[start] {
            continue;
        }
        let mut stack = vec![start];
        let mut component = Vec::new();
        seen[start] = true;
        while let Some(v) = stack.pop() {
            component.push(v);
            for &u in &undirected[v] {
                if !seen[u] {
                    seen[u] = true;
                    stack.push(u);
                }
            }
        }
        if component.len() >= min_size {
            out.push(component);
        }
    }
    out
}

/// Size of the largest strongly-connected component in `directed`.
/// Iterative Tarjan: deterministic by index order, O(V+E).
pub(super) fn largest_scc_size(directed: &[Vec<usize>]) -> usize {
    let n = directed.len();
    const UNVISITED: usize = usize::MAX;
    let mut index_of: Vec<usize> = vec![UNVISITED; n];
    let mut lowlink: Vec<usize> = vec![0; n];
    let mut on_stack: Vec<bool> = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut next_index: usize = 0;
    let mut best: usize = 0;

    // Iterative DFS frame: (vertex, next-callee-index to inspect).
    for root in 0..n {
        if index_of[root] != UNVISITED {
            continue;
        }
        let mut frames: Vec<(usize, usize)> = vec![(root, 0)];
        index_of[root] = next_index;
        lowlink[root] = next_index;
        next_index += 1;
        stack.push(root);
        on_stack[root] = true;
        loop {
            let last_idx = match frames.len() {
                0 => break,
                k => k - 1,
            };
            let (v, ci_val) = frames[last_idx];
            if ci_val < directed[v].len() {
                let w = directed[v][ci_val];
                frames[last_idx].1 = ci_val + 1;
                if index_of[w] == UNVISITED {
                    index_of[w] = next_index;
                    lowlink[w] = next_index;
                    next_index += 1;
                    stack.push(w);
                    on_stack[w] = true;
                    frames.push((w, 0));
                } else if on_stack[w] {
                    let lw = index_of[w];
                    if lw < lowlink[v] {
                        lowlink[v] = lw;
                    }
                }
            } else {
                // Pop v; propagate lowlink upward and emit SCC root.
                let v_low = lowlink[v];
                if index_of[v] == v_low {
                    let mut size = 0;
                    while let Some(w) = stack.pop() {
                        on_stack[w] = false;
                        size += 1;
                        if w == v {
                            break;
                        }
                    }
                    if size > best {
                        best = size;
                    }
                }
                frames.pop();
                if !frames.is_empty() {
                    let p_idx = frames.len() - 1;
                    let p = frames[p_idx].0;
                    if v_low < lowlink[p] {
                        lowlink[p] = v_low;
                    }
                }
            }
        }
    }
    best
}

/// Find the dispatcher AND its spoke decomposition. For each candidate
/// with out-degree ≥ [`MIN_COMPONENT_COUNT`], compute the spoke
/// decomposition (directed-reach of each direct callee with the
/// dispatcher excluded; nodes reachable from exactly one spoke belong
/// to that spoke's cluster, the rest are shared utilities and form no
/// cluster). Rank by (qualifying_cluster_count, total_cluster_size,
/// lex name). Returns `None` if no candidate produces
/// ≥ [`MIN_COMPONENT_COUNT`] clusters of
/// ≥ [`MIN_RESIDUAL_COMPONENT_SIZE`] members. O(V * (V + E)).
///
/// Why spoke decomposition over plain "remove and count WCCs": shared
/// utility helpers (high in-degree leaves like `clone_str`,
/// `line_of`, `sym_to_string`) connect every spoke through the
/// undirected graph, so plain WCC after dispatcher removal misclassifies
/// hub-and-spoke files as one big component. Directed-reach with the
/// dispatcher excluded correctly attributes each utility to "shared"
/// because it appears in multiple spokes' reach sets.
pub(super) fn find_dispatcher_via_spoke_decomp(
    graph: &FileGraph,
) -> Option<(usize, Vec<Vec<usize>>)> {
    let n = graph.names.len();
    let mut best: Option<(usize, Vec<Vec<usize>>, usize)> = None;
    for c in 0..n {
        if graph.directed[c].len() < MIN_COMPONENT_COUNT {
            continue;
        }
        let clusters = spoke_decomposition(graph, c);
        let qualifying: Vec<Vec<usize>> = clusters
            .into_iter()
            .filter(|cl| cl.len() >= MIN_RESIDUAL_COMPONENT_SIZE)
            .collect();
        if qualifying.len() < MIN_COMPONENT_COUNT {
            continue;
        }
        let total: usize = qualifying.iter().map(|x| x.len()).sum();
        // Material-coverage gate — filters shallow 2/2/2-style dispatchers
        // whose total splittable surface is too small to justify a fire.
        if total < MIN_TOTAL_CLUSTER_COVERAGE {
            continue;
        }
        let better = match &best {
            None => true,
            Some((b_c, b_q, b_total)) => {
                (qualifying.len(), total) > (b_q.len(), *b_total)
                    || ((qualifying.len(), total) == (b_q.len(), *b_total)
                        && graph.names[c] < graph.names[*b_c])
            }
        };
        if better {
            best = Some((c, qualifying, total));
        }
    }
    best.map(|(c, q, _)| (c, q))
}

/// Compute spoke clusters around `dispatcher`. For each direct callee
/// `c_i`, the cluster is `{nodes reachable from c_i along directed
/// edges with `dispatcher` excluded} ∩ {nodes whose spoke-reach count
/// is exactly 1}`. Shared utilities (called from multiple spokes) are
/// in no cluster.
fn spoke_decomposition(graph: &FileGraph, dispatcher: usize) -> Vec<Vec<usize>> {
    let n = graph.names.len();
    let callees: Vec<usize> = graph.directed[dispatcher].clone();

    // Pass 1 — accumulate per-node spoke count without storing each spoke's
    // reach set. For every callee we run one DFS with the dispatcher
    // excluded; the `visited` buffer is reused across spokes so we never
    // hold more than one reach set in memory at a time.
    let mut spoke_count: Vec<u32> = vec![0; n];
    let mut visited: Vec<bool> = vec![false; n];
    let mut stack: Vec<usize> = Vec::new();
    for &c in &callees {
        dfs_with_excluded(&graph.directed, c, dispatcher, &mut visited, &mut stack);
        for (node, hit) in visited.iter().enumerate() {
            if *hit && node != dispatcher {
                spoke_count[node] = spoke_count[node].saturating_add(1);
            }
        }
        for v in visited.iter_mut() {
            *v = false;
        }
    }

    // Pass 2 — re-walk each spoke and emit `{v reachable from c_i :
    // spoke_count[v] == 1}`. Same `visited` / `stack` buffers reused
    // across spokes.
    let mut clusters: Vec<Vec<usize>> = Vec::with_capacity(callees.len());
    for &c in &callees {
        dfs_with_excluded(&graph.directed, c, dispatcher, &mut visited, &mut stack);
        let mut cluster: Vec<usize> = Vec::new();
        for (node, hit) in visited.iter().enumerate() {
            if *hit && node != dispatcher && spoke_count[node] == 1 {
                cluster.push(node);
            }
        }
        clusters.push(cluster);
        for v in visited.iter_mut() {
            *v = false;
        }
    }
    clusters
}

/// Iterative DFS over `directed` starting at `start`, with `excluded`
/// removed from the graph. Marks every reachable node (excluding
/// `excluded`) in `visited`. Caller is responsible for clearing
/// `visited` between calls; this function does not mutate `excluded`
/// in `visited` so the same buffer can be reused across spokes.
fn dfs_with_excluded(
    directed: &[Vec<usize>],
    start: usize,
    excluded: usize,
    visited: &mut [bool],
    stack: &mut Vec<usize>,
) {
    stack.clear();
    if start == excluded || visited[start] {
        return;
    }
    visited[start] = true;
    stack.push(start);
    while let Some(v) = stack.pop() {
        for &u in &directed[v] {
            if u == excluded || visited[u] {
                continue;
            }
            visited[u] = true;
            stack.push(u);
        }
    }
}
