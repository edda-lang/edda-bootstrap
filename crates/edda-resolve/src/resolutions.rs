//! Path resolution result type and the span-keyed lookup map.

use std::collections::HashMap;

use edda_span::Span;

use crate::binding::BindingId;
use crate::graph::ModuleId;

/// What a Path AST node resolves to. Single-segment paths typically
/// resolve to a [`Resolved::Binding`] (local, param, or top-level
/// item) or [`Resolved::Module`] (an import-leaf naming a module);
/// multi-segment paths via import-leaves or full dot-paths resolve
/// to a [`Resolved::Binding`] of the target item. Variant / field
/// access (`Color.red`) records the head as a Binding and defers
/// the tail to typecheck.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum Resolved {
    /// Path resolved to a binding — local, param, top-level item,
    /// or a cross-module item via leaf or full dot-path.
    Binding(BindingId),
    /// Path resolved to an imported module via its leaf name.
    Module(ModuleId),
    /// Resolution failed; a diagnostic has already been emitted.
    Error,
}

/// Span-keyed map of resolved Path AST nodes. The intra-function pass
/// populates this for every Path that appears in expression / type / pattern
/// position; downstream passes (typecheck) consume it via
/// [`Resolutions::lookup_path`].
#[derive(Clone, Debug, Default)]
pub struct Resolutions {
    paths: HashMap<Span, Resolved>,
}

impl Resolutions {
    /// Construct an empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up the resolution of a Path AST node by its span.
    pub fn lookup_path(&self, span: Span) -> Option<Resolved> {
        self.paths.get(&span).copied()
    }

    /// Record a resolution. Later writes overwrite earlier ones at
    /// the same span — only the resolver itself should call this.
    pub(crate) fn insert(&mut self, span: Span, resolved: Resolved) {
        self.paths.insert(span, resolved);
    }

    /// Record a resolution for cross-crate test fixtures.
    ///
    /// Production code populates [`Resolutions`] via the resolver
    /// itself; this constructor lets downstream crates'
    /// inline tests build a minimal resolution map without
    /// reproducing the full source-graph + resolution pipeline.
    pub fn record_for_testing(&mut self, span: Span, resolved: Resolved) {
        self.paths.insert(span, resolved);
    }

    /// Number of resolved Path nodes.
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    /// `true` when no resolutions have been recorded.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Iterate every [`Resolved`] value recorded in this map. Order is
    /// `HashMap` iteration order — non-deterministic. Used by
    /// reachability-style lints (e.g. `dead_private_function`) that
    /// only need the set of referenced binding handles.
    pub fn values(&self) -> impl Iterator<Item = Resolved> + '_ {
        self.paths.values().copied()
    }
}
