//! Type-expression pretty-printer plus the satellite types (effect
//! rows, refinement clauses) that travel with function declarations.

use super::Printer;
use crate::ast::{
    EffectMember, EffectRow, FnTypeParam, ParamMode, RefinementClause, RefinementKind, Type,
    TypeKind,
};

impl<'a> Printer<'a> {
    pub(crate) fn print_type(&mut self, t: &Type) {
        match &t.kind {
            TypeKind::Path(p) => self.print_path(p),
            TypeKind::Tuple(elems) => {
                self.write("(");
                self.comma_separated(elems, |p, e| p.print_type(e));
                self.write(")");
            }
            TypeKind::Slice(inner) => {
                self.write("[");
                self.print_type(inner);
                self.write("]");
            }
            TypeKind::Unit => self.write("()"),
            TypeKind::Function {
                params,
                ret,
                effects,
            } => {
                self.write("function(");
                self.comma_separated(params, |p, fp| p.print_fn_type_param(fp));
                self.write(") -> ");
                self.print_type(ret);
                if let Some(row) = effects {
                    self.write(" ");
                    self.print_effect_row(row);
                }
            }
            TypeKind::Meta => self.write("Type"),
            TypeKind::Comptime(inner) => {
                self.write("comptime ");
                self.print_type(inner);
            }
            TypeKind::Refined { base, pred } => {
                self.print_type(base);
                self.write(" where ");
                self.print_expr(pred);
            }
            TypeKind::Error => self.write("/* parse error */"),
        }
    }

    fn print_fn_type_param(&mut self, fp: &FnTypeParam) {
        if let Some(name) = &fp.name {
            self.write_resolved(name.name);
            self.write(": ");
        }
        match fp.mode {
            ParamMode::Default => {}
            ParamMode::Mutable => self.write("mutable "),
            ParamMode::Take => self.write("take "),
            ParamMode::Init => self.write("set "),
        }
        self.print_type(&fp.ty);
    }

    pub(crate) fn print_effect_row(&mut self, row: &EffectRow) {
        self.write("with {");
        self.comma_separated(&row.members, |p, m| p.print_effect_member(m));
        self.write("}");
    }

    fn print_effect_member(&mut self, m: &EffectMember) {
        match m {
            EffectMember::Capability(name) => self.write_resolved(name.name),
            EffectMember::Named { name, ty } => {
                self.write_resolved(name.name);
                self.write(": ");
                self.print_type(ty);
            }
            EffectMember::Spread(path) => {
                self.write("...");
                self.print_path(path);
            }
            EffectMember::Graded { kind, bound } => {
                self.write_resolved(kind.name);
                self.write("(");
                self.print_expr(bound);
                self.write(")");
            }
        }
    }

    pub(crate) fn print_refinement_clause(&mut self, c: &RefinementClause) {
        self.write(match c.kind {
            RefinementKind::Where => "where ",
            RefinementKind::Requires => "requires ",
            RefinementKind::Ensures => "ensures ",
            RefinementKind::Decreases => "decreases ",
        });
        self.print_expr(&c.pred);
    }
}
