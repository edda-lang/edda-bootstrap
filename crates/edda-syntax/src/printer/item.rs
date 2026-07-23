//! Item pretty-printer plus the public file-level entry point.

use edda_intern::Interner;

use super::Printer;
use crate::ast::{
    File, FnBody, FnDecl, GenericKind, GenericParam, Import, Item, ItemKind, LetDecl, Linearity,
    ModuleDecl, Param, ParamMode, Spec, SpecInvocation, Stability, TypeDecl, TypeDeclKind,
    TypeField, Variant, VariantPayload, Visibility,
};

/// Pretty-print a complete file. The output is constructed so that
/// `parse(print(file)) == parse(print(parse(print(file))))` — re-parsing
/// the printed source and re-printing it yields the same text.
pub fn print_file(file: &File, interner: &Interner) -> String {
    let mut p = Printer::new(interner);
    p.print_file(file);
    p.finish()
}

impl<'a> Printer<'a> {
    pub(crate) fn print_item(&mut self, item: &Item) {
        for doc in &item.doc {
            self.print_doc_line(doc);
        }
        for attr in &item.attributes {
            self.print_attribute(attr);
            self.write_newline();
        }
        match &item.kind {
            ItemKind::Function(fd) => self.print_fn_decl(fd),
            ItemKind::TypeDecl(td) => self.print_type_decl(td),
            ItemKind::Spec(s) => self.print_spec(s),
            ItemKind::SpecInvocation(si) => self.print_spec_invocation(si),
            ItemKind::Let(ld) => self.print_let_item(ld),
            ItemKind::Import(i) => self.print_import(i),
            ItemKind::Module(m) => self.print_module(m),
            ItemKind::Derive(d) => self.print_derive(d),
        }
    }

    /// Emit the `stable` / `unstable` keyword + trailing space, or
    /// nothing.
    ///
    /// Per §3.7 / D-19 the keyword is the sole source of API stability
    /// (`@stable` / `@unstable` are not attributes), so it always
    /// prints when present. `since` is vestigial and always `None`.
    fn print_stability(&mut self, s: Option<Stability>) {
        match s {
            Some(Stability::Stable { .. }) => self.write("stable "),
            Some(Stability::Unstable { .. }) => self.write("unstable "),
            None => {}
        }
    }

    fn print_visibility(&mut self, v: Visibility) {
        if v == Visibility::Public {
            self.write("public ");
        }
    }

    /// Emit the linearity modifier (`affine ` / `linear `) between
    /// visibility and `type`. Absent linearity emits nothing.
    fn print_linearity(&mut self, l: Option<Linearity>) {
        match l {
            Some(Linearity::Affine) => self.write("affine "),
            Some(Linearity::Linear) => self.write("linear "),
            None => {}
        }
    }

    fn print_fn_decl(&mut self, fd: &FnDecl) {
        self.print_stability(fd.stability);
        self.print_visibility(fd.visibility);
        if fd.refinement_stable {
            self.write("stable ");
        }
        self.write("function ");
        self.write_resolved(fd.name.name);
        if !fd.outbound_generics.is_empty() {
            self.write("<");
            self.comma_separated(&fd.outbound_generics, |p, g| p.print_comptime_generic(g));
            self.write(">");
        }
        self.write("(");
        let comptime_generics: Vec<&GenericParam> = fd
            .generics
            .iter()
            .filter(|g| g.kind == GenericKind::Comptime)
            .collect();
        let mut first = true;
        for g in &comptime_generics {
            if !first {
                self.write(", ");
            }
            first = false;
            self.print_comptime_generic(g);
        }
        for param in &fd.params {
            if !first {
                self.write(", ");
            }
            first = false;
            self.print_param(param);
        }
        self.write(")");
        if let Some(ret) = &fd.return_ty {
            self.write(" -> ");
            // Return-position borrow mode (`let` / `mutable`) precedes
            // the type; `ByValue` emits nothing so by-value returns
            // round-trip unchanged.
            let mode_kw = fd.return_mode.keyword();
            if !mode_kw.is_empty() {
                self.write(mode_kw);
                self.write(" ");
            }
            self.print_type(ret);
        }
        if let Some(row) = &fd.effects {
            self.write(" ");
            self.print_effect_row(row);
        }
        for clause in &fd.refinements {
            self.write(" ");
            self.print_refinement_clause(clause);
        }
        self.write(" ");
        self.print_fn_body(&fd.body);
    }

    fn print_fn_body(&mut self, body: &FnBody) {
        match body {
            FnBody::Block(b) => self.print_block(b),
            FnBody::Extern { name, dll, .. } => {
                self.write("extern \"");
                let resolved = self.interner.resolve(*name).to_string();
                self.write_escaped_str(&resolved);
                self.write("\"");
                if let Some(dll) = dll {
                    self.write(" from \"");
                    let resolved = self.interner.resolve(*dll).to_string();
                    self.write_escaped_str(&resolved);
                    self.write("\"");
                }
            }
        }
    }

    fn print_comptime_generic(&mut self, g: &GenericParam) {
        self.write("comptime ");
        self.write_resolved(g.name.name);
        if let Some(ty) = &g.ty {
            self.write(": ");
            self.print_type(ty);
        }
    }

    fn print_param(&mut self, p: &Param) {
        self.write_resolved(p.name.name);
        self.write(": ");
        match p.mode {
            ParamMode::Default => {}
            ParamMode::Mutable => self.write("mutable "),
            ParamMode::Take => self.write("take "),
            ParamMode::Init => self.write("init "),
        }
        self.print_type(&p.ty);
    }

