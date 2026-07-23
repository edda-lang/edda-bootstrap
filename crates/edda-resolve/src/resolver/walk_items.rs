//! Item-level AST walking — functions, type declarations, specs,
//! derives, spec invocations, effect rows, and type expressions. Each
//! walker declares the bindings the item introduces and resolves the
//! paths in its signature / body subtrees.

use edda_diag::DiagnosticClass;
use edda_intern::Symbol;
use edda_span::Span;
use edda_syntax::ast::{
    EffectMember, EffectRow, File, FnBody, FnDecl, Item, ItemKind, RefinementKind, Spec, Type,
    TypeDecl, TypeDeclKind, TypeKind, VariantPayload,
};

use crate::binding::BindingKind;

use super::{Resolver, emit_diag};

impl<'a, 'i> Resolver<'a, 'i> {
    pub(super) fn walk_file(&mut self, file: &File) {
        for item in &file.items {
            self.walk_item(item);
        }
        self.check_ord_requires_eq(&file.items);
    }

    /// Module-wide `ord → eq` cross-derive dependency check. For every
    /// `derive` listing `ord` whose target type has no `derive eq` in the
    /// same file, emit at the `ord` item's span. Target types are matched
    /// by their path's leaf segment symbol (the desugaring binds by leaf).
    fn check_ord_requires_eq(&mut self, items: &[Item]) {
        let eq_sym = self.cx.interner.intern("eq");
        let ord_sym = self.cx.interner.intern("ord");
        // Collect (target-leaf, ord-item-span) for every derive listing
        // `ord`, and separately the set of target leaves that have a
        // sibling `derive eq`. Both passes are read-only, so gather the
        // violation spans before touching `self.diags`.
        let mut violations: Vec<Span> = Vec::new();
        for item in items {
            let ItemKind::Derive(d) = &item.kind else {
                continue;
            };
            let Some(ord_span) = derive_item_span(&d.items, ord_sym) else {
                continue;
            };
            let Some(target) = derive_target_leaf(d) else {
                continue;
            };
            if !module_derives_eq_for(items, target, eq_sym) {
                violations.push(ord_span);
            }
        }
        for ord_span in violations {
            emit_diag(
                self.diags,
                self.lint_cfg,
                DiagnosticClass::ImportResolutionError,
                ord_span,
                "`derive ord` requires `derive eq` in the same module \
                 per corpus/edda-codex/language/04-specs-comptime.md §5.3"
                    .to_string(),
                "add `eq` to a `derive` for the same type in this module",
            );
        }
    }

