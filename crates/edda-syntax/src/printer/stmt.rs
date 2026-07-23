//! Statement and block pretty-printer.

use super::Printer;
use crate::ast::{AssignOp, BindingMode, Block, Stmt, StmtKind};

impl<'a> Printer<'a> {
    pub(crate) fn print_block(&mut self, b: &Block) {
        self.write("{");
        self.with_indent(|p| {
            for stmt in &b.stmts {
                p.write_newline();
                p.print_stmt(stmt);
                p.write(";");
            }
            if let Some(trailing) = &b.trailing {
                p.write_newline();
                p.print_expr(trailing);
            }
        });
        self.write_newline();
        self.write("}");
    }

    pub(crate) fn print_stmt(&mut self, s: &Stmt) {
        // Leading statement attributes, each on its own line above the
        // statement (mirrors the item printer), so parse → print → parse
        // round-trips a site-level `@trust` / `@unverified`.
        for attr in &s.attributes {
            self.print_attribute(attr);
            self.write_newline();
        }
        match &s.kind {
            StmtKind::Let {
                mutability,
                pat,
                ty,
                init,
            } => {
                self.write(match mutability {
                    BindingMode::Immutable => "let ",
                    BindingMode::Mutable => "var ",
                    BindingMode::Uninit => "uninit ",
                });
                self.print_pat(pat);
                if let Some(ty) = ty {
                    self.write(": ");
                    self.print_type(ty);
                }
                if let Some(init) = init {
                    self.write(" = ");
                    self.print_expr(init);
                }
            }
            StmtKind::Assign { target, op, rhs } => {
                self.print_expr(target);
                self.write(" ");
                self.write(assign_op_str(*op));
                self.write(" ");
                self.print_expr(rhs);
            }
            StmtKind::Expr(e) => self.print_expr(e),
        }
    }
}

fn assign_op_str(op: AssignOp) -> &'static str {
    match op {
        AssignOp::Plain => "=",
        AssignOp::Add => "+=",
        AssignOp::Sub => "-=",
        AssignOp::Mul => "*=",
        AssignOp::Div => "/=",
        AssignOp::Mod => "%=",
        AssignOp::BitAnd => "&=",
        AssignOp::BitOr => "|=",
        AssignOp::BitXor => "^=",
        AssignOp::Shl => "<<=",
        AssignOp::Shr => ">>=",
    }
}
