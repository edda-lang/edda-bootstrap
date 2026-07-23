//! Strongly-connected-component analysis over the package's function
//! call graph, used by the divergence-injection rule.
//!
//! Per `corpus/edda-codex/language/03-verification.md` §5, a recursive
//! function whose termination is not proven by a `decreases` measure
//! must admit `effect divergence` in its row. "Recursive" is taken in
//! the call-graph SCC sense — self-recursion (single-node SCC with a
//! self-edge) and mutual recursion (SCC size ≥ 2) are both treated as
//! a single termination obligation per the codex's *mutual recursion
//! shares one `decreases` tuple* rule.
//!
//! This module owns three things:
//!
//! 1. The pure call-graph builder ([`build_call_graph`]) that walks the
//!    resolved AST and records every in-package `Call` edge keyed by the
//!    caller's [`BindingId`].
//! 2. The Tarjan SCC algorithm ([`scc_partition`]) that partitions
//!    [`BindingId`] nodes into SCCs.
//! 3. The decision function ([`SccMap::is_divergent`]) that combines
//!    SCC shape with per-function `decreases` presence to decide whether
//!    each function should accumulate [`Pure(Divergence)`] at entry.
//!
//! Loop-level divergence (`loop` without `decreases`) is a node-local
//! concern, not an SCC concern — it lives at the [`super::ctrl::synth_loop`]
//! site, not here.

use std::collections::HashMap;

use edda_resolve::{BindingId, Resolved, ResolvedPackage};
use edda_syntax::ast::visit::{self as ast_visit, Visitor};
use edda_syntax::ast::{self, Expr, ExprKind, FnBody, ItemKind, RefinementKind};

/// Forward call-graph: caller → set of callees.
///
/// Built from the resolved AST: each [`ExprKind::Call`] whose callee
/// is a [`Path`] expression whose span resolves (via
/// [`ResolvedPackage::resolutions`]) to a Function [`BindingId`]
/// contributes one edge.
pub(crate) type CallGraph = HashMap<BindingId, Vec<BindingId>>;

/// SCC partition of the package's Function bindings.
///
/// Produced by [`scc_partition`] from a [`CallGraph`]. `sccs[i]` is
/// the i-th SCC's member list; `scc_index[binding]` maps a binding to
/// its SCC index and the binding's position within that SCC.
#[derive(Clone, Debug, Default)]
pub(crate) struct SccPartition {
    /// SCC member lists, in topological order (callers after callees).
    pub sccs: Vec<Vec<BindingId>>,
    /// Per-binding SCC membership.
    pub scc_index: HashMap<BindingId, SccId>,
}

/// Opaque SCC handle — an index into [`SccPartition::sccs`].
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub(crate) struct SccId(pub usize);

/// SCC partition plus the per-SCC "is this SCC divergent without
/// `decreases`?" decision. The decision is a function of (SCC shape,
/// `decreases` presence on any member) and is computed once at the
/// start of [`check_package`](crate::check_package).
#[derive(Clone, Debug, Default)]
pub(crate) struct SccMap {
    partition: SccPartition,
    divergent: Vec<bool>,
    /// Per-SCC self-edge flag — set when at least one member of the
    /// SCC calls itself directly. Needed to distinguish a recursive
    /// single-member SCC (self-edge) from a leaf SCC (no edges).
    has_self_edge: Vec<bool>,
}

impl SccMap {
    /// Empty map — every binding lookup returns `false`. Used in
    /// inference contexts that do not run a full `check_package`
    /// (unit-test fixtures, REPL).
    pub(crate) fn empty() -> Self {
        Self::default()
    }

    /// `true` iff the function at `binding` is in an SCC that the
    /// inference pass must inject [`Pure(Divergence)`] into.
    pub(crate) fn is_divergent(&self, binding: BindingId) -> bool {
        match self.partition.scc_index.get(&binding) {
            Some(SccId(idx)) => self.divergent[*idx],
            None => false,
        }
    }

    /// `true` iff the function at `binding` is in a recursive SCC — a
    /// self-loop or a mutually-recursive cycle. C5 uses this gate to
    /// decide whether to emit termination obligations on the function's
    /// `decreases` clause: non-recursive functions trivially terminate
    /// and need no per-call obligation.
    pub(crate) fn is_recursive(&self, binding: BindingId) -> bool {
        let Some(SccId(idx)) = self.partition.scc_index.get(&binding).copied() else {
            return false;
        };
        let members = &self.partition.sccs[idx];
        members.len() >= 2 || self.has_self_edge.get(idx).copied().unwrap_or(false)
    }

    /// `true` iff `a` and `b` are members of the same SCC. Used at the
    /// recursive-call obligation site to decide whether a call edge
    /// participates in the caller's termination obligation — only
    /// in-SCC edges shorten the well-founded chain; calls out of the
    /// SCC are not part of the recursion the `decreases` measure
    /// guards.
    pub(crate) fn same_scc(&self, a: BindingId, b: BindingId) -> bool {
        match (
            self.partition.scc_index.get(&a),
            self.partition.scc_index.get(&b),
        ) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        }
    }
}

