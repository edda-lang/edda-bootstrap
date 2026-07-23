//! Source-form rendering of signatures, types, effect rows, and expressions.
//!
//! All functions here turn AST fragments into the locked source spellings
//! the structmap surface stores; none are parsed back.

use edda_intern::{Interner, Symbol};
use edda_syntax::ast::{EffectMember, EffectRow, FnDecl};

use super::items::path_string;

pub(super) fn render_sig_only(interner: &Interner, fd: &FnDecl) -> String {
    let mut out = String::from("(");
    let mut first = true;
    for p in &fd.params {
        if !first {
            out.push_str(", ");
        }
        first = false;
        let mode = match p.mode {
            edda_syntax::ast::ParamMode::Default => "",
            edda_syntax::ast::ParamMode::Mutable => "mutable ",
            edda_syntax::ast::ParamMode::Take => "take ",
            edda_syntax::ast::ParamMode::Init => "init ",
        };
        out.push_str(interner_text(interner, p.name.name));
        out.push_str(": ");
        out.push_str(mode);
        out.push_str(&type_text(interner, &p.ty));
    }
    out.push(')');
    if let Some(ret) = &fd.return_ty {
        out.push_str(" -> ");
        out.push_str(&type_text(interner, ret));
    }
    out
}

fn effect_row_text(interner: &Interner, row: &EffectRow) -> String {
    if row.members.is_empty() {
        return "{}".to_string();
    }
    let mut out = String::from("{ ");
    let mut first = true;
    for m in &row.members {
        if !first {
            out.push_str(", ");
        }
        first = false;
        out.push_str(&effect_member_text(interner, m));
    }
    out.push_str(" }");
    out
}

pub(crate) fn effect_member_text(interner: &Interner, m: &EffectMember) -> String {
    match m {
        EffectMember::Capability(name) => interner_text(interner, name.name).to_string(),
        EffectMember::Named { name, ty } => {
            format!(
                "{}: {}",
                interner_text(interner, name.name),
                type_text(interner, ty)
            )
        }
        EffectMember::Spread(path) => format!("...{}", path_string(path, interner)),
        EffectMember::Graded { kind, bound } => {
            format!(
                "{}({})",
                interner_text(interner, kind.name),
                expr_text(interner, bound)
            )
        }
    }
}

fn type_text(interner: &Interner, ty: &edda_syntax::ast::Type) -> String {
    use edda_syntax::ast::TypeKind;
    match &ty.kind {
        TypeKind::Path(p) => path_string(p, interner),
        TypeKind::Tuple(elems) => {
            let inner = elems
                .iter()
                .map(|e| type_text(interner, e))
                .collect::<Vec<_>>()
                .join(", ");
            format!("({})", inner)
        }
        TypeKind::Slice(t) => format!("[{}]", type_text(interner, t)),
        TypeKind::Unit => "()".to_string(),
        TypeKind::Function { params, ret, effects } => {
            let mut out = String::from("function(");
            let mut first = true;
            for p in params {
                if !first {
                    out.push_str(", ");
                }
                first = false;
                out.push_str(&fn_type_param_text(interner, p));
            }
            out.push(')');
            out.push_str(" -> ");
            out.push_str(&type_text(interner, ret));
            if let Some(row) = effects {
                out.push_str(" with ");
                out.push_str(&effect_row_text(interner, row));
            }
            out
        }
        TypeKind::Meta => "Type".to_string(),
        TypeKind::Comptime(t) => format!("comptime {}", type_text(interner, t)),
        TypeKind::Refined { base, .. } => type_text(interner, base),
        TypeKind::Error => "<error>".to_string(),
    }
}

fn fn_type_param_text(interner: &Interner, p: &edda_syntax::ast::FnTypeParam) -> String {
    let mut out = String::new();
    if let Some(name) = &p.name {
        out.push_str(interner_text(interner, name.name));
        out.push_str(": ");
    }
    match p.mode {
        edda_syntax::ast::ParamMode::Default => {}
        edda_syntax::ast::ParamMode::Mutable => out.push_str("mutable "),
        edda_syntax::ast::ParamMode::Take => out.push_str("take "),
        edda_syntax::ast::ParamMode::Init => out.push_str("init "),
    }
    out.push_str(&type_text(interner, &p.ty));
    out
}

pub(super) fn expr_text(interner: &Interner, expr: &edda_syntax::ast::Expr) -> String {
    use edda_syntax::ast::ExprKind;
    use edda_syntax::ast::Literal;
    match &expr.kind {
        ExprKind::Literal(l) => match l {
            Literal::Int { value, .. } => value.to_string(),
            Literal::Float(s) => interner_text(interner, *s).to_string(),
            Literal::Str(s) => format!("\"{}\"", interner_text(interner, *s)),
            Literal::Bool(b) => b.to_string(),
            Literal::Unit => "()".to_string(),
        },
        ExprKind::Path(p) => path_string(p, interner),
        ExprKind::Binary { op, lhs, rhs } => format!(
            "({} {} {})",
            expr_text(interner, lhs),
            binop_str(*op),
            expr_text(interner, rhs),
        ),
        ExprKind::Unary { op, expr } => format!("({}{})", unop_str(*op), expr_text(interner, expr)),
        ExprKind::Call { callee, args } => {
            let a = args
                .iter()
                .map(|c| expr_text(interner, &c.expr))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{}({})", expr_text(interner, callee), a)
        }
        // Fallback for forms whose source rendering is non-trivial — emit a
        // placeholder; structmap is informational, not a contract.
        _ => "<expr>".to_string(),
    }
}

fn binop_str(op: edda_syntax::ast::BinOp) -> &'static str {
    use edda_syntax::ast::BinOp;
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

fn unop_str(op: edda_syntax::ast::UnOp) -> &'static str {
    use edda_syntax::ast::UnOp;
    match op {
        UnOp::Neg => "-",
        UnOp::Not => "!",
        UnOp::BitNot => "~",
    }
}

pub(super) fn interner_text(interner: &Interner, sym: Symbol) -> &str {
    interner.try_resolve(sym).unwrap_or("<missing>")
}
