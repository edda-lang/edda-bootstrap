//! Item-level walker helpers for [`super::walk::substitute_spec_body`].
//!
//! Split out from `walk.rs` for file-size reasons. These methods clone-
//! and-rewrite the per-item shapes admitted in a spec body — `function`,
//! `type`, module-level `let`, and nested `spec` invocations
//! (`stdlib/collections/hashmap/src/hashmap.ea` uses `spec std.core.option.Option(V)`
//! inside the `StringMap(V)` body to expand `Option_V` per
//! instantiation). The remaining item kinds (Import / Module / nested
//! Spec declaration) pass through unchanged so the walker remains
//! total; the parser is responsible for rejecting them at the body
//! boundary.

use edda_syntax::ast::{
    FnBody, FnDecl, GenericParam, Item, ItemKind, LetDecl, Param, RefinementClause, SpecInvocation,
    TypeDecl, TypeDeclKind, TypeField, Variant, VariantPayload,
};

use super::walk::Walker;

impl<'a> Walker<'a> {
    pub(super) fn item(&self, i: &Item) -> Item {
        Item {
            span: i.span,
            doc: i.doc.clone(),
            attributes: i.attributes.clone(),
            kind: self.item_kind(&i.kind),
        }
    }

    fn item_kind(&self, k: &ItemKind) -> ItemKind {
        match k {
            ItemKind::Function(fd) => ItemKind::Function(Box::new(self.fn_decl(fd))),
            ItemKind::TypeDecl(td) => ItemKind::TypeDecl(Box::new(self.type_decl(td))),
            ItemKind::Let(ld) => ItemKind::Let(Box::new(self.let_decl(ld))),
            ItemKind::SpecInvocation(si) => {
                ItemKind::SpecInvocation(Box::new(self.spec_invocation(si)))
            }
            ItemKind::Spec(s) => ItemKind::Spec(s.clone()),
            ItemKind::Import(i) => ItemKind::Import(i.clone()),
            ItemKind::Module(m) => ItemKind::Module(m.clone()),
            ItemKind::Derive(d) => ItemKind::Derive(d.clone()),
        }
    }

    //   are fixed references, never bound generics. Only the argument
    //   expressions are walked so e.g. `spec std.option.Option(V)` inside
    //   `StringMap(V)` becomes `spec std.option.Option(f64)` after binding
    //   `V := f64`.
    fn spec_invocation(&self, si: &SpecInvocation) -> SpecInvocation {
        SpecInvocation {
            span: si.span,
            path: si.path.clone(),
            args: si.args.iter().map(|a| self.expr(a)).collect(),
        }
    }

    fn fn_decl(&self, fd: &FnDecl) -> FnDecl {
        FnDecl {
            span: fd.span,
            stability: fd.stability,
            visibility: fd.visibility,
            refinement_stable: fd.refinement_stable,
            name: fd.name,
            outbound_generics: fd
                .outbound_generics
                .iter()
                .map(|g| self.generic_param(g))
                .collect(),
            generics: fd.generics.iter().map(|g| self.generic_param(g)).collect(),
            params: fd.params.iter().map(|p| self.param(p)).collect(),
            return_ty: fd.return_ty.as_ref().map(|t| self.ty(t)),
            return_mode: fd.return_mode,
            effects: fd.effects.as_ref().map(|e| self.effect_row(e)),
            refinements: fd
                .refinements
                .iter()
                .map(|c| self.refinement_clause(c))
                .collect(),
            body: match &fd.body {
                FnBody::Block(b) => FnBody::Block(self.block(b)),
                FnBody::Extern { span, name_span, name, dll } => FnBody::Extern {
                    span: *span,
                    name_span: *name_span,
                    name: *name,
                    dll: *dll,
                },
            },
        }
    }

    fn generic_param(&self, g: &GenericParam) -> GenericParam {
        GenericParam {
            span: g.span,
            kind: g.kind,
            name: g.name,
            ty: g.ty.as_ref().map(|t| self.ty(t)),
            // admits clauses pass through unchanged — substitution
            // does not currently rewrite operator / constant / member
            // atoms.
            admits: g.admits.clone(),
        }
    }

    fn param(&self, p: &Param) -> Param {
        Param {
            span: p.span,
            name: p.name,
            mode: p.mode,
            ty: self.ty(&p.ty),
        }
    }

    fn refinement_clause(&self, c: &RefinementClause) -> RefinementClause {
        RefinementClause {
            span: c.span,
            kind: c.kind,
            pred: self.expr(&c.pred),
        }
    }

    fn type_decl(&self, td: &TypeDecl) -> TypeDecl {
        TypeDecl {
            span: td.span,
            stability: td.stability,
            visibility: td.visibility,
            linearity: td.linearity,
            name: td.name,
            generics: td.generics.iter().map(|g| self.generic_param(g)).collect(),
            kind: match &td.kind {
                TypeDeclKind::Product { fields } => TypeDeclKind::Product {
                    fields: fields.iter().map(|f| self.type_field(f)).collect(),
                },
                TypeDeclKind::Sum { variants } => TypeDeclKind::Sum {
                    variants: variants.iter().map(|v| self.variant(v)).collect(),
                },
            },
        }
    }

    fn type_field(&self, f: &TypeField) -> TypeField {
        TypeField {
            span: f.span,
            name: f.name,
            ty: self.ty(&f.ty),
            refinement: f.refinement.as_ref().map(|e| self.expr(e)),
        }
    }

    fn variant(&self, v: &Variant) -> Variant {
        Variant {
            span: v.span,
            name: v.name,
            payload: match &v.payload {
                VariantPayload::Unit => VariantPayload::Unit,
                VariantPayload::Tuple(tys) => {
                    VariantPayload::Tuple(tys.iter().map(|t| self.ty(t)).collect())
                }
                VariantPayload::Struct(fs) => {
                    VariantPayload::Struct(fs.iter().map(|f| self.type_field(f)).collect())
                }
            },
        }
    }

    fn let_decl(&self, ld: &LetDecl) -> LetDecl {
        LetDecl {
            span: ld.span,
            visibility: ld.visibility,
            name: ld.name,
            ty: self.ty(&ld.ty),
            init: self.expr(&ld.init),
        }
    }
}
