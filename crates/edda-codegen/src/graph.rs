//! Cascade dependency-graph walker — the `migration.md` §3 step-3
//! traversal that follows a hash change from a root-set of edited
//! specs through their downstream consumers.
//!
//! This module provides the *graph* and the *walk*; [`crate::CodegenSession`]
//! consumes the resulting [`CascadePlan`]
//! to stage regenerated bodies. Hash recomputation is the caller's
//! responsibility — the walker returns a topological order over
//! potentially-affected artifacts; the caller iterates the plan,
//! re-renders each artifact, computes its new hash via
//! [`crate::compute_hash`], and stops the cascade at any node whose
//! recomputed hash matches its prior recorded hash (per `migration.md`
//! §3: "hashes match → no new artifact → cascade stops").
//!
//! # Why does the walker not recompute hashes?
//!
//! Recomputing a downstream consumer's hash requires its *structural*
//! [`CanonicalForm`] — `canonical_body` bytes, the original
//! [`crate::ArgumentTuple`], the nested set with hashes substituted —
//! and the cache layer's [`edda_cache::ArtifactInputs`] only stores
//! the printable form. The structural form lives in the source +
//! resolver + monomorphization output, not in the manifest. Pushing
//! hash recomputation onto the caller keeps this module free of the
//! source-rendering dependency and lets the cascade walker test in
//! isolation against any [`edda_cache::Manifest`].

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use edda_cache::{ArtifactEntry, Manifest};
use smol_str::SmolStr;

use crate::error::CodegenError;

/// Forward and reverse dependency indices over a [`Manifest`].
///
/// Keys are artifact short names (the mangled form, without the
/// `__<hash-prefix>` tail). The forward edge `a → b` means `a`'s
/// [`ArtifactInputs::nested_deps`] includes `b` — `a` depends on `b`.
/// The reverse edge `b → a` is the inverse: `b`'s downstream
/// consumer `a` reaches into it.
///
/// The reverse index is the §3 cascade primitive: when an upstream
/// artifact's hash changes, the reverse-reachable subgraph from that
/// artifact is the candidate set for regeneration.
///
/// [`ArtifactInputs::nested_deps`]: edda_cache::ArtifactInputs::nested_deps
pub struct CascadeGraph<'a> {
    forward: BTreeMap<SmolStr, Vec<SmolStr>>,
    reverse: BTreeMap<SmolStr, Vec<SmolStr>>,
    entries: BTreeMap<SmolStr, &'a ArtifactEntry>,
}

impl<'a> CascadeGraph<'a> {
    /// Build a [`CascadeGraph`] from `manifest`. The graph borrows
    /// the manifest's entries; the manifest must outlive the graph.
    pub fn from_manifest(manifest: &'a Manifest) -> Self {
        let mut forward: BTreeMap<SmolStr, Vec<SmolStr>> = BTreeMap::new();
        let mut reverse: BTreeMap<SmolStr, Vec<SmolStr>> = BTreeMap::new();
        let mut entries: BTreeMap<SmolStr, &'a ArtifactEntry> = BTreeMap::new();
        for entry in &manifest.artifacts {
            entries.insert(entry.short_name.clone(), entry);
            forward.entry(entry.short_name.clone()).or_default();
            reverse.entry(entry.short_name.clone()).or_default();
        }
        for entry in &manifest.artifacts {
            for dep in &entry.inputs.nested_deps {
                forward
                    .entry(entry.short_name.clone())
                    .or_default()
                    .push(dep.clone());
                if entries.contains_key(dep) {
                    reverse
                        .entry(dep.clone())
                        .or_default()
                        .push(entry.short_name.clone());
                }
            }
        }
        CascadeGraph {
            forward,
            reverse,
            entries,
        }
    }