/// Build the call graph for `package` by walking every function body's
/// AST and recording each [`ExprKind::Call`] whose callee resolves to
/// an in-package Function [`BindingId`].
pub(crate) fn build_call_graph(package: &ResolvedPackage) -> CallGraph {
    let resolutions = package.resolutions();
    let mut graph: CallGraph = HashMap::new();
    for module in package.modules() {
        for item in &package.module_entry(module.id).ast.items {
            let ItemKind::Function(fn_decl) = &item.kind else {
                continue;
            };
            let Some(caller) = module.items.lookup(fn_decl.name.name) else {
                continue;
            };
            graph.entry(caller).or_default();
            let FnBody::Block(body) = &fn_decl.body else {
                continue;
            };
            let mut collector = CallGraphCollector {
                resolutions,
                package,
                out: Vec::new(),
            };
            collector.visit_block(body);
            let mut callees = collector.out;
            // Dedup while preserving first-seen order so the graph is
            // deterministic against arbitrary HashMap iteration.
            callees.sort_unstable_by_key(|b| (b.module.as_usize(), b.index));
            callees.dedup();
            graph.insert(caller, callees);
        }
    }
    graph
}

/// Collects in-package callee bindings reachable from a function body
/// for the call-graph builder. The caller dedupes the resulting list.
struct CallGraphCollector<'a> {
    resolutions: &'a edda_resolve::Resolutions,
    package: &'a ResolvedPackage,
    out: Vec<BindingId>,
}

impl<'a, 'ast> Visitor<'ast> for CallGraphCollector<'a> {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        match &expr.kind {
            ExprKind::Call { callee, args } => {
                if let Some(b) = resolve_function_callee(callee, self.resolutions, self.package) {
                    self.out.push(b);
                }
                self.visit_expr(callee);
                for arg in args {
                    self.visit_expr(&arg.expr);
                }
            }
            ExprKind::Spawn(s) => {
                // Preserve pre-visitor behavior: do not descend into
                // `s.args[].init`. Whether those argument initialisers
                // should contribute to the enclosing function's call
                // graph is a separate design question — see W1 follow-up.
                self.visit_block(&s.body);
            }
            _ => ast_visit::walk_expr(self, expr),
        }
    }
}

fn resolve_function_callee(
    callee: &Expr,
    resolutions: &edda_resolve::Resolutions,
    package: &ResolvedPackage,
) -> Option<BindingId> {
    let ExprKind::Path(path) = &callee.kind else {
        return None;
    };
    let resolved = resolutions.lookup_path(path.span)?;
    let Resolved::Binding(binding) = resolved else {
        return None;
    };
    // Must be a Function binding — TypeDecl constructors and Param /
    // Local bindings can appear in call position but never participate
    // in the termination call graph.
    let entry = package.binding(binding);
    if !matches!(entry.kind, edda_resolve::BindingKind::Function) {
        return None;
    }
    Some(binding)
}

fn scc_partition(graph: &CallGraph) -> SccPartition {
    // Collect every node referenced (caller or callee) so the partition
    // is total — Tarjan operates only on nodes it visits.
    let mut nodes: Vec<BindingId> = graph.keys().copied().collect();
    for callees in graph.values() {
        for &c in callees {
            nodes.push(c);
        }
    }
    nodes.sort_unstable_by_key(|b| (b.module.as_usize(), b.index));
    nodes.dedup();

    let mut state: HashMap<BindingId, TarjanState> = HashMap::with_capacity(nodes.len());
    let mut stack: Vec<BindingId> = Vec::new();
    let mut on_stack: HashMap<BindingId, bool> = HashMap::with_capacity(nodes.len());
    let mut sccs: Vec<Vec<BindingId>> = Vec::new();
    let mut next_index: u32 = 0;

    for &node in &nodes {
        if state.contains_key(&node) {
            continue;
        }
        strongconnect(
            node,
            graph,
            &mut state,
            &mut stack,
            &mut on_stack,
            &mut sccs,
            &mut next_index,
        );
    }

    let mut scc_index: HashMap<BindingId, SccId> = HashMap::with_capacity(state.len());
    for (i, members) in sccs.iter().enumerate() {
        for &m in members {
            scc_index.insert(m, SccId(i));
        }
    }
    SccPartition { sccs, scc_index }
}

#[derive(Copy, Clone, Debug)]
struct TarjanState {
    index: u32,
    lowlink: u32,
}

