//! Bitwise/compare-mix precedence guardrail and small operator
//! helpers used by `expr.rs`. Split out for file-size reasons; the
//! diagnostic message format and the operator-category sets live here
//! so the Pratt loop in `expr.rs` stays focused on parse mechanics.
//!
//! The rule, locked in `corpus/edda-codex/docs/syntax/expressions.md`
//! §"Operator precedence": `a & b == c` is a syntax error. Either side
//! must wrap with explicit parens. This module owns the operator-set
//! predicates and the message template; `expr.rs` owns the call sites
//! where the guardrail fires.

use edda_span::Span;

use crate::ast::BinOp;

use super::Parser;

/// `&`, `|`, `^` — the bitwise integer operators the precedence-mix
/// guardrail covers. `<<` and `>>` are not included: `expressions.md`'s
/// worked example only locks the bitwise-and/or/xor vs comparison/equality
/// trap.
pub(super) fn is_bitwise(op: BinOp) -> bool {
    matches!(op, BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor)
}

/// `<`, `<=`, `>`, `>=`, `==`, `!=` — the comparison-or-equality tier
/// the precedence-mix guardrail covers.
pub(super) fn is_compare_or_eq(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::Ne
    )
}

/// Source spelling of every [`BinOp`] this module diagnoses. Hand-coded
/// here rather than added as `Display` to [`BinOp`] so the AST crate's
/// public surface stays minimal.
pub(super) fn op_spelling(op: BinOp) -> &'static str {
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

/// True for `(bitwise, compare/eq)` pairs in either direction. Caller is
/// responsible for ensuring neither operand was parenthesised — this is a
/// pure operator-category predicate.
pub(super) fn is_precedence_mix(a: BinOp, b: BinOp) -> bool {
    (is_bitwise(a) && is_compare_or_eq(b)) || (is_compare_or_eq(a) && is_bitwise(b))
}

impl<'a> Parser<'a> {
    /// Render the `expressions.md` lock as a `parse_error` carrying the span of the bitwise sub-expression. After emission parsing continues — the AST is best-effort and the diagnostic is the user-visible artifact.
    pub(super) fn emit_precedence_mix(&mut self, span: Span, a: BinOp, b: BinOp) {
        let (bit, cmp) = if is_bitwise(a) { (a, b) } else { (b, a) };
        let cmp_label = if matches!(cmp, BinOp::Eq | BinOp::Ne) {
            "equality"
        } else {
            "comparison"
        };
        let msg = format!(
            "mixing bitwise `{bit_op}` and {cmp_label} `{cmp_op}` requires parentheses — write `(a {bit_op} b) {cmp_op} c` or `a {bit_op} (b {cmp_op} c)`",
            bit_op = op_spelling(bit),
            cmp_op = op_spelling(cmp),
        );
        self.emit_error(span, msg);
    }
}
