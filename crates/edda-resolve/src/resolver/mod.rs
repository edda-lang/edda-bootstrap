//! Intra-function lexical resolver — walks each module's AST, declares
//! every binding occurrence (function params, generic params,
//! pattern bindings, `let` / `var` locals), and resolves every Path
//! node in expression, type, and pattern position to a [`Resolved`].
//!
//! The resolver is per-module: one [`Resolver`] instance runs over
//! one [`crate::ResolvedModule`]'s file AST and produces (a) a flat
//! list of Param + Local [`BindingEntry`]s appended after the
//! module's top-level items and (b) a span-keyed [`Resolutions`]
//! map. The driver in `module.rs` runs one per module and merges
//! the per-module outputs into the final [`crate::ResolvedPackage`].

mod diagnostics;
mod paths;
mod walk_exprs;
mod walk_items;

use std::collections::{HashMap, HashSet};

use edda_diag::{Diagnostic, DiagnosticClass, Diagnostics, LintConfig};
use edda_intern::Symbol;
use edda_span::Span;
use edda_syntax::ast::{Path as AstPath, Visibility};

use crate::binding::{BindingEntry, BindingId, BindingKind};
use crate::graph::{ModuleId, ResolvedSourceGraph};
use crate::module::ResolvedModule;
use crate::resolutions::Resolved;
use crate::resolve::ResolveCx;

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub(super) enum PathPos {
    /// Expression / value position — `head.tail` on a Param/Local head
    /// is field / method access on that binding.
    Value,
    /// Type / pattern / module-qualified position — `head.tail` is a
    /// qualified path through the module namespace.
    Type,
}

struct Resolver<'a, 'i> {
    cx: &'a ResolveCx<'i>,
    module_id: ModuleId,
    graph: &'a ResolvedSourceGraph,
    modules: &'a [ResolvedModule],
    spec_inv_targets: &'a HashMap<BindingId, (ModuleId, Option<BindingId>)>,
    module_aliases: &'a HashMap<ModuleId, BindingId>,
    diags: &'a mut Diagnostics,
    lint_cfg: &'a LintConfig,
    locals: Vec<BindingEntry>,
    scopes: Vec<HashMap<Symbol, BindingId>>,
    used_leaves: HashSet<Symbol>,
    resolutions: HashMap<Span, Resolved>,
    field_refinement_siblings: Option<Box<[(Symbol, Span)]>>,
}

/// Drive a per-module walk. Produces the appended Param / Local
/// [`BindingEntry`]s, the Path resolution map, and the set of
/// import-leaf symbols this module's source explicitly references;
/// the caller composes these into the package-level
/// [`crate::ResolvedPackage`].
///
/// `unused_import` is **not** emitted from this function — the
/// driver re-emits it after later passes (typecheck's
/// method-resolution and intrinsic-call accounting) have contributed
/// their own "used" signals via
/// [`crate::ResolvedPackage::mark_leaf_used`]. See the
/// [`crate::emit_unused_import_lints`] entry point.
pub(crate) fn resolve_module(
    module_id: ModuleId,
    graph: &ResolvedSourceGraph,
    modules: &[ResolvedModule],
    spec_inv_targets: &HashMap<BindingId, (ModuleId, Option<BindingId>)>,
    module_aliases: &HashMap<ModuleId, BindingId>,
    cx: &ResolveCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> (Vec<BindingEntry>, HashMap<Span, Resolved>, HashSet<Symbol>) {
    let entry = graph.module(module_id);
    let mut resolver = Resolver {
        cx,
        module_id,
        graph,
        modules,
        spec_inv_targets,
        module_aliases,
        diags,
        lint_cfg,
        locals: Vec::new(),
        scopes: Vec::new(),
        used_leaves: HashSet::new(),
        resolutions: HashMap::new(),
        field_refinement_siblings: None,
    };
    resolver.walk_file(&entry.ast);
    (resolver.locals, resolver.resolutions, resolver.used_leaves)
}

impl<'a, 'i> Resolver<'a, 'i> {
    fn module(&self) -> &ResolvedModule {
        &self.modules[self.module_id.as_usize()]
    }

    fn enter_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn exit_scope(&mut self) {
        self.scopes.pop();
    }

    fn declare(&mut self, kind: BindingKind, name: Symbol, span: Span) -> BindingId {
        let item_count = self.module().items.len();
        let index = (item_count + self.locals.len()) as u32;
        let id = BindingId::new(self.module_id, index);
        self.locals.push(BindingEntry {
            kind,
            name,
            span,
            module: self.module_id,
            visibility: Visibility::Module,
        });
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, id);
        }
        id
    }

    fn lookup_local(&self, name: Symbol) -> Option<BindingId> {
        self.scopes
            .iter()
            .rev()
            .find_map(|s| s.get(&name).copied())
    }

    fn record(&mut self, span: Span, resolved: Resolved) {
        self.resolutions.insert(span, resolved);
    }
}

/// `true` when `name` is the head of a path that downstream layers
/// resolve via the locked primitive / capability / comptime-built-in
/// catalogues. The resolver stays silent on such paths so it does not
/// shadow the typechecker's, comptime evaluator's, or codegen's
/// catalogue lookup.
pub(super) fn is_catalogue_head(name: &str) -> bool {
    matches!(
        name,
        // edda_types::Primitive — signed integers
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize"
        // edda_types::Primitive — unsigned integers
        | "u8" | "u16" | "u32" | "u64" | "u128" | "usize"
        // edda_types::Primitive — floats, bool, never, meta, string, codepoint, heap pointer
        | "f32" | "f64" | "bool" | "never" | "Type" | "String" | "Codepoint" | "HeapPtr"
        // edda_types::CapabilityType
        | "Clock" | "MonotonicClock" | "Stdout" | "Stderr" | "Stdin"
        | "Allocator" | "Filesystem" | "Network" | "Random"
        | "Executor" | "ReadOnlyFilesystem" | "SandboxedFilesystem"
        | "LocalhostNetwork" | "RestrictedNetwork" | "BoundedAllocator"
        | "DeterministicRandom" | "Subprocess" | "Debugger"
        // edda_comptime::Builtin — layout / target
        | "size_of" | "align_of" | "offset_of" | "target_has"
        // edda_comptime::Builtin — reflective introspection
        | "field_count" | "field_name_at" | "field_type_at"
        | "is_signed" | "is_unsigned" | "is_integer"
        | "is_floating" | "is_numeric" | "is_primitive" | "is_sum"
        | "is_container"
        // D-22 runtime intrinsic — sum-discriminant read
        | "discriminant_of"
    )
}

pub(super) fn render_ast_path(path: &AstPath, cx: &ResolveCx<'_>) -> String {
    let mut out = String::new();
    for (i, seg) in path.segments.iter().enumerate() {
        if i > 0 {
            out.push('.');
        }
        out.push_str(cx.interner.resolve(seg.name));
    }
    out
}

pub(super) fn emit_diag(
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
    class: DiagnosticClass,
    span: Span,
    message: String,
    note: &'static str,
) {
    let severity = lint_cfg.effective(class);
    diags.push(Diagnostic::new(class, severity, span, message).with_note(note));
}