fn strongconnect(
    start: BindingId,
    graph: &CallGraph,
    state: &mut HashMap<BindingId, TarjanState>,
    stack: &mut Vec<BindingId>,
    on_stack: &mut HashMap<BindingId, bool>,
    sccs: &mut Vec<Vec<BindingId>>,
    next_index: &mut u32,
) {
    // Iterative driver — Rust's stack would blow up on a deeply linear
    // call chain (`a → b → c → … → n`) if we recursed naively. The
    // frame carries the node plus the index of the next out-edge to
    // visit.
    struct Frame {
        node: BindingId,
        next_edge: usize,
    }
    let mut frames: Vec<Frame> = Vec::new();
    push_node(start, state, stack, on_stack, next_index);
    frames.push(Frame {
        node: start,
        next_edge: 0,
    });
    while let Some(top) = frames.last_mut() {
        let node = top.node;
        let edges = graph.get(&node).map(|v| v.as_slice()).unwrap_or(&[]);
        if top.next_edge < edges.len() {
            let next = edges[top.next_edge];
            top.next_edge += 1;
            if !state.contains_key(&next) {
                push_node(next, state, stack, on_stack, next_index);
                frames.push(Frame {
                    node: next,
                    next_edge: 0,
                });
                continue;
            }
            if *on_stack.get(&next).unwrap_or(&false) {
                let next_index_value = state[&next].index;
                let node_state = state.get_mut(&node).unwrap();
                node_state.lowlink = node_state.lowlink.min(next_index_value);
            }
            continue;
        }
        // All out-edges visited — pop the frame.
        let node_state = state[&node];
        if node_state.lowlink == node_state.index {
            let mut component: Vec<BindingId> = Vec::new();
            while let Some(popped) = stack.pop() {
                on_stack.insert(popped, false);
                component.push(popped);
                if popped == node {
                    break;
                }
            }
            sccs.push(component);
        }
        frames.pop();
        // Propagate the popped node's lowlink up to its parent.
        if let Some(parent) = frames.last() {
            let popped_low = node_state.lowlink;
            let parent_state = state.get_mut(&parent.node).unwrap();
            parent_state.lowlink = parent_state.lowlink.min(popped_low);
        }
    }
}

fn push_node(
    node: BindingId,
    state: &mut HashMap<BindingId, TarjanState>,
    stack: &mut Vec<BindingId>,
    on_stack: &mut HashMap<BindingId, bool>,
    next_index: &mut u32,
) {
    let idx = *next_index;
    *next_index += 1;
    state.insert(
        node,
        TarjanState {
            index: idx,
            lowlink: idx,
        },
    );
    stack.push(node);
    on_stack.insert(node, true);
}

fn compute_divergent(
    partition: &SccPartition,
    graph: &CallGraph,
    package: &ResolvedPackage,
) -> Vec<bool> {
    let mut out = vec![false; partition.sccs.len()];
    for (scc_idx, members) in partition.sccs.iter().enumerate() {
        let recursive = if members.len() >= 2 {
            true
        } else {
            // Single-member SCC — recursive iff it has a self-edge.
            let member = members[0];
            graph
                .get(&member)
                .map(|callees| callees.iter().any(|&c| c == member))
                .unwrap_or(false)
        };
        if !recursive {
            continue;
        }
        let any_decreases = members
            .iter()
            .any(|&id| function_has_decreases(id, package));
        out[scc_idx] = !any_decreases;
    }
    out
}

fn function_has_decreases(binding: BindingId, package: &ResolvedPackage) -> bool {
    let module = package.module(binding.module);
    let module_entry = package.module_entry(binding.module);
    let entry = package.binding(binding);
    let _ = module;
    let item = module_entry.ast.items.iter().find_map(|i| match &i.kind {
        ItemKind::Function(fd) if fd.name.name == entry.name => Some(fd),
        _ => None,
    });
    item.map(|fd| {
        fd.refinements
            .iter()
            .any(|c| c.kind == RefinementKind::Decreases)
    })
    .unwrap_or(false)
}

/// Build the [`SccMap`] for `package` — the per-Function-binding "must
/// I inject divergence?" answer for the inference pass.
pub(crate) fn build_scc_map(package: &ResolvedPackage) -> SccMap {
    let graph = build_call_graph(package);
    let partition = scc_partition(&graph);
    let divergent = compute_divergent(&partition, &graph, package);
    let has_self_edge = compute_self_edges(&partition, &graph);
    SccMap {
        partition,
        divergent,
        has_self_edge,
    }
}

/// Per-SCC self-edge flag — set when at least one member of the SCC
/// calls itself directly.
fn compute_self_edges(partition: &SccPartition, graph: &CallGraph) -> Vec<bool> {
    let mut out = vec![false; partition.sccs.len()];
    for (scc_idx, members) in partition.sccs.iter().enumerate() {
        out[scc_idx] = members
            .iter()
            .any(|&m| graph.get(&m).map_or(false, |cs| cs.contains(&m)));
    }
    out
}

// Silence the unused-AST-import warning when this module is compiled
// against a future ast surface that gains items without callsites.
#[allow(dead_code)]
const _: fn(&ast::Block) = |_| {};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_map_returns_false_for_every_query() {
        let m = SccMap::empty();
        let id = BindingId {
            module: edda_resolve::ModuleId::new(0),
            index: 0,
        };
        assert!(!m.is_divergent(id));
    }
}
