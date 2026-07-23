//! Top-level dispatchers — `write_item` over [`ItemKind`] and the
//! `write_spec_body` entry that produces the bytes filling
//! [`crate::CanonicalForm::canonical_body`].
//!
//! Composes the shared encoder building blocks via the [`Encoder`] impl
//! blocks in [`super::encoder`], [`super::exprs`], [`super::literals`],
//! and [`super::items`].

use edda_syntax::ast::{Import, ItemKind, ModuleDecl, Spec, SpecInvocation};

use super::encoder::{checked_u32, Encoder};
use super::tags;

impl<'a> Encoder<'a> {
    //   the variant's payload bytes
    /// Encode an [`ItemKind`] AST node, emitting its
    /// [`tags::item_kind`] tag followed by the variant's payload.
    pub fn write_item(&mut self, kind: &ItemKind) {
        match kind {
            ItemKind::Function(decl) => {
                self.push_byte(tags::item_kind::FUNCTION);
                self.write_fn_decl(decl);
            }
            ItemKind::TypeDecl(decl) => {
                self.push_byte(tags::item_kind::TYPE_DECL);
                self.write_type_decl(decl);
            }
            ItemKind::Spec(spec) => {
                self.push_byte(tags::item_kind::SPEC);
                self.write_spec(spec);
            }
            ItemKind::SpecInvocation(si) => {
                self.push_byte(tags::item_kind::SPEC_INVOCATION);
                self.write_spec_invocation(si);
            }
            ItemKind::Import(imp) => {
                self.push_byte(tags::item_kind::IMPORT);
                self.write_import(imp);
            }
            ItemKind::Module(md) => {
                self.push_byte(tags::item_kind::MODULE);
                self.write_module_decl(md);
            }
            ItemKind::Let(_) => {
                self.push_byte(tags::item_kind::LET);
            }
            // `derive <items> for <target>` — encoded as items count +
            // each item ident + resolved target path. Added at
            // BodyVersion(0x04). Deep desugaring to spec invocations
            // happens at codegen-expansion time in C7.
            ItemKind::Derive(d) => {
                self.push_byte(tags::item_kind::DERIVE);
                self.write_u32_le(checked_u32(d.items.len()));
                for item in &d.items {
                    self.write_ident(item);
                }
                let qualified = self.resolver().resolve_path(&d.target);
                self.write_length_prefixed_str(qualified.as_str());
            }
        }
    }

    /// Encode a [`SpecInvocation`]: resolved path + comma-separated args.
    pub fn write_spec_invocation(&mut self, si: &SpecInvocation) {
        let qualified = self.resolver().resolve_path(&si.path);
        self.write_length_prefixed_str(qualified.as_str());
        self.write_u32_le(checked_u32(si.args.len()));
        for arg in &si.args {
            self.write_expr(arg);
        }
    }

    //   resolution is what makes alias-renames invisible to the hash
    //   (`spec-language.md` §9)
    /// Encode an [`Import`]: the imported path resolved to its
    /// fully-qualified form.
    pub fn write_import(&mut self, imp: &Import) {
        let qualified = self.resolver().resolve_path(&imp.path);
        self.write_length_prefixed_str(qualified.as_str());
    }

    /// Encode a [`ModuleDecl`]: the override path resolved to its
    /// fully-qualified form.
    pub fn write_module_decl(&mut self, md: &ModuleDecl) {
        let qualified = self.resolver().resolve_path(&md.path);
        self.write_length_prefixed_str(qualified.as_str());
    }

    //   body items); for the entry-point case where the outer spec's
    //   name is already in the [`crate::CanonicalForm::spec_qualified`]
    //   field, use [`Encoder::write_spec_body`] instead
    //   accessibility marker per `declarations.md`, not a body-affecting
    //   change; spec-body bytes hash to the same value regardless of
    //   `public` / `module` placement
    /// Encode a [`Spec`] as an item: name + generics + body items.
    pub fn write_spec(&mut self, spec: &Spec) {
        self.write_ident(&spec.name);
        self.write_u32_le(checked_u32(spec.generics.len()));
        for gp in &spec.generics {
            self.write_generic_param(gp);
        }
        self.write_u32_le(checked_u32(spec.body.len()));
        for item in &spec.body {
            self.write_item(&item.kind);
        }
    }

