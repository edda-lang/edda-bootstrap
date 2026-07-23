//! Path resolution — the per-position dispatch from an AST [`Path`]
//! node to a [`Resolved`] (local binding, top-level item, import-leaf
//! module, spec-invocation alias, or cross-module item), plus the
//! catalogue-head / primitive short-circuits.

use edda_intern::Symbol;
use edda_syntax::ast::{Path as AstPath, Visibility};

use crate::binding::{BindingId, BindingKind};
use crate::graph::ModuleId;
use crate::path::ModulePath;
use crate::resolutions::Resolved;

use super::{PathPos, Resolver, is_catalogue_head};

impl<'a, 'i> Resolver<'a, 'i> {
    /// `true` when `path` is a single-segment reference to a primitive
    /// type. The resolver does not record such paths in its
    /// resolution map — primitives are not scope-resident; the
    /// typechecker resolves them by name via the locked
    /// `Primitive` catalogue.
    pub(super) fn is_primitive_type_path(&self, path: &AstPath) -> bool {
        if path.segments.len() != 1 {
            return false;
        }
        // Parser-recovery sentinel — never a catalogue head; bail
        // before reaching the interner so the DUMMY symbol does not
        // hit `Interner::resolve`'s out-of-range panic.
        if path.segments[0].name == Symbol::DUMMY {
            return false;
        }
        is_catalogue_head(self.cx.interner.resolve(path.segments[0].name))
    }

    pub(super) fn resolve_path_pos(&mut self, path: &AstPath, pos: PathPos) -> Resolved {
        // Parser-recovery sentinel: zero-segment paths and paths whose
        // head segment is `Symbol::DUMMY` are produced by error-recovery
        // sites in `edda-syntax`. The parse_error has already been
        // pushed onto the diagnostic take, so we record `Resolved::Error`
        // for the span (so any later span-keyed consult sees it) and
        // return without invoking the interner — `cx.interner.resolve`
        // panics on DUMMY, and the catalogue-head probe / unresolved-
        // path diagnostic builder below both reach into it.
        if crate::resolve::is_recovery_path(path) {
            self.record(path.span, Resolved::Error);
            return Resolved::Error;
        }
        let head = path.segments[0].name;
        if let Some(id) = self.lookup_local(head) {
            // The position-split rule: in TYPE position a multi-segment path whose
            // head is shadowed by a foreign leaf import dispatches through
            // the leaf — `method: method.Method` is module-qualified
            // access. In VALUE position the same `method.field` is
            // field/method access on the local, so we skip the flip and
            // fall through to record `Resolved::Binding(local)`; edda-types
            // decomposes the chain (retiring the `_mod`-alias workaround).
            //
            // B-014 self-alias exclusion: the synthetic leaf for the
            // current module (added by `build_import_leaf_table`) points
            // back at self; a param named after the module leaf must keep
            // ordinary access, so we only flip when the leaf names a
            // foreign module.
            if pos == PathPos::Type
                && path.segments.len() > 1
                && let Some(leaf) = self.module().leaf_imports.lookup(head)
                && leaf.module != self.module_id
            {
                let target = leaf.module;
                self.used_leaves.insert(head);
                return self.resolve_after_leaf(path, target);
            }
            let r = Resolved::Binding(id);
            self.record(path.span, r);
            return r;
        }
        if let Some(id) = self.module().items.lookup(head) {
            // Spec-invocation alias: after the cascade restart, the
            // SpecInvocation binding has a generated module that hosts the
            // monomorphised TypeDecl + functions. Dispatch through it so the
            // typechecker sees the generated decl / function bindings, not
            // the placeholder SpecInvocation binding.
            let entry_kind = self.module().items.get(id).kind;
            if entry_kind == BindingKind::SpecInvocation
                && let Some(&(gen_module_id, gen_typedecl_id)) =
                    self.spec_inv_targets.get(&id)
            {
                return self.resolve_spec_invocation_alias(
                    path,
                    gen_module_id,
                    gen_typedecl_id,
                );
            }
            // C13: when a multi-segment path's head names a local
            // Function or Const item that shadows an imported module,
            // prefer the import. Functions and consts have no nested
            // namespace — they cannot be a dot-path prefix — so the
            // user must have meant the import. Single-segment paths
            // (direct calls / value references) still resolve to the
            // shadowing function, so callers of the local item are
            // unaffected.
            if path.segments.len() > 1
                && matches!(entry_kind, BindingKind::Function | BindingKind::Const)
                && let Some(target) = self
                    .module()
                    .leaf_imports
                    .lookup(head)
                    .map(|l| l.module)
            {
                self.used_leaves.insert(head);
                return self.resolve_after_leaf(path, target);
            }
            let r = Resolved::Binding(id);
            self.record(path.span, r);
            return r;
        }
        let leaf_entry = self.module().leaf_imports.lookup(head).copied();
        if let Some(entry) = leaf_entry {
            self.used_leaves.insert(head);
            // Selected-name leaf: `head` names an ITEM
            // inside `entry.module`, not the module itself — resolve
            // straight through `resolve_cross_module_item` (the same
            // lookup a qualified `<leaf>.<item>` reference performs)
            // rather than `resolve_after_leaf`, which would bind a
            // single-segment path to `Resolved::Module`.
            if entry.is_selected {
                return self.resolve_cross_module_item(path, entry.module, head);
            }
            return self.resolve_after_leaf(path, entry.module);
        }
        // Catalogue head — primitive (`f64`, `String`), capability (`Stdout`,
        // `Allocator`), or comptime built-in / reserved name (`size_of`,
        // `field_count`). These are not scope items at any layer; the
        // typechecker / comptime evaluator / codegen handle them via their
        // respective catalogues. Multi-segment paths headed by a
        // primitive-static-method catalogue entry also land here: the
        // resolver stays silent on the head, and any downstream
        // "primitive-associated function not found" diagnostic is the
        // typechecker's to make.
        if is_catalogue_head(self.cx.interner.resolve(head)) {
            return Resolved::Error;
        }
        if path.segments.len() >= 2 {
            return self.resolve_full_dotpath(path);
        }
        self.emit_unresolved(path);
        self.record(path.span, Resolved::Error);
        Resolved::Error
    }