    pub(super) fn walk_item(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Function(f) => self.walk_fn(f),
            ItemKind::TypeDecl(t) => self.walk_type_decl(t),
            ItemKind::Spec(s) => self.walk_spec(s),
            ItemKind::SpecInvocation(si) => self.walk_spec_invocation(si),
            ItemKind::Let(l) => self.walk_let_item(l),
            ItemKind::Import(_) | ItemKind::Module(_) => {}
            ItemKind::Derive(d) => self.walk_derive(d),
        }
    }

    /// Validate a `derive` declaration: every item must be in the closed
    /// whitelist. The target path is resolved like any cross-module
    /// reference. Cross-derive dependencies (`ord → eq`) are module-wide
    /// per codex §5.3 and checked once per file in `check_ord_requires_eq`,
    /// not here. Codegen desugaring (turning the derive into a sequence of
    /// `spec std.<path>(<target>)` invocations) is the cascade's
    /// responsibility; this pass only validates.
    pub(super) fn walk_derive(&mut self, d: &edda_syntax::ast::Derive) {
        // Resolve the target type's path like any other path-position
        // reference; downstream codegen substitutes the resolved name
        // into the generated `<type>_<derive>` short name.
        self.resolve_path(&d.target);
        // Per-item whitelist check.
        for item in &d.items {
            // Parser-recovery sentinel guard — `expect_ident` produces
            // `Symbol::DUMMY` on `derive <bad>` recovery; the parse_error
            // is already on the diagnostic take, so skip the whitelist
            // probe rather than panic inside `Interner::resolve` (which
            // rejects `Symbol::DUMMY` with "Symbol(u32::MAX) is out of
            // range"). Mirrors the DUMMY-guard pattern in
            // `register_derive_bindings` and `enqueue_derive_spec_imports`.
            let Some(name) = self.cx.interner.try_resolve(item.name) else {
                continue;
            };
            match name {
                "eq" | "ord" | "hash" | "debug" | "clone" | "properties" | "serialize"
                | "deserialize" => {}
                _ => {
                    emit_diag(
                        self.diags,
                        self.lint_cfg,
                        DiagnosticClass::ImportResolutionError,
                        item.span,
                        format!(
                            "`derive {name}` is not in the locked vocabulary; admitted items are \
                             eq, ord, hash, debug, clone, properties, serialize, deserialize"
                        ),
                        "remove the unknown derive item or replace it with an admitted one",
                    );
                }
            }
        }
    }

    pub(super) fn walk_spec_invocation(&mut self, si: &edda_syntax::ast::SpecInvocation) {
        self.resolve_path(&si.path);
        for arg in &si.args {
            self.walk_expr(arg);
        }
    }

    pub(super) fn walk_let_item(&mut self, ld: &edda_syntax::ast::LetDecl) {
        self.walk_type(&ld.ty);
        self.walk_expr(&ld.init);
    }

    pub(super) fn walk_fn(&mut self, fn_decl: &FnDecl) {
        self.enter_scope();
        for gp in fn_decl.outbound_generics.iter().chain(&fn_decl.generics) {
            self.declare(BindingKind::Param, gp.name.name, gp.name.span);
        }
        // Parameter `x` must be in scope before its own inline `where`
        // refinement walks: `x: i32 where x >= 0` — the predicate
        // references the binding being constrained.
        for p in &fn_decl.params {
            self.declare(BindingKind::Param, p.name.name, p.name.span);
            self.walk_type(&p.ty);
        }
        if let Some(rt) = &fn_decl.return_ty {
            self.walk_type(rt);
        }
        if let Some(er) = &fn_decl.effects {
            self.walk_effects(er);
        }
        // `requires` / `ensures` clauses: only `ensures` predicates may
        // name `result` (the function's return value). Bind `result` in
        // a temporary nested scope so it is visible to the predicate
        // and dropped immediately after — without this, `result` leaks
        // into the function body, where it must remain unresolved.
        for r in &fn_decl.refinements {
            match r.kind {
                RefinementKind::Ensures => {
                    self.enter_scope();
                    let result_sym = self.cx.interner.intern("result");
                    self.declare(BindingKind::Local, result_sym, r.span);
                    self.walk_expr(&r.pred);
                    self.exit_scope();
                }
                // `decreases` admits the built-in `box_depth` measure
                // identifier (see edda-types::refine::termination::box_depth)
                // alongside any in-scope binding. Pre-bind the name in a
                // temporary scope so the predicate walker's path-resolution
                // doesn't reject `box_depth(b)` as an unresolved call before
                // the refinement-discharge pass even sees the clause —
                // mirrors the `result`-in-`ensures` binding above. B-021.
                RefinementKind::Decreases => {
                    self.enter_scope();
                    let box_depth_sym = self.cx.interner.intern("box_depth");
                    self.declare(BindingKind::Local, box_depth_sym, r.span);
                    self.walk_expr(&r.pred);
                    self.exit_scope();
                }
                _ => {
                    self.walk_expr(&r.pred);
                }
            }
        }
        // Extern-bodied functions have no source body to walk —
        // params and refinement clauses still get their scope, but
        // the body block is absent. The item-table binding for the
        // function itself was already registered upstream.
        match &fn_decl.body {
            FnBody::Block(b) => self.walk_block(b),
            FnBody::Extern { .. } => {}
        }
        self.exit_scope();
    }

    pub(super) fn walk_type_decl(&mut self, t: &TypeDecl) {
        self.enter_scope();
        for gp in &t.generics {
            self.declare(BindingKind::Param, gp.name.name, gp.name.span);
        }
        match &t.kind {
            TypeDeclKind::Product { fields } => {
                let prev = self.field_refinement_siblings.replace(
                    fields
                        .iter()
                        .map(|f| (f.name.name, f.name.span))
                        .collect::<Box<[_]>>(),
                );
                for f in fields {
                    self.walk_type_field(f);
                }
                self.field_refinement_siblings = prev;
            }
            TypeDeclKind::Sum { variants } => {
                for v in variants {
                    self.walk_variant_payload(&v.payload);
                }
            }
        }
        self.exit_scope();
    }

    pub(super) fn walk_type_field(&mut self, f: &edda_syntax::ast::TypeField) {
        // Walk the field's type with no field name in scope. Inline
        // `where` clauses inside the type (`radius: f64 where radius >=
        // 0.0`, which `parse_ty` turns into `TypeKind::Refined`) reach
        // back into the sibling-fields scope through the
        // `field_refinement_siblings` stash and the `Refined` arm of
        // `walk_type` — see that arm for the predicate-side push.
        self.walk_type(&f.ty);
        if let Some(r) = &f.refinement {
            // Trailing field-level refinement (`f.refinement`): same
            // sibling-fields-in-scope discipline as inline `where`.
            self.enter_scope();
            self.declare_sibling_fields_for_refinement();
            self.walk_expr(r);
            self.exit_scope();
        }
    }

    pub(super) fn walk_variant_payload(&mut self, payload: &VariantPayload) {
        match payload {
            VariantPayload::Unit => {}
            VariantPayload::Tuple(tys) => {
                for t in tys {
                    self.walk_type(t);
                }
            }
            VariantPayload::Struct(fields) => {
                let prev = self.field_refinement_siblings.replace(
                    fields
                        .iter()
                        .map(|f| (f.name.name, f.name.span))
                        .collect::<Box<[_]>>(),
                );
                for f in fields {
                    self.walk_type_field(f);
                }
                self.field_refinement_siblings = prev;
            }
        }
    }

    /// Declare every sibling field name from the active type-decl /
    /// struct-variant payload into the current scope. No-op when not
    /// walking a field refinement (the resolver only sets
    /// `field_refinement_siblings` while inside a product / struct-variant
    /// body — every other caller leaves it `None`).
    pub(super) fn declare_sibling_fields_for_refinement(&mut self) {
        let siblings: Vec<(Symbol, Span)> = self
            .field_refinement_siblings
            .as_deref()
            .map(|s| s.to_vec())
            .unwrap_or_default();
        for (name, span) in siblings {
            self.declare(BindingKind::Local, name, span);
        }
    }

    pub(super) fn walk_spec(&mut self, s: &Spec) {
        self.enter_scope();
        for gp in &s.generics {
            self.declare(BindingKind::Param, gp.name.name, gp.name.span);
        }
        for clause in &s.where_clauses {
            self.walk_expr(&clause.pred);
        }
        self.declare_spec_body_items(&s.body);
        for item in &s.body {
            self.walk_item(item);
        }
        self.exit_scope();
    }

    /// CA1 pass-1 pre-walk over a spec body: declare every Function /
    /// TypeDecl / Let / SpecInvocation in the active scope so the
    /// subsequent per-item walks see sibling references regardless of
    /// declaration order. Imports / `module` overrides are not admitted
    /// inside a spec body (per `Spec.body` invariant).
    pub(super) fn declare_spec_body_items(&mut self, body: &[Item]) {
        for item in body {
            match &item.kind {
                ItemKind::Function(f) => {
                    self.declare(BindingKind::Function, f.name.name, f.name.span);
                }
                ItemKind::TypeDecl(t) => {
                    self.declare(BindingKind::TypeDecl, t.name.name, t.name.span);
                }
                ItemKind::Let(l) => {
                    self.declare(BindingKind::Const, l.name.name, l.name.span);
                }
                ItemKind::Spec(s) => {
                    self.declare(BindingKind::Spec, s.name.name, s.name.span);
                }
                ItemKind::SpecInvocation(si) => {
                    if let Some(name) =
                        crate::spec_mangling::mangle_spec_invocation_name(si, self.cx.interner)
                    {
                        self.declare(BindingKind::SpecInvocation, name, si.span);
                    }
                }
                ItemKind::Import(_) | ItemKind::Module(_) => {}
                // `derive` is a top-level form; nested in a spec body it
                // would be a parse error. For C1 we
                // simply do not declare any binding for it.
                ItemKind::Derive(_) => {}
            }
        }
    }

    pub(super) fn walk_effects(&mut self, er: &EffectRow) {
        for m in &er.members {
            match m {
                EffectMember::Capability(_) => {}
                EffectMember::Named { name: _, ty } => self.walk_type(ty),
                EffectMember::Spread(p) => {
                    self.resolve_path(p);
                }
                EffectMember::Graded { kind: _, bound } => {
                    // Walk the RHS of the `<resource_var> <= EXPR` bound
                    // so the parameter-referencing LIA lift in
                    // `edda_types::graded_refine` can resolve param paths
                    // through the resolver's span-keyed resolutions map.
                    // The LHS is the kind's resource variable
                    // (`bytes`/`calls`/`ops`), validated separately at
                    // sig-lowering and intentionally not a binding — we
                    // walk only the RHS to avoid emitting
                    // `unresolved_path` on it.
                    if let edda_syntax::ast::ExprKind::Binary {
                        op: edda_syntax::ast::BinOp::Le,
                        rhs,
                        ..
                    } = &bound.kind
                    {
                        self.walk_expr(rhs);
                    }
                }
            }
        }
    }

    pub(super) fn walk_type(&mut self, t: &Type) {
        match &t.kind {
            TypeKind::Path(p) => {
                if !self.is_primitive_type_path(p) {
                    self.resolve_path(p);
                }
            }
            TypeKind::Tuple(ts) => {
                for t in ts {
                    self.walk_type(t);
                }
            }
            TypeKind::Slice(t) => self.walk_type(t),
            TypeKind::Unit | TypeKind::Meta | TypeKind::Error => {}
            TypeKind::Function { params, ret, effects } => {
                for p in params {
                    self.walk_type(&p.ty);
                }
                self.walk_type(ret);
                if let Some(e) = effects {
                    self.walk_effects(e);
                }
            }
            TypeKind::Comptime(t) => self.walk_type(t),
            TypeKind::Refined { base, pred } => {
                self.walk_type(base);
                // Field-position refinements (inline `where` on a record
                // field's type) need every sibling field in scope so
                // cross-field constraints resolve. Non-field refinements
                // (e.g. on a function parameter's type) get nothing here —
                // the parameter binding is already in scope from
                // `walk_fn`.
                let in_field_position = self.field_refinement_siblings.is_some();
                if in_field_position {
                    self.enter_scope();
                    self.declare_sibling_fields_for_refinement();
                }
                self.walk_expr(pred);
                if in_field_position {
                    self.exit_scope();
                }
            }
        }
    }
}

