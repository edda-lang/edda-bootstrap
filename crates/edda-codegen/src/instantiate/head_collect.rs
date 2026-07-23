//! Post-substitution path-head collection for the import filter.
//!
//! Split out from `instantiate/mod.rs` for file-size reasons. Walks the
//! substituted spec-body items and gathers the head [`Symbol`] of every
//! `Path` reference; [`super::instantiate_spec`] feeds the resulting set
//! to `import_binding_in_use` to drop parent imports whose binding the
//! body never references (B-018).

use std::collections::HashSet;

use edda_intern::Symbol;
use edda_syntax::ast::visit::{Visitor, walk_expr};
use edda_syntax::ast::{
    EffectMember, EffectRow, Expr, ExprKind, FnBody, Item, ItemKind, Path, RefinementClause, Type,
    TypeDeclKind, TypeKind, VariantPayload,
};

/// Walk the substituted items and collect the head [`Symbol`] of every
/// `Path` reference (expressions, types, effect-row spreads / payloads,
/// spec invocations, refinement clauses, derive targets). The returned
/// set is the input to [`super::instantiate_spec`]'s import filter.
pub(super) fn collect_used_heads(items: &[Item]) -> HashSet<Symbol> {
    let mut collector = HeadCollector { heads: HashSet::new() };
    for item in items {
        collector.visit_item(item);
    }
    collector.heads
}

/// Visitor state for [`collect_used_heads`].
struct HeadCollector {
    heads: HashSet<Symbol>,
}

impl HeadCollector {
    fn record_path_head(&mut self, path: &Path) {
        if let Some(first) = path.segments.first() {
            self.heads.insert(first.name);
        }
    }

    fn visit_item(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Function(decl) => self.visit_fn_decl(decl),
            ItemKind::TypeDecl(decl) => match &decl.kind {
                TypeDeclKind::Product { fields } => {
                    for field in fields {
                        self.visit_type(&field.ty);
                        if let Some(pred) = &field.refinement {
                            self.visit_expr(pred);
                        }
                    }
                }
                TypeDeclKind::Sum { variants } => {
                    for variant in variants {
                        self.visit_variant_payload(&variant.payload);
                    }
                }
            },
            ItemKind::Let(decl) => {
                self.visit_type(&decl.ty);
                self.visit_expr(&decl.init);
            }
            ItemKind::SpecInvocation(inv) => {
                self.record_path_head(&inv.path);
                for arg in &inv.args {
                    self.visit_expr(arg);
                }
            }
            ItemKind::Derive(d) => self.record_path_head(&d.target),
            // Spec declarations don't appear in the substituted body
            // (the body IS the spec); Imports and Module decls don't
            // reference anything we need to filter on.
            ItemKind::Spec(_) | ItemKind::Import(_) | ItemKind::Module(_) => {}
        }
    }

    fn visit_fn_decl(&mut self, decl: &edda_syntax::ast::FnDecl) {
        for param in &decl.params {
            self.visit_type(&param.ty);
        }
        for generic in &decl.generics {
            if let Some(ty) = &generic.ty {
                self.visit_type(ty);
            }
        }
        if let Some(ret) = &decl.return_ty {
            self.visit_type(ret);
        }
        if let Some(row) = &decl.effects {
            self.visit_effect_row(row);
        }
        for clause in &decl.refinements {
            self.visit_refinement_clause(clause);
        }
        if let FnBody::Block(block) = &decl.body {
            <Self as Visitor<'_>>::visit_block(self, block);
        }
    }

    fn visit_variant_payload(&mut self, payload: &VariantPayload) {
        match payload {
            VariantPayload::Unit => {}
            VariantPayload::Tuple(tys) => {
                for ty in tys {
                    self.visit_type(ty);
                }
            }
            VariantPayload::Struct(fields) => {
                for field in fields {
                    self.visit_type(&field.ty);
                    if let Some(pred) = &field.refinement {
                        self.visit_expr(pred);
                    }
                }
            }
        }
    }

    fn visit_type(&mut self, ty: &Type) {
        match &ty.kind {
            TypeKind::Path(p) => self.record_path_head(p),
            TypeKind::Tuple(elems) => {
                for elem in elems {
                    self.visit_type(elem);
                }
            }
            TypeKind::Slice(inner) => self.visit_type(inner),
            TypeKind::Function { params, ret, effects } => {
                for p in params {
                    self.visit_type(&p.ty);
                }
                self.visit_type(ret);
                if let Some(row) = effects {
                    self.visit_effect_row(row);
                }
            }
            TypeKind::Refined { base, pred } => {
                self.visit_type(base);
                self.visit_expr(pred);
            }
            TypeKind::Comptime(inner) => self.visit_type(inner),
            TypeKind::Unit | TypeKind::Meta | TypeKind::Error => {}
        }
    }

    fn visit_effect_row(&mut self, row: &EffectRow) {
        for member in &row.members {
            match member {
                EffectMember::Capability(ident) => {
                    self.heads.insert(ident.name);
                }
                EffectMember::Named { ty, .. } => self.visit_type(ty),
                EffectMember::Spread(path) => self.record_path_head(path),
                EffectMember::Graded { .. } => {}
            }
        }
    }

    fn visit_refinement_clause(&mut self, clause: &RefinementClause) {
        self.visit_expr(&clause.pred);
    }

    fn visit_expr(&mut self, expr: &Expr) {
        <Self as Visitor<'_>>::visit_expr(self, expr);
    }
}

impl<'ast> Visitor<'ast> for HeadCollector {
    fn visit_expr(&mut self, expr: &'ast Expr) {
        match &expr.kind {
            ExprKind::Path(p) => self.record_path_head(p),
            ExprKind::EffectRow(row) => self.visit_effect_row(row),
            ExprKind::Cast { ty, .. } => self.visit_type(ty),
            ExprKind::StructLit { path, .. } => self.record_path_head(path),
            _ => {}
        }
        walk_expr(self, expr);
    }
}
