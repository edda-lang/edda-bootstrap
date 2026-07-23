//! Item-level attribute pretty-printer.
//!
//! Each attribute prints as `@name(args)` on its own line, between the
//! item's doc-comments and its visibility / kind keyword. Comma-separated
//! args mirror the parser's grammar — positional first, named last —
//! and a `Named` arg's value is restricted to `Ident` / `Lit` by the
//! parser, so the recursive walk does not need to handle nested named.

use super::Printer;
use crate::ast::{AttrArg, AttrLit, Attribute};
use crate::token::IntBase;

impl<'a> Printer<'a> {
    /// Emit one `@name(args)` clause, no trailing whitespace.
    pub(crate) fn print_attribute(&mut self, attr: &Attribute) {
        self.write("@");
        self.write_resolved(attr.name.name);
        self.write("(");
        for (i, arg) in attr.args.iter().enumerate() {
            if i > 0 {
                self.write(", ");
            }
            self.print_attr_arg(arg);
        }
        self.write(")");
    }

    fn print_attr_arg(&mut self, arg: &AttrArg) {
        match arg {
            AttrArg::Ident(id) => self.write_resolved(id.name),
            AttrArg::Lit { lit, .. } => self.print_attr_lit(*lit),
            AttrArg::Named { key, value, .. } => {
                self.write_resolved(key.name);
                self.write(": ");
                //   contract; the recursive call handles either leaf.
                self.print_attr_arg(value);
            }
        }
    }

    fn print_attr_lit(&mut self, lit: AttrLit) {
        match lit {
            AttrLit::Str(sym) => {
                self.write("\"");
                let resolved = self.interner.resolve(sym).to_string();
                self.write_escaped_str(&resolved);
                self.write("\"");
            }
            AttrLit::Int { value, base } => match base {
                IntBase::Dec => self.write(&format!("{}", value)),
                IntBase::Hex => self.write(&format!("0x{:X}", value)),
                IntBase::Bin => self.write(&format!("0b{:b}", value)),
                IntBase::Oct => self.write(&format!("0o{:o}", value)),
            },
            AttrLit::Bool(b) => self.write(if b { "true" } else { "false" }),
        }
    }
}