    /// `true` when `path` is a single-segment, unshadowed reference to
    /// the `offset_of` comptime built-in. Used by
    /// [`super::walk_exprs::Resolver::walk_direct_call`] to skip
    /// value-binding resolution of `offset_of`'s field-name argument —
    /// the bootstrap-side half of a parity fix.
    pub(super) fn is_unshadowed_offset_of(&self, path: &AstPath) -> bool {
        if path.segments.len() != 1 {
            return false;
        }
        let head = path.segments[0].name;
        if head == Symbol::DUMMY {
            return false;
        }
        if self.lookup_local(head).is_some() {
            return false;
        }
        if self.module().items.lookup(head).is_some() {
            return false;
        }
        if self.module().leaf_imports.lookup(head).is_some() {
            return false;
        }
        self.cx.interner.resolve(head) == "offset_of"
    }

    /// Resolve a path in type / module-qualified position — the default
    /// for every call site except the expression-position value path in
    /// `walk_expr`, which calls [`Self::resolve_path_pos`] with
    /// [`PathPos::Value`] so `head.tail` on a Param/Local head is treated
    /// as field/method access.
    pub(super) fn resolve_path(&mut self, path: &AstPath) -> Resolved {
        self.resolve_path_pos(path, PathPos::Type)
    }

    /// Resolve a path whose head is a SpecInvocation binding that has a
    /// known generated module + TypeDecl (populated by
    /// `build_spec_inv_targets`). See the invariants for the dispatch
    /// matrix.
    pub(super) fn resolve_spec_invocation_alias(
        &mut self,
        path: &AstPath,
        gen_module_id: ModuleId,
        gen_typedecl_id: Option<BindingId>,
    ) -> Resolved {
        if path.segments.len() == 1 {
            // A single-segment reference resolves to the eponymous nominal
            // so type-position uses unify with the codegen-produced type.
            // A function-only spec has no such nominal — a bare
            // reference to its placeholder is unresolvable.
            let Some(typedecl_id) = gen_typedecl_id else {
                self.emit_unresolved(path);
                self.record(path.span, Resolved::Error);
                return Resolved::Error;
            };
            let r = Resolved::Binding(typedecl_id);
            self.record(path.span, r);
            return r;
        }
        if path.segments.len() == 2 || path.segments.len() == 3 {
            let member = path.segments[1].name;
            // Try a top-level item in the generated module first
            // (`StringMap_f64.new` → the generated `new` function). This
            // is the path that makes a function-only spec's members
            // (`HashMap_..._.get`) resolve — it needs only the module
            // link, not the eponymous typedecl.
            let gen_module = &self.modules[gen_module_id.as_usize()];
            if let Some(item_id) = gen_module.items.lookup(member) {
                let entry = gen_module.items.get(item_id);
                if entry.visibility != Visibility::Public {
                    self.emit_visibility(path, member, gen_module_id);
                    self.record(path.span, Resolved::Error);
                    return Resolved::Error;
                }
                let r = Resolved::Binding(item_id);
                self.record(path.span, r);
                return r;
            }
            // No top-level item with that name — fall back to the
            // generated TypeDecl binding so the typechecker resolves
            // `.member` as a variant constructor / field against the
            // sum / product layout (`Option_f64.some` → variant on
            // `Option`). Absent for a function-only spec → unresolved.
            let Some(typedecl_id) = gen_typedecl_id else {
                self.emit_unresolved(path);
                self.record(path.span, Resolved::Error);
                return Resolved::Error;
            };
            let r = Resolved::Binding(typedecl_id);
            self.record(path.span, r);
            return r;
        }
        self.emit_unresolved(path);
        self.record(path.span, Resolved::Error);
        Resolved::Error
    }