    /// Number of artifacts in the graph.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` if the graph has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Borrow the [`ArtifactEntry`] for `short`, if any.
    pub fn entry(&self, short: &str) -> Option<&'a ArtifactEntry> {
        self.entries.get(short).copied()
    }

    /// Short names of artifacts whose `nested_deps` include `short`
    /// (its downstream consumers, in arbitrary stable order).
    pub fn consumers_of(&self, short: &str) -> &[SmolStr] {
        self.reverse.get(short).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Short names of artifacts that `short` depends on (its upstream
    /// nested invocations, in declaration order).
    pub fn dependencies_of(&self, short: &str) -> &[SmolStr] {
        self.forward.get(short).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// Regeneration plan for one cascade.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CascadePlan {
    /// Short names of artifacts that may need regeneration, in
    /// **upstream-first** topological order.
    ///
    /// The caller iterates this list, re-renders each artifact, and
    /// computes its new hash. Per `migration.md` §3, when a
    /// recomputed hash matches the prior recorded hash no new
    /// artifact is written and the cascade stops at that node;
    /// downstream consumers remain in the plan but the caller must
    /// skip regenerating them in that case. The plan does not encode
    /// "stop here" — every reverse-reachable node is included and the
    /// caller filters by observed hash equality.
    pub regenerate_order: Vec<SmolStr>,
}

/// Walk the reverse cascade graph from `roots`.
///
/// `roots` is the set of artifact short names whose source has
/// changed — typically the artifacts directly affected by the
/// user's edit. The walker returns every artifact transitively
/// reachable from a root through reverse edges, in upstream-first
/// topological order so the caller processes each node before its
/// consumers.
pub fn walk_cascade(
    graph: &CascadeGraph<'_>,
    roots: &[SmolStr],
) -> Result<CascadePlan, CodegenError> {
    let subgraph = collect_reverse_reachable(graph, roots);
    let order = kahn_sort(graph, &subgraph)?;
    Ok(CascadePlan {
        regenerate_order: order,
    })
}

/// BFS from `roots` through reverse edges. Returns the set of
/// visited nodes including roots.
fn collect_reverse_reachable(graph: &CascadeGraph<'_>, roots: &[SmolStr]) -> BTreeSet<SmolStr> {
    let mut visited: BTreeSet<SmolStr> = BTreeSet::new();
    let mut queue: VecDeque<SmolStr> = VecDeque::new();
    for root in roots {
        if visited.insert(root.clone()) {
            queue.push_back(root.clone());
        }
    }
    while let Some(node) = queue.pop_front() {
        for consumer in graph.consumers_of(&node) {
            if visited.insert(consumer.clone()) {
                queue.push_back(consumer.clone());
            }
        }
    }
    visited
}

/// Kahn's topological sort restricted to `subgraph`.
///
/// Nodes outside the subgraph are ignored entirely; an edge to a
/// node outside the subgraph does not contribute to the in-degree
/// count.
fn kahn_sort(
    graph: &CascadeGraph<'_>,
    subgraph: &BTreeSet<SmolStr>,
) -> Result<Vec<SmolStr>, CodegenError> {
    let mut in_degree: BTreeMap<SmolStr, usize> = BTreeMap::new();
    for node in subgraph {
        let parents_in_sub = graph
            .dependencies_of(node)
            .iter()
            .filter(|p| subgraph.contains(*p))
            .count();
        in_degree.insert(node.clone(), parents_in_sub);
    }
    let mut ready: VecDeque<SmolStr> = in_degree
        .iter()
        .filter_map(|(name, deg)| (*deg == 0).then(|| name.clone()))
        .collect();
    let mut order: Vec<SmolStr> = Vec::with_capacity(subgraph.len());
    while let Some(node) = ready.pop_front() {
        order.push(node.clone());
        for consumer in graph.consumers_of(&node) {
            if !subgraph.contains(consumer) {
                continue;
            }
            let deg = in_degree
                .get_mut(consumer)
                .expect("consumer in subgraph must have an in-degree entry");
            *deg = deg.saturating_sub(1);
            if *deg == 0 {
                ready.push_back(consumer.clone());
            }
        }
    }
    if order.len() != subgraph.len() {
        let involved: Vec<SmolStr> = subgraph
            .iter()
            .filter(|n| !order.contains(n))
            .cloned()
            .collect();
        return Err(CodegenError::CascadeCycle { involved });
    }
    Ok(order)
}
