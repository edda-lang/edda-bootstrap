//! Expression pretty-printer.
//!
//! Strategy for round-trip safety:
//! - Compound expressions (everything that has subexpressions) are
//!   wrapped in `(...)` when they appear as sub-positions. Atomic forms
//!   (literals, paths, blocks, unit, tuples, struct lits) emit directly.
//! - Method calls and field accesses on a [`crate::ast::Path`] receiver
//!   wrap the receiver in `(...)` so the parser cannot greedily merge
//!   the dot back into the path.
//! - The `f"..."` raw payload is emitted verbatim between `f"` and `"`.

use super::Printer;
use crate::ast::{
    BinOp, CallArg, Capture, CaptureMode, Closure, Expr, ExprKind, FStringPart, Literal, Param,
    ParamMode, Path, RangeKind, SpawnArg, SpawnExpr, UnOp,
};
use crate::token::IntBase;

impl<'a> Printer<'a> {
    pub(crate) fn print_expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Literal(l) => self.print_literal(l),
            ExprKind::FString(parts) => self.print_fstring(parts),
            ExprKind::Path(p) => self.print_path(p),
            ExprKind::Binary { op, lhs, rhs } => {
                self.print_atom(lhs);
                self.write(" ");
                self.write(binop_str(*op));
                self.write(" ");
                self.print_atom(rhs);
            }
            ExprKind::Unary { op, expr } => {
                self.write(unop_str(*op));
                self.print_atom(expr);
            }
            ExprKind::Call { callee, args } => {
                self.print_call_callee(callee);
                self.write("(");
                self.comma_separated(args, |p, a| p.print_call_arg(a));
                self.write(")");
            }
            ExprKind::MethodCall {
                receiver,
                name,
                args,
            } => {
                // Parens around the receiver prevent the parser from
                // greedily merging `<path>.<name>` into a single path.
                self.write("(");
                self.print_expr(receiver);
                self.write(").");
                self.write_resolved(name.name);
                self.write("(");
                self.comma_separated(args, |p, a| p.print_call_arg(a));
                self.write(")");
            }
            ExprKind::Field { receiver, name } => {
                self.write("(");
                self.print_expr(receiver);
                self.write(").");
                self.write_resolved(name.name);
            }
            ExprKind::TupleIndex { receiver, index } => {
                self.write("(");
                self.print_expr(receiver);
                self.write(").");
                self.write(&index.to_string());
            }
            ExprKind::CompField { receiver, index } => {
                self.write("(");
                self.print_expr(receiver);
                self.write(").(");
                self.print_expr(index);
                self.write(")");
            }
            ExprKind::Index { receiver, index } => {
                self.print_atom(receiver);
                self.write("[");
                self.print_expr(index);
                self.write("]");
            }
            ExprKind::If {
                cond,
                then_block,
                else_branch,
            } => {
                self.write("if ");
                self.print_expr(cond);
                self.write(" ");
                self.print_block(then_block);
                if let Some(eb) = else_branch {
                    self.write(" else ");
                    self.print_expr(eb);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.write("match ");
                self.print_expr(scrutinee);
                self.write(" {");
                self.with_indent(|p| {
                    for arm in arms {
                        p.write_newline();
                        p.write("case ");
                        p.print_match_pat(&arm.pat);
                        if let Some(guard) = &arm.guard {
                            p.write(" where ");
                            p.print_expr(guard);
                        }
                        p.write(" => ");
                        p.print_expr(&arm.body);
                    }
                });
                self.write_newline();
                self.write("}");
            }
            ExprKind::Block(b) => self.print_block(b),
            ExprKind::Cast { expr, ty, mode } => {
                self.print_atom(expr);
                self.write(" as ");
                self.print_type(ty);
                if let Some(kw) = mode.keyword() {
                    self.write(" ");
                    self.write(kw);
                }
            }
            ExprKind::Range { lo, hi, kind } => {
                if let Some(lo_expr) = lo {
                    self.print_atom(lo_expr);
                }
                let op = match (lo.is_some(), hi.is_some(), kind) {
                    // Bounded operators print their explicit `..<` / `..=` form.
                    (true, true, RangeKind::HalfOpen) => "..<",
                    (true, true, RangeKind::Closed) => "..=",
                    // Open-ended forms always use bare `..` per phase-2-locks Gap 7.
                    _ => "..",
                };
                self.write(op);
                if let Some(hi_expr) = hi {
                    self.print_atom(hi_expr);
                }
            }
            ExprKind::Tuple(elems) => {
                self.write("(");
                self.comma_separated(elems, |p, e| p.print_expr(e));
                self.write(")");
            }
            ExprKind::Array(elems) => {
                self.write("[");
                self.comma_separated(elems, |p, e| p.print_expr(e));
                self.write("]");
            }
            ExprKind::StructLit { path, fields } => {
                self.print_path(path);
                self.write(" {");
                self.with_indent(|p| {
                    for (i, f) in fields.iter().enumerate() {
                        if i > 0 {
                            p.write(",");
                        }
                        p.write_newline();
                        p.write_resolved(f.name.name);
                        p.write(": ");
                        if let Some(mode) = f.mode {
                            p.write(mode.keyword());
                            p.write(" ");
                        }
                        p.print_expr(&f.value);
                    }
                });
                self.write_newline();
                self.write("}");
            }
            ExprKind::Loop {
                body, decreases, ..
            } => {
                self.write("loop ");
                if let Some(measure) = decreases {
                    self.write("decreases ");
                    self.print_atom(measure);
                    self.write(" ");
                }
                self.print_block(body);
            }
            ExprKind::For { pat, iter, body, .. } => {
                self.write("for ");
                self.print_pat(pat);
                self.write(" in ");
                self.print_expr(iter);
                self.write(" ");
                self.print_block(body);
            }
            ExprKind::Try(inner) => {
                self.print_atom(inner);
                self.write("?");
            }
            ExprKind::Await(inner) => {
                self.print_atom(inner);
                self.write(".await");
            }
            ExprKind::Raise(inner) => {
                self.write("raise ");
                self.print_atom(inner);
            }
            ExprKind::Panic(inner) => {
                self.write("panic ");
                self.print_atom(inner);
            }
            ExprKind::Comptime(inner) => {
                self.write("comptime ");
                self.print_atom(inner);
            }
            ExprKind::ComptimeBlock(b) => {
                self.write("comptime ");
                self.print_block(b);
            }
            ExprKind::Scope { kind, name, body } => {
                self.write("scope(");
                self.write(kind.as_str());
                self.write(") ");
                if let Some(name) = name {
                    self.write_resolved(name.name);
                    self.write(" ");
                }
                self.print_block(body);
            }
            ExprKind::Return(v) => {
                self.write("return");
                if let Some(v) = v {
                    self.write(" ");
                    self.print_expr(v);
                }
            }
            ExprKind::Break { value, .. } => {
                self.write("break");
                if let Some(v) = value {
                    self.write(" ");
                    self.print_expr(v);
                }
            }
            ExprKind::Continue { .. } => self.write("continue"),
            ExprKind::EffectRow(row) => self.print_effect_row(row),
            ExprKind::Closure(c) => self.print_closure(c),
            ExprKind::Handle {
                effect,
                ty,
                binder,
                recovery,
                body,
            } => {
                self.write("handle ");
                self.write_resolved(effect.name);
                if let Some(ty) = ty {
                    self.write(": ");
                    self.print_type(ty);
                }
                if let Some(b) = binder {
                    self.write(" as ");
                    self.write_resolved(b.name);
                }
                self.write(" -> ");
                self.print_atom(recovery);
                self.write(" ");
                self.print_block(body);
            }
            ExprKind::Spawn(s) => self.print_spawn(s),
            ExprKind::Forall { bound, iter, body } => {
                self.write("forall ");
                self.write_resolved(bound.name);
                self.write(" in ");
                self.print_expr(iter);
                self.write(": ");
                self.print_expr(body);
            }
            ExprKind::Exists { bound, iter, body } => {
                self.write("exists ");
                self.write_resolved(bound.name);
                self.write(" in ");
                self.print_expr(iter);
                self.write(": ");
                self.print_expr(body);
            }
            ExprKind::Error => self.write("/* parse error */"),
        }
    }

    fn print_spawn(&mut self, s: &SpawnExpr) {
        self.write_resolved(s.scope_name.name);
        self.write(".spawn");
        if !s.args.is_empty() {
            self.write("(");
            self.comma_separated(&s.args, |p, a| p.print_spawn_arg(a));
            self.write(")");
        }
        self.write(" ");
        self.print_block(&s.body);
    }

    fn print_spawn_arg(&mut self, a: &SpawnArg) {
        self.write("take ");
        self.write_resolved(a.name.name);
        if let Some(ty) = &a.ty {
            self.write(": ");
            self.print_type(ty);
        }
        self.write(" = ");
        self.print_expr(&a.init);
    }

    fn print_closure(&mut self, c: &Closure) {
        self.write("function(");
        self.comma_separated(&c.params, |p, param| p.print_closure_param(param));
        self.write(") -> ");
        self.print_type(&c.ret);
        if let Some(row) = &c.effects {
            self.write(" ");
            self.print_effect_row(row);
        }
        if let Some(captures) = &c.captures {
            self.write(" captures {");
            self.comma_separated(captures, |p, cap| p.print_capture(cap));
            self.write("}");
        }
        self.write(" ");
        self.print_block(&c.body);
    }

    fn print_closure_param(&mut self, p: &Param) {
        self.write_resolved(p.name.name);
        self.write(": ");
        match p.mode {
            ParamMode::Default => {}
            ParamMode::Mutable => self.write("mutable "),
            ParamMode::Take => self.write("take "),
            ParamMode::Init => self.write("set "),
        }
        self.print_type(&p.ty);
    }

    fn print_capture(&mut self, c: &Capture) {
        self.write_resolved(c.name.name);
        match c.mode {
            CaptureMode::Let => {}
            CaptureMode::Take => self.write(": take"),
        }
    }

    /// Like [`Self::print_expr`] but wraps compound forms in `(...)`. Used
    /// for sub-positions where the parser's precedence climbing could
    /// otherwise re-associate operators.
    fn print_atom(&mut self, e: &Expr) {
        if is_atomic(&e.kind) {
            self.print_expr(e);
        } else {
            self.write("(");
            self.print_expr(e);
            self.write(")");
        }
    }

    /// Emit one call argument with its optional mode keyword and
    /// optional payload-field name. A bare argument prints just the
    /// expression; a mode-prefixed argument prints `<keyword> <expr>`;
    /// a named-payload argument prints `<name>: <expr>`.
    fn print_call_arg(&mut self, arg: &CallArg) {
        if let Some(mode) = arg.mode {
            self.write(mode.keyword());
            self.write(" ");
        }
        if let Some(name) = &arg.name {
            self.write_resolved(name.name);
            self.write(": ");
        }
        self.print_expr(&arg.expr);
    }

    /// Emit the callee of a `Call`. Bare paths print without parens so
    /// `foo(args)` and `std.fs.read(args)` round-trip as themselves; any
    /// non-path callee is parenthesised.
    fn print_call_callee(&mut self, e: &Expr) {
        if matches!(e.kind, ExprKind::Path(_)) {
            self.print_expr(e);
        } else {
            self.write("(");
            self.print_expr(e);
            self.write(")");
        }
    }

    pub(crate) fn print_path(&mut self, p: &Path) {
        for (i, seg) in p.segments.iter().enumerate() {
            if i > 0 {
                self.write(".");
            }
            self.write_resolved(seg.name);
        }
    }

    /// Reconstruct an `f"...{expr}..."` interpolated string from its
    /// parsed [`FStringPart`]s.
    fn print_fstring(&mut self, parts: &[FStringPart]) {
        self.write("f\"");
        for part in parts {
            match part {
                FStringPart::Text(sym) => self.write_resolved(*sym),
                FStringPart::Slot(expr) => {
                    self.write("{");
                    self.print_expr(expr);
                    self.write("}");
                }
            }
        }
        self.write("\"");
    }

    pub(crate) fn print_literal(&mut self, l: &Literal) {
        match l {
            Literal::Int { value, base } => match base {
                IntBase::Dec => self.write(&format!("{}", value)),
                IntBase::Hex => self.write(&format!("0x{:X}", value)),
                IntBase::Bin => self.write(&format!("0b{:b}", value)),
                IntBase::Oct => self.write(&format!("0o{:o}", value)),
            },
            Literal::Float(sym) => self.write_resolved(*sym),
            Literal::Str(sym) => {
                self.write("\"");
                self.write_escaped_str(self.interner.resolve(*sym));
                self.write("\"");
            }
            Literal::Bool(b) => self.write(if *b { "true" } else { "false" }),
            Literal::Unit => self.write("()"),
        }
    }

    pub(crate) fn write_escaped_str(&mut self, s: &str) {
        // Re-escape only the characters that the lexer's escape grammar
        // distinguishes; other UTF-8 passes through verbatim.
        let mut buf = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '\\' => buf.push_str("\\\\"),
                '"' => buf.push_str("\\\""),
                '\n' => buf.push_str("\\n"),
                '\r' => buf.push_str("\\r"),
                '\t' => buf.push_str("\\t"),
                '\0' => buf.push_str("\\0"),
                _ => buf.push(c),
            }
        }
        self.out.push_str(&buf);
    }
}

fn is_atomic(k: &ExprKind) -> bool {
    matches!(
        k,
        ExprKind::Literal(_)
            | ExprKind::FString(_)
            | ExprKind::Path(_)
            | ExprKind::Block(_)
            | ExprKind::Tuple(_)
            | ExprKind::Array(_)
            | ExprKind::StructLit { .. }
            | ExprKind::EffectRow(_)
            | ExprKind::Closure(_)
            | ExprKind::Error
    )
}

fn binop_str(op: BinOp) -> &'static str {
    match op {
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
    }
}

fn unop_str(op: UnOp) -> &'static str {
    match op {
        UnOp::Neg => "-",
        UnOp::Not => "!",
        UnOp::BitNot => "~",
    }
}