    pub(super) fn resolve_after_leaf(&mut self, path: &AstPath, target: ModuleId) -> Resolved {
        if path.segments.len() == 1 {
            let r = Resolved::Module(target);
            self.record(path.span, r);
            return r;
        }
        // 2-segment `<leaf>.<item>` — cross-module item access.
        // 3-segment `<leaf>.<TypeDecl>.<variant>` — record the
        // TypeDecl binding for the whole span; the typechecker
        // resolves the trailing variant against the sum layout via
        // `path.segments.last()` (mirrors the in-module
        // `<TypeDecl>.<variant>` path).
        if path.segments.len() == 2 || path.segments.len() == 3 {
            let item = path.segments[1].name;
            return self.resolve_cross_module_item(path, target, item);
        }
        self.emit_unresolved(path);
        self.record(path.span, Resolved::Error);
        Resolved::Error
    }

    pub(super) fn resolve_full_dotpath(&mut self, path: &AstPath) -> Resolved {
        // First try the full path as a module reference — if it names a
        // spec-generated module, dispatch through its alias TypeDecl so
        // qualified spec args (`spec StringMap(std.mem.alloc.Box_Payload)`)
        // and qualified field types (`value: std.mem.alloc.Box_Payload`)
        // both resolve to the generated `Box` TypeDecl instead of failing
        // with `unresolved item Box_Payload in module std.mem.alloc`.
        let full: Vec<Symbol> = path.segments.iter().map(|s| s.name).collect();
        let full_mp = ModulePath::new(full);
        if let Some(full_target) = self.graph.lookup_by_path(&full_mp) {
            if let Some(&alias_typedecl) = self.module_aliases.get(&full_target) {
                self.mark_target_module_imports_used(full_target);
                let r = Resolved::Binding(alias_typedecl);
                self.record(path.span, r);
                return r;
            }
        }
        let last = path.segments.len() - 1;
        let item_name = path.segments[last].name;
        let prefix: Vec<Symbol> = path.segments[..last].iter().map(|s| s.name).collect();
        let mp = ModulePath::new(prefix);
        let Some(target) = self.graph.lookup_by_path(&mp) else {
            // Dotted `target.supports(Cap)` comptime built-in.
            // `target` is not a real
            // module; the typechecker + comptime evaluator answer this
            // via the per-target cap-availability catalogue
            // (`edda_target::TargetTriple::supports_capability`). Stay
            // silent so they handle it — the same short-circuit the
            // single-segment catalogue heads take above. Placed inside
            // the module-miss arm so a genuine `target` module would
            // keep precedence; only the unresolved case lands here.
            if path.segments.len() == 2
                && self.cx.interner.resolve(path.segments[0].name) == "target"
                && self.cx.interner.resolve(path.segments[1].name) == "supports"
            {
                self.record(path.span, Resolved::Error);
                return Resolved::Error;
            }
            self.emit_unresolved_modulepath(path, &mp);
            self.record(path.span, Resolved::Error);
            return Resolved::Error;
        };
        self.mark_target_module_imports_used(target);
        self.resolve_cross_module_item(path, target, item_name)
    }

    /// Mark every leaf import in the current module whose target
    /// module is `target` as used, so the `unused_import` lint does
    /// not fire when the user references that target via a
    /// fully-qualified path.
    pub(super) fn mark_target_module_imports_used(&mut self, target: ModuleId) {
        let leaves: Vec<Symbol> = self
            .module()
            .leaf_imports
            .entries()
            .iter()
            .filter(|e| e.module == target)
            .map(|e| e.leaf)
            .collect();
        for leaf in leaves {
            self.used_leaves.insert(leaf);
        }
    }

    pub(super) fn resolve_cross_module_item(
        &mut self,
        path: &AstPath,
        target: ModuleId,
        item: Symbol,
    ) -> Resolved {
        let target_module = &self.modules[target.as_usize()];
        let Some(id) = target_module.items.lookup(item) else {
            self.emit_unresolved_item(path, item, target);
            self.record(path.span, Resolved::Error);
            return Resolved::Error;
        };
        if target != self.module_id {
            let entry = target_module.items.get(id);
            if entry.visibility != Visibility::Public {
                self.emit_visibility(path, item, target);
                self.record(path.span, Resolved::Error);
                return Resolved::Error;
            }
        }
        let r = Resolved::Binding(id);
        self.record(path.span, r);
        r
    }
}