    fn print_type_decl(&mut self, td: &TypeDecl) {
        self.print_stability(td.stability);
        self.print_visibility(td.visibility);
        self.print_linearity(td.linearity);
        self.write("type ");
        self.write_resolved(td.name.name);
        self.write(" {");
        match &td.kind {
            TypeDeclKind::Product { fields } => {
                self.with_indent(|p| {
                    for f in fields {
                        p.write_newline();
                        p.print_type_field(f);
                    }
                });
            }
            TypeDeclKind::Sum { variants } => {
                self.with_indent(|p| {
                    for v in variants {
                        p.write_newline();
                        p.print_variant(v);
                    }
                });
            }
        }
        self.write_newline();
        self.write("}");
    }

    fn print_type_field(&mut self, f: &TypeField) {
        self.write_resolved(f.name.name);
        self.write(": ");
        self.print_type(&f.ty);
        if let Some(ref_pred) = &f.refinement {
            self.write(" where ");
            self.print_expr(ref_pred);
        }
    }

    fn print_variant(&mut self, v: &Variant) {
        self.write("case ");
        self.write_resolved(v.name.name);
        match &v.payload {
            VariantPayload::Unit => {}
            VariantPayload::Tuple(tys) => {
                self.write("(");
                self.comma_separated(tys, |p, t| p.print_type(t));
                self.write(")");
            }
            VariantPayload::Struct(fields) => {
                self.write("(");
                self.comma_separated(fields, |p, f| p.print_type_field(f));
                self.write(")");
            }
        }
    }

    fn print_spec(&mut self, s: &Spec) {
        self.print_stability(s.stability);
        self.print_visibility(s.visibility);
        self.write("spec ");
        self.write_resolved(s.name.name);
        self.write("(");
        let mut first = true;
        for g in &s.generics {
            if !first {
                self.write(", ");
            }
            first = false;
            self.print_spec_generic(g);
        }
        self.write(")");
        for clause in &s.where_clauses {
            self.write(" ");
            self.print_refinement_clause(clause);
        }
        self.write(" {");
        self.with_indent(|p| {
            for item in &s.body {
                p.write_newline();
                p.print_item(item);
            }
        });
        self.write_newline();
        self.write("}");
    }

    fn print_spec_generic(&mut self, g: &GenericParam) {
        if g.kind == GenericKind::Comptime {
            self.write("comptime ");
        }
        self.write_resolved(g.name.name);
        if let Some(ty) = &g.ty {
            self.write(": ");
            self.print_type(ty);
        }
        if !g.admits.is_empty() {
            self.write(" where ");
            self.write_resolved(g.name.name);
            self.write(" admits ");
            for (i, atom) in g.admits.iter().enumerate() {
                if i > 0 {
                    self.write(", ");
                }
                self.print_admits_atom(atom);
            }
        }
    }

    fn print_admits_atom(&mut self, atom: &crate::ast::AdmitsConstraint) {
        use crate::ast::{AdmitsConstraint, BinOp};
        match atom {
            AdmitsConstraint::Op { op, .. } => {
                let s = match op {
                    BinOp::Add => "+",
                    BinOp::Sub => "-",
                    BinOp::Mul => "*",
                    BinOp::Div => "/",
                    BinOp::Mod => "%",
                    BinOp::WrapAdd => "+%",
                    BinOp::WrapSub => "-%",
                    BinOp::WrapMul => "*%",
                    BinOp::CheckAdd => "+?",
                    BinOp::CheckSub => "-?",
                    BinOp::CheckMul => "*?",
                    BinOp::CheckMod => "%?",
                    BinOp::SatAdd => "+|",
                    BinOp::SatSub => "-|",
                    BinOp::SatMul => "*|",
                    BinOp::Eq => "==",
                    BinOp::Ne => "!=",
                    BinOp::Lt => "<",
                    BinOp::Le => "<=",
                    BinOp::Gt => ">",
                    BinOp::Ge => ">=",
                    BinOp::And => "&&",
                    BinOp::Or => "||",
                    BinOp::BitAnd => "&",
                    BinOp::BitOr => "|",
                    BinOp::BitXor => "^",
                    BinOp::Shl => "<<",
                    BinOp::Shr => ">>",
                };
                self.write(s);
            }
            AdmitsConstraint::Literal { lit, .. } => self.print_literal(lit),
            AdmitsConstraint::Member { name, ty, .. } => {
                self.write_resolved(name.name);
                self.write(": ");
                self.print_type(ty);
            }
        }
    }

    fn print_spec_invocation(&mut self, si: &SpecInvocation) {
        self.write("spec ");
        self.print_path(&si.path);
        self.write("(");
        self.comma_separated(&si.args, |p, e| p.print_expr(e));
        self.write(")");
    }

    fn print_let_item(&mut self, ld: &LetDecl) {
        self.print_visibility(ld.visibility);
        self.write("let ");
        self.write_resolved(ld.name.name);
        self.write(": ");
        self.print_type(&ld.ty);
        self.write(" = ");
        self.print_expr(&ld.init);
    }

    fn print_import(&mut self, i: &Import) {
        self.write("import ");
        self.print_path(&i.path);
        if let Some(selection) = &i.selection {
            self.write(".{");
            let mut first = true;
            for name in selection {
                if !first {
                    self.write(", ");
                }
                first = false;
                self.write_resolved(name.name);
            }
            self.write("}");
        }
        if let Some(alias) = &i.alias {
            self.write(" as ");
            self.write_resolved(alias.name);
        }
    }

    fn print_module(&mut self, m: &ModuleDecl) {
        self.write("module ");
        self.print_path(&m.path);
    }

    fn print_derive(&mut self, d: &crate::ast::Derive) {
        self.write("derive ");
        let mut first = true;
        for item in &d.items {
            if !first {
                self.write(", ");
            }
            first = false;
            self.write_resolved(item.name);
        }
        self.write(" for ");
        self.print_path(&d.target);
    }
}
