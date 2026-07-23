//! Item-level encoders — visibility, generics, params, type
//! declarations, function declarations, and refinement clauses.
//!
//! Each method dispatches on the variant kind (where applicable), emits
//! the tag byte from [`super::tags`], and writes the variant's payload
//! by composing the shared encoder building blocks (`write_type`,
//! `write_effect_row`, `write_expr`, `write_block`, etc.).

use edda_syntax::ast::{
    FnBody, FnDecl, GenericKind, GenericParam, Param, RefinementClause, RefinementKind, TypeDecl,
    TypeDeclKind, TypeField, Variant, VariantPayload, Visibility,
};

use crate::body::encoder::{checked_u32, Encoder};
use crate::body::tags;

#[cfg(test)]
mod tests;

impl<'a> Encoder<'a> {
    /// Encode a [`Visibility`] as a single kind tag byte.
    pub fn write_visibility(&mut self, v: Visibility) {
        let tag = match v {
            Visibility::Module => tags::visibility::MODULE,
            Visibility::Public => tags::visibility::PUBLIC,
        };
        self.push_byte(tag);
    }

    /// Encode a [`GenericKind`] as a single kind tag byte.
    pub fn write_generic_kind(&mut self, k: GenericKind) {
        let tag = match k {
            GenericKind::Type => tags::generic_kind::TYPE,
            GenericKind::Comptime => tags::generic_kind::COMPTIME,
        };
        self.push_byte(tag);
    }

    /// Encode a [`GenericParam`]: kind + name.
    pub fn write_generic_param(&mut self, gp: &GenericParam) {
        self.write_generic_kind(gp.kind);
        self.write_ident(&gp.name);
    }

    /// Encode a [`Param`]: mode + name + type.
    pub fn write_param(&mut self, p: &Param) {
        self.write_param_mode(p.mode);
        self.write_ident(&p.name);
        self.write_type(&p.ty);
    }

    //   predicate from `declarations.md`; absent on most fields, this
    //   helper writes a `None` flag in that case
    /// Encode a [`TypeField`]: name + type + optional refinement
    /// predicate expression.
    pub fn write_type_field(&mut self, f: &TypeField) {
        self.write_ident(&f.name);
        self.write_type(&f.ty);
        self.write_optional_expr(f.refinement.as_ref());
    }

    /// Encode a [`Variant`]: name + payload.
    pub fn write_variant(&mut self, v: &Variant) {
        self.write_ident(&v.name);
        self.write_variant_payload(&v.payload);
    }

    /// Encode a [`VariantPayload`]: tag byte + payload bytes.
    pub fn write_variant_payload(&mut self, p: &VariantPayload) {
        match p {
            VariantPayload::Unit => {
                self.push_byte(tags::variant_payload::UNIT);
            }
            VariantPayload::Tuple(types) => {
                self.push_byte(tags::variant_payload::TUPLE);
                self.write_u32_le(checked_u32(types.len()));
                for t in types {
                    self.write_type(t);
                }
            }
            VariantPayload::Struct(fields) => {
                self.push_byte(tags::variant_payload::STRUCT);
                self.write_u32_le(checked_u32(fields.len()));
                for field in fields {
                    self.write_type_field(field);
                }
            }
        }
    }

    /// Encode a [`RefinementKind`] as a single kind tag byte.
    pub fn write_refinement_kind(&mut self, k: RefinementKind) {
        let tag = match k {
            RefinementKind::Where => tags::refinement_kind::WHERE,
            RefinementKind::Requires => tags::refinement_kind::REQUIRES,
            RefinementKind::Ensures => tags::refinement_kind::ENSURES,
            RefinementKind::Decreases => tags::refinement_kind::DECREASES,
        };
        self.push_byte(tag);
    }

    /// Encode a [`RefinementClause`]: clause kind + predicate
    /// expression.
    pub fn write_refinement_clause(&mut self, c: &RefinementClause) {
        self.write_refinement_kind(c.kind);
        self.write_expr(&c.pred);
    }

    //   appended afterwards as a u32-le count + each item
    fn write_type_decl_kind(&mut self, k: &TypeDeclKind) {
        match k {
            TypeDeclKind::Product { fields } => {
                self.push_byte(tags::type_decl_kind::PRODUCT);
                self.write_u32_le(checked_u32(fields.len()));
                for field in fields {
                    self.write_type_field(field);
                }
            }
            TypeDeclKind::Sum { variants } => {
                self.push_byte(tags::type_decl_kind::SUM);
                self.write_u32_le(checked_u32(variants.len()));
                for variant in variants {
                    self.write_variant(variant);
                }
            }
        }
    }

    /// Encode a [`TypeDecl`]: visibility + name + generic params +
    /// product-or-sum body.
    pub fn write_type_decl(&mut self, td: &TypeDecl) {
        self.write_visibility(td.visibility);
        self.write_ident(&td.name);
        self.write_u32_le(checked_u32(td.generics.len()));
        for gp in &td.generics {
            self.write_generic_param(gp);
        }
        self.write_type_decl_kind(&td.kind);
    }

    //   parser produces an empty block for abstract / external functions
    //   when those forms eventually land
    /// Encode an [`FnDecl`]: visibility, name, generics, params, return
    /// type, effect row, refinement clauses, and body block.
    pub fn write_fn_decl(&mut self, fd: &FnDecl) {
        self.write_visibility(fd.visibility);
        self.write_ident(&fd.name);
        self.write_u32_le(checked_u32(fd.generics.len()));
        for gp in &fd.generics {
            self.write_generic_param(gp);
        }
        self.write_u32_le(checked_u32(fd.params.len()));
        for p in &fd.params {
            self.write_param(p);
        }
        self.write_optional_type(fd.return_ty.as_ref());
        self.write_optional_effect_row(fd.effects.as_ref());
        self.write_u32_le(checked_u32(fd.refinements.len()));
        for clause in &fd.refinements {
            self.write_refinement_clause(clause);
        }
        self.write_fn_body(&fd.body);
    }

    /// Encode an [`FnBody`]: a single discriminator byte followed by
    /// either the block bytes (for source-bodied functions) or the
    /// length-prefixed extern-symbol bytes plus the optional
    /// importing-DLL bytes (for extern-bodied functions). Added at
    /// `BodyVersion(0x03)` to admit extern declarations in spec-body
    /// item slots; the dll clause bytes were added at
    /// `BodyVersion(0x07)`.
    pub fn write_fn_body(&mut self, body: &FnBody) {
        match body {
            FnBody::Block(b) => {
                self.push_byte(tags::fn_body::BLOCK);
                self.write_block(b);
            }
            FnBody::Extern { name, dll, .. } => {
                self.push_byte(tags::fn_body::EXTERN);
                let resolved = self.interner().resolve(*name).to_string();
                self.write_length_prefixed_str(&resolved);
                match dll {
                    Some(dll) => {
                        self.push_byte(1);
                        let resolved = self.interner().resolve(*dll).to_string();
                        self.write_length_prefixed_str(&resolved);
                    }
                    None => self.push_byte(0),
                }
            }
        }
    }
}