/// Leaf symbol of a derive's target type path (`derive ord for a.b.Point`
/// → the `Point` segment). `None` when the path is empty (parser recovery).
/// The derive desugaring binds instances by this leaf, so cross-derive
/// dependency matching is leaf-keyed, mirroring the native compiler's
/// `module_derives_eq_for`.
fn derive_target_leaf(d: &edda_syntax::ast::Derive) -> Option<Symbol> {
    d.target.segments.last().map(|seg| seg.name)
}

/// Span of the derive item named `name` within `items`, or `None` if
/// absent. Used to anchor the `ord → eq` diagnostic at the offending
/// `ord` item rather than the whole declaration.
fn derive_item_span(items: &[edda_syntax::ast::Ident], name: Symbol) -> Option<Span> {
    items.iter().find(|i| i.name == name).map(|i| i.span)
}

/// Does any `derive` in `items` (the module's top-level items) list `eq`
/// for the type whose target-path leaf is `target`? Codex §5.3 admits the
/// `ord → eq` dependency being satisfied by any sibling declaration in the
/// same module, not just the same declaration set.
fn module_derives_eq_for(items: &[Item], target: Symbol, eq_sym: Symbol) -> bool {
    items.iter().any(|item| {
        let ItemKind::Derive(d) = &item.kind else {
            return false;
        };
        derive_target_leaf(d) == Some(target) && d.items.iter().any(|i| i.name == eq_sym)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use edda_intern::Interner;
    use edda_syntax::ast::{Derive, Ident, Path};

    fn ident(interner: &Interner, s: &str) -> Ident {
        Ident { name: interner.intern(s), span: Span::DUMMY }
    }

    fn derive_item(interner: &Interner, target: &str, items: &[&str]) -> Item {
        let d = Derive {
            span: Span::DUMMY,
            items: items.iter().map(|i| ident(interner, i)).collect(),
            target: Path {
                segments: vec![ident(interner, target)],
                span: Span::DUMMY,
            },
        };
        Item {
            span: Span::DUMMY,
            doc: vec![],
            attributes: vec![],
            kind: ItemKind::Derive(Box::new(d)),
        }
    }

    #[test]
    fn ord_and_eq_on_same_declaration_is_satisfied() {
        let i = Interner::new();
        let items = vec![derive_item(&i, "Point", &["eq", "ord"])];
        assert!(module_derives_eq_for(
            &items,
            i.intern("Point"),
            i.intern("eq")
        ));
    }

    #[test]
    fn eq_on_a_separate_declaration_in_the_same_module_is_satisfied() {
        let i = Interner::new();
        let items = vec![
            derive_item(&i, "Point", &["eq"]),
            derive_item(&i, "Point", &["ord"]),
        ];
        assert!(module_derives_eq_for(
            &items,
            i.intern("Point"),
            i.intern("eq")
        ));
    }

    #[test]
    fn ord_with_no_eq_anywhere_in_module_is_unsatisfied() {
        let i = Interner::new();
        let items = vec![derive_item(&i, "Point", &["ord"])];
        assert!(!module_derives_eq_for(
            &items,
            i.intern("Point"),
            i.intern("eq")
        ));
    }

    #[test]
    fn eq_for_a_different_target_type_does_not_satisfy() {
        let i = Interner::new();
        let items = vec![
            derive_item(&i, "Other", &["eq"]),
            derive_item(&i, "Point", &["ord"]),
        ];
        assert!(!module_derives_eq_for(
            &items,
            i.intern("Point"),
            i.intern("eq")
        ));
    }

    #[test]
    fn derive_item_span_locates_present_and_absent_items() {
        let i = Interner::new();
        let items: Vec<Ident> = ["eq", "ord"].iter().map(|s| ident(&i, s)).collect();
        assert!(derive_item_span(&items, i.intern("ord")).is_some());
        assert!(derive_item_span(&items, i.intern("hash")).is_none());
    }
}
