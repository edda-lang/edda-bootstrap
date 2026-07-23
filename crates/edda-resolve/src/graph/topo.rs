//! Adjacency construction, topological ordering, and import-cycle
//! detection + diagnostics over the resolved module graph.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_intern::Interner;

use crate::path::ModulePath;

use super::{ModuleEntry, ModuleId, canonical_key, whole_file_span};

pub(super) fn build_adjacency(
    pending: &[(ModuleId, ModulePath, PathBuf)],
    by_path: &HashMap<ModulePath, ModuleId>,
    by_file_path: &HashMap<PathBuf, ModuleId>,
    n: usize,
) -> Vec<Vec<ModuleId>> {
    let mut adj = vec![Vec::new(); n];
    for (importer, target_path, target_file) in pending {
        let target_id = by_path
            .get(target_path)
            .copied()
            .or_else(|| by_file_path.get(&canonical_key(target_file)).copied());
        if let Some(target_id) = target_id {
            adj[importer.as_usize()].push(target_id);
        }
    }
    adj
}

/// Iterative DFS that produces a topological order and the list of
/// distinct cycles in the graph. White / gray / black coloring; back
/// edges into a `gray` node yield a cycle extracted from the active
/// DFS stack.
pub(super) fn topo_or_cycles(imports: &[Vec<ModuleId>]) -> (Vec<ModuleId>, Vec<Vec<ModuleId>>) {
    let n = imports.len();
    let mut color: Vec<u8> = vec![0; n];
    let mut topo: Vec<ModuleId> = Vec::with_capacity(n);
    let mut cycles: HashSet<Vec<ModuleId>> = HashSet::new();

    for root in 0..n {
        if color[root] != 0 {
            continue;
        }
        color[root] = 1;
        let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some(&(node, child_idx)) = stack.last() {
            if let Some(&child) = imports[node].get(child_idx) {
                stack.last_mut().expect("non-empty").1 += 1;
                let child_u = child.as_usize();
                match color[child_u] {
                    0 => {
                        color[child_u] = 1;
                        stack.push((child_u, 0));
                    }
                    1 => {
                        let mut cycle = extract_cycle(&stack, child_u);
                        normalize_cycle(&mut cycle);
                        cycles.insert(cycle);
                    }
                    _ => {}
                }
            } else {
                color[node] = 2;
                topo.push(ModuleId::new(node as u32));
                stack.pop();
            }
        }
    }
    topo.reverse();
    let cycles: Vec<Vec<ModuleId>> = cycles.into_iter().collect();
    (topo, cycles)
}

fn extract_cycle(stack: &[(usize, usize)], target: usize) -> Vec<ModuleId> {
    let start = stack
        .iter()
        .position(|(n, _)| *n == target)
        .expect("back-edge target must be on the DFS stack");
    stack[start..]
        .iter()
        .map(|(n, _)| ModuleId::new(*n as u32))
        .collect()
}

fn normalize_cycle(cycle: &mut [ModuleId]) {
    if cycle.is_empty() {
        return;
    }
    let min_pos = cycle
        .iter()
        .enumerate()
        .min_by_key(|(_, id)| id.as_u32())
        .map(|(i, _)| i)
        .unwrap_or(0);
    cycle.rotate_left(min_pos);
}

pub(super) fn emit_import_cycle(
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
    modules: &[ModuleEntry],
    cycle: &[ModuleId],
    interner: &Interner,
) {
    let head_id = cycle[0];
    let head = &modules[head_id.as_usize()];
    let header_span = whole_file_span(head.file_id, &head.ast);

    let mut path = String::new();
    for (i, id) in cycle.iter().enumerate() {
        if i > 0 {
            path.push_str(" -> ");
        }
        path.push_str(&modules[id.as_usize()].canonical_path.display(interner).to_string());
    }
    path.push_str(" -> ");
    path.push_str(&head.canonical_path.display(interner).to_string());

    let class = DiagnosticClass::ImportCycle;
    let severity = lint_cfg.effective(class);
    let mut diag = Diagnostic::new(
        class,
        severity,
        header_span,
        format!("module import cycle: {path}"),
    );
    for id in cycle.iter().skip(1) {
        let m = &modules[id.as_usize()];
        diag = diag.with_label(
            whole_file_span(m.file_id, &m.ast),
            "participates in the cycle",
        );
    }
    diag = diag.with_note("rework imports so the source graph is acyclic per build-system.md §2");
    diags.push(diag);
}