    //   already part of the [`crate::CanonicalForm::spec_qualified`]
    //   field which the §2 byte serialiser hashes alongside the
    //   body bytes produced by this method
    //   `spec-language.md` §9 (a change to a comptime parameter's name,
    //   type, or position is a body-affecting change)
    //   §253; the per-item walk uses [`Encoder::write_item`] so each
    //   variant carries its kind tag inline
    /// Top-level entry: encode the bytes that fill
    /// [`crate::CanonicalForm::canonical_body`] for one spec
    /// invocation.
    pub fn write_spec_body(&mut self, spec: &Spec) {
        self.write_u32_le(checked_u32(spec.generics.len()));
        for gp in &spec.generics {
            self.write_generic_param(gp);
        }
        self.write_u32_le(checked_u32(spec.body.len()));
        for item in &spec.body {
            self.write_item(&item.kind);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::test_support::{ident, path, PassThroughResolver};
    use edda_intern::Interner;
    use edda_syntax::ast::{
        Block, FnBody, FnDecl, GenericKind, GenericParam, Import, ItemKind, ModuleDecl, Spec,
        TypeDecl, TypeDeclKind, Visibility,
    };

    fn empty_block() -> Block {
        Block {
            span: edda_span::Span::DUMMY,
            stmts: vec![],
            trailing: None,
        }
    }

    fn empty_spec_body() -> Vec<edda_syntax::ast::Item> {
        Vec::new()
    }

    fn fn_decl(interner: &Interner, name: &str) -> FnDecl {
        FnDecl {
            span: edda_span::Span::DUMMY,
            stability: None,
            visibility: Visibility::Module,
            refinement_stable: false,
            name: ident(interner, name),
            outbound_generics: vec![],
            generics: vec![],
            params: vec![],
            return_ty: None,
            return_mode: edda_syntax::ast::ReturnMode::ByValue,
            effects: None,
            refinements: vec![],
            body: FnBody::Block(empty_block()),
        }
    }

    fn type_decl(interner: &Interner, name: &str) -> TypeDecl {
        TypeDecl {
            span: edda_span::Span::DUMMY,
            stability: None,
            visibility: Visibility::Module,
            linearity: None,
            name: ident(interner, name),
            generics: vec![],
            kind: TypeDeclKind::Product { fields: vec![] },
        }
    }

    #[test]
    fn item_function_writes_kind_then_fn_decl() {
        let interner = Interner::new();
        let resolver = PassThroughResolver::new(&interner);
        let mut enc = Encoder::new(&interner, &resolver);
        enc.write_item(&ItemKind::Function(Box::new(fn_decl(&interner, "f"))));
        let bytes = enc.into_bytes();
        assert_eq!(bytes[0], tags::item_kind::FUNCTION);
        // FnDecl: visibility MODULE, name "f" (u32 len + 1 byte)
        assert_eq!(bytes[1], tags::visibility::MODULE);
        assert_eq!(&bytes[2..6], &1u32.to_le_bytes());
        assert_eq!(bytes[6], b'f');
    }

    #[test]
    fn item_type_decl_writes_kind_then_payload() {
        let interner = Interner::new();
        let resolver = PassThroughResolver::new(&interner);
        let mut enc = Encoder::new(&interner, &resolver);
        enc.write_item(&ItemKind::TypeDecl(Box::new(type_decl(&interner, "T"))));
        let bytes = enc.into_bytes();
        assert_eq!(bytes[0], tags::item_kind::TYPE_DECL);
        assert_eq!(bytes[1], tags::visibility::MODULE);
        assert_eq!(&bytes[2..6], &1u32.to_le_bytes());
        assert_eq!(bytes[6], b'T');
    }

    #[test]
    fn item_import_writes_kind_then_resolved_path() {
        let interner = Interner::new();
        let resolver = PassThroughResolver::new(&interner);
        let mut enc = Encoder::new(&interner, &resolver);
        enc.write_item(&ItemKind::Import(Import {
            span: edda_span::Span::DUMMY,
            path: path(&interner, &["std", "option"]),
            alias: None,
            selection: None,
        }));
        let bytes = enc.into_bytes();
        assert_eq!(bytes[0], tags::item_kind::IMPORT);
        assert_eq!(&bytes[1..5], &("std.option".len() as u32).to_le_bytes());
        assert_eq!(&bytes[5..], b"std.option");
    }

    #[test]
    fn item_module_decl_writes_kind_then_resolved_path() {
        let interner = Interner::new();
        let resolver = PassThroughResolver::new(&interner);
        let mut enc = Encoder::new(&interner, &resolver);
        enc.write_item(&ItemKind::Module(ModuleDecl {
            span: edda_span::Span::DUMMY,
            path: path(&interner, &["foo"]),
        }));
        let bytes = enc.into_bytes();
        assert_eq!(bytes[0], tags::item_kind::MODULE);
        assert_eq!(&bytes[1..5], &3u32.to_le_bytes());
        assert_eq!(&bytes[5..], b"foo");
    }

    #[test]
    fn item_spec_writes_kind_then_name_and_body() {
        let interner = Interner::new();
        let resolver = PassThroughResolver::new(&interner);
        let mut enc = Encoder::new(&interner, &resolver);
        enc.write_item(&ItemKind::Spec(Box::new(Spec {
            span: edda_span::Span::DUMMY,
            stability: None,
            visibility: Visibility::Module,
            name: ident(&interner, "Stack"),
            generics: vec![],
            where_clauses: vec![],
            body: empty_spec_body(),
        })));
        let bytes = enc.into_bytes();
        assert_eq!(bytes[0], tags::item_kind::SPEC);
        // name "Stack": u32(5) + "Stack"(5)
        assert_eq!(&bytes[1..5], &5u32.to_le_bytes());
        assert_eq!(&bytes[5..10], b"Stack");
        // 0 generics
        assert_eq!(&bytes[10..14], &0u32.to_le_bytes());
        // 0 body items
        assert_eq!(&bytes[14..18], &0u32.to_le_bytes());
        assert_eq!(bytes.len(), 18);
    }

    #[test]
    fn item_kinds_are_distinct_in_byte_output() {
        let interner = Interner::new();
        let resolver = PassThroughResolver::new(&interner);
        let mk_bytes = |kind: ItemKind| {
            let mut enc = Encoder::new(&interner, &resolver);
            enc.write_item(&kind);
            enc.into_bytes()
        };
        let fn_bytes = mk_bytes(ItemKind::Function(Box::new(fn_decl(&interner, "f"))));
        let ty_bytes = mk_bytes(ItemKind::TypeDecl(Box::new(type_decl(&interner, "f"))));
        assert_ne!(fn_bytes, ty_bytes);
        assert_eq!(fn_bytes[0], tags::item_kind::FUNCTION);
        assert_eq!(ty_bytes[0], tags::item_kind::TYPE_DECL);
    }

    #[test]
    fn spec_body_omits_name_but_encodes_generics() {
        let interner = Interner::new();
        let resolver = PassThroughResolver::new(&interner);
        let mut enc = Encoder::new(&interner, &resolver);
        enc.write_spec_body(&Spec {
            span: edda_span::Span::DUMMY,
            stability: None,
            visibility: Visibility::Module,
            name: ident(&interner, "Stack"), // not in output
            generics: vec![GenericParam {
                span: edda_span::Span::DUMMY,
                kind: GenericKind::Comptime,
                name: ident(&interner, "n"),
                ty: None,
                admits: vec![],
            }],
            where_clauses: vec![],
            body: empty_spec_body(),
        });
        let bytes = enc.into_bytes();
        // First 4 bytes: generic count = 1
        assert_eq!(&bytes[0..4], &1u32.to_le_bytes());
        // Generic param: COMPTIME tag + ident "n"
        assert_eq!(bytes[4], tags::generic_kind::COMPTIME);
        assert_eq!(&bytes[5..9], &1u32.to_le_bytes());
        assert_eq!(bytes[9], b'n');
        // Then 0 body items
        assert_eq!(&bytes[10..14], &0u32.to_le_bytes());
        assert_eq!(bytes.len(), 14);
    }

    #[test]
    fn spec_body_name_change_does_not_affect_bytes() {
        // The outer spec's name lives in CanonicalForm.spec_qualified,
        // not in the body bytes — confirm by hashing two specs with
        // identical bodies but different names.
        let interner = Interner::new();
        let resolver = PassThroughResolver::new(&interner);
        let mk = |name: &str| Spec {
            span: edda_span::Span::DUMMY,
            stability: None,
            visibility: Visibility::Module,
            name: ident(&interner, name),
            generics: vec![],
            where_clauses: vec![],
            body: empty_spec_body(),
        };
        let mut a = Encoder::new(&interner, &resolver);
        let mut b = Encoder::new(&interner, &resolver);
        a.write_spec_body(&mk("Stack"));
        b.write_spec_body(&mk("Queue"));
        assert_eq!(a.into_bytes(), b.into_bytes());
    }

    #[test]
    fn spec_body_generic_change_does_affect_bytes() {
        let interner = Interner::new();
        let resolver = PassThroughResolver::new(&interner);
        let mk = |generics: Vec<GenericParam>| Spec {
            span: edda_span::Span::DUMMY,
            stability: None,
            visibility: Visibility::Module,
            name: ident(&interner, "S"),
            generics,
            where_clauses: vec![],
            body: empty_spec_body(),
        };
        let mut a = Encoder::new(&interner, &resolver);
        let mut b = Encoder::new(&interner, &resolver);
        a.write_spec_body(&mk(vec![]));
        b.write_spec_body(&mk(vec![GenericParam {
            span: edda_span::Span::DUMMY,
            kind: GenericKind::Type,
            name: ident(&interner, "T"),
            ty: None,
            admits: vec![],
        }]));
        assert_ne!(a.into_bytes(), b.into_bytes());
    }
}
