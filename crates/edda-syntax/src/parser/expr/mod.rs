//! Expression parser. Combines recursive descent (control-flow forms,
//! primary expressions, struct literals) with Pratt-style precedence
//! climbing for the locked binary operators (`expressions.md` §"Operator
//! precedence").
//!
//! Postfix operators (`.field`, `.method(...)`, `[idx]`, `?`, `.await`)
//! and unary prefix operators (`!`, `-`, `~`) bind tighter than every
//! binary operator and are handled outside the Pratt loop. The `as`
//! cast sits between unary prefix and the multiplicative tier; it is
//! handled in the same loop with a special-cased RHS (a [`Type`] rather
//! than another [`Expr`]).
//!
//! This module is split by concept for file-size reasons; every
//! submodule extends the same `impl Parser` block:
//! - this file — the Pratt precedence-climbing core, `as` cast, range
//!   attachment, and unary prefix;
//! - [`primary`] — primary-expression dispatch, paren/tuple, ident-form,
//!   struct-literal tail;
//! - [`closure`] — closure-literal, params, captures;
//! - [`spawn`] — spawn-block disambiguation and the spawn-arg list;
//! - [`postfix`] — postfix chains (`.`/`(`/`[`/`?`/`.await`), tuple-index,
//!   call-argument lists.

use crate::ast::{BinOp, CastMode, Expr, ExprKind, RangeKind, UnOp};
use crate::token::Token;

use super::Parser;
use super::control::can_start_expr;
use super::op_guardrail::is_precedence_mix;

mod closure;
mod postfix;
mod primary;
mod spawn;

// Binding-power table. Higher numbers bind tighter. Returns the operator,
// its bp, and whether the operator is non-associative.
fn binop_bp(t: Token) -> Option<(BinOp, u8, bool)> {
    Some(match t {
        Token::Star => (BinOp::Mul, 12, false),
        Token::Slash => (BinOp::Div, 12, false),
        Token::Percent => (BinOp::Mod, 12, false),
        Token::PercentQuestion => (BinOp::CheckMod, 12, false),
        Token::StarPct => (BinOp::WrapMul, 12, false),
        Token::StarQuestion => (BinOp::CheckMul, 12, false),
        Token::StarPipe => (BinOp::SatMul, 12, false),
        Token::Plus => (BinOp::Add, 11, false),
        Token::Minus => (BinOp::Sub, 11, false),
        Token::PlusPct => (BinOp::WrapAdd, 11, false),
        Token::MinusPct => (BinOp::WrapSub, 11, false),
        Token::PlusQuestion => (BinOp::CheckAdd, 11, false),
        Token::MinusQuestion => (BinOp::CheckSub, 11, false),
        Token::PlusPipe => (BinOp::SatAdd, 11, false),
        Token::MinusPipe => (BinOp::SatSub, 11, false),
        Token::LtLt => (BinOp::Shl, 10, false),
        Token::GtGt => (BinOp::Shr, 10, false),
        Token::Amp => (BinOp::BitAnd, 9, false),
        Token::Caret => (BinOp::BitXor, 8, false),
        Token::Pipe => (BinOp::BitOr, 7, false),
        Token::Lt => (BinOp::Lt, 6, true),
        Token::Gt => (BinOp::Gt, 6, true),
        Token::LtEq => (BinOp::Le, 6, true),
        Token::GtEq => (BinOp::Ge, 6, true),
        Token::EqEq => (BinOp::Eq, 5, true),
        Token::BangEq => (BinOp::Ne, 5, true),
        Token::AmpAmp => (BinOp::And, 4, false),
        Token::PipePipe => (BinOp::Or, 3, false),
        _ => return None,
    })
}

const AS_BP: u8 = 13;
const RANGE_BP: u8 = 2;

impl<'a> Parser<'a> {
    /// Parse a single expression at the topmost precedence.
    pub(crate) fn parse_expr(&mut self) -> Expr {
        self.parse_expr_bp(0, true, None)
    }

    /// Parse an expression but reject struct literals at the head. Used
    /// for the condition of `if` / `match` / `for` so that `if x { ... }`
    /// parses the braces as the body, not as a struct-literal payload.
    pub(super) fn parse_expr_no_struct(&mut self) -> Expr {
        self.parse_expr_bp(0, false, None)
    }

    /// Pratt precedence-climbing loop over the binary operators. The guardrail emits a `parse_error` when a bare bitwise (`& | ^`) operator is adjacent to a comparison/equality operator without explicit parens, per `expressions.md` §"Operator precedence".
    fn parse_expr_bp(
        &mut self,
        min_bp: u8,
        allow_struct: bool,
        parent_op: Option<BinOp>,
    ) -> Expr {
        let mut lhs = self.parse_prefix(allow_struct);
        // `parse_prefix` may have returned a parenthesised binary, but
        // those are opaque to us — only operators we attach below are
        // candidates for the bitwise/compare-mix guardrail.
        let mut fresh: Option<BinOp> = None;
        loop {
            lhs = self.parse_postfix(lhs);
            if fresh.is_some() && !matches!(lhs.kind, ExprKind::Binary { .. }) {
                fresh = None;
            }
            if self.at(Token::As) && AS_BP >= min_bp {
                lhs = self.attach_as_cast(lhs);
                fresh = None;
                continue;
            }
            if matches!(
                self.peek_kind(),
                Token::DotDotLt | Token::DotDotEq | Token::DotDot
            ) && RANGE_BP >= min_bp
            {
                lhs = self.attach_range(lhs, allow_struct);
                fresh = None;
                continue;
            }
            let Some((op, bp, non_assoc)) = binop_bp(self.peek_kind()) else {
                break;
            };
            if bp < min_bp {
                break;
            }
            // Bitwise/compare-mix guardrail (LHS side): we're about to
            // attach `op` on top of a freshly-produced unparenthesised
            // `Binary` at an incompatible tier.
            if let Some(prev_op) = fresh
                && is_precedence_mix(prev_op, op)
            {
                self.emit_precedence_mix(lhs.span, prev_op, op);
            }
            self.bump();
            let rhs = self.parse_expr_bp(bp + 1, allow_struct, Some(op));
            let span = lhs.span.join(rhs.span);
            lhs = Expr {
                span,
                kind: ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
            };
            fresh = Some(op);
            if non_assoc
                && let Some((_, next_bp, _)) = binop_bp(self.peek_kind())
                && next_bp == bp
            {
                let span = self.peek().span;
                self.emit_error(
                    span,
                    "operator is non-associative; use parentheses to disambiguate",
                );
            }
        }
        // Bitwise/compare-mix guardrail (return-edge): if this call's
        // final unparenthesised binary will be attached by the caller as
        // an operand of an incompatible operator, flag the mix. Catches
        // `a == b & c` (RHS side) where the outer level cannot see that
        // the recursive call freshly produced `b & c`.
        if let Some(fresh_op) = fresh
            && let Some(parent) = parent_op
            && is_precedence_mix(fresh_op, parent)
        {
            self.emit_precedence_mix(lhs.span, fresh_op, parent);
        }
        lhs
    }

    /// Consume the leading `as` and wrap `lhs` in [`ExprKind::Cast`].
    /// After the target type, admits an optional trailing cast-mode
    /// keyword (`wrapping` / `saturating` / `checked`) per CLAUDE.md
    /// §"Numeric operators".
    fn attach_as_cast(&mut self, lhs: Expr) -> Expr {
        self.bump();
        let ty = self.parse_ty();
        let (mode, mode_span) = match self.peek_kind() {
            Token::Wrapping => {
                let span = self.peek().span;
                self.bump();
                (CastMode::Wrapping, Some(span))
            }
            Token::Saturating => {
                let span = self.peek().span;
                self.bump();
                (CastMode::Saturating, Some(span))
            }
            Token::Checked => {
                let span = self.peek().span;
                self.bump();
                (CastMode::Checked, Some(span))
            }
            _ => (CastMode::Trap, None),
        };
        let end_span = mode_span.unwrap_or(ty.span);
        let span = lhs.span.join(end_span);
        Expr {
            span,
            kind: ExprKind::Cast {
                expr: Box::new(lhs),
                ty: Box::new(ty),
                mode,
            },
        }
    }

    /// Consume a `..<` / `..=` / `..` and wrap `lhs` in [`ExprKind::Range`]. Also emits the non-associativity diagnostic for `a..<b..<c`.
    fn attach_range(&mut self, lhs: Expr, allow_struct: bool) -> Expr {
        let op_tok = self.peek_kind();
        let kind = match op_tok {
            Token::DotDotEq => RangeKind::Closed,
            _ => RangeKind::HalfOpen,
        };
        let allow_missing_rhs = op_tok == Token::DotDot;
        self.bump();
        let rhs_opt = if allow_missing_rhs && !can_start_expr(self.peek_kind()) {
            None
        } else {
            let rhs = self.parse_expr_bp(RANGE_BP + 1, allow_struct, None);
            Some(rhs)
        };
        let end_span = rhs_opt
            .as_ref()
            .map(|r| r.span)
            .unwrap_or_else(|| self.tokens[self.pos.saturating_sub(1)].span);
        let span = lhs.span.join(end_span);
        let result = Expr {
            span,
            kind: ExprKind::Range {
                lo: Some(Box::new(lhs)),
                hi: rhs_opt.map(Box::new),
                kind,
            },
        };
        if matches!(
            self.peek_kind(),
            Token::DotDotLt | Token::DotDotEq | Token::DotDot
        ) {
            let span = self.peek().span;
            self.emit_error(
                span,
                "range operators are non-associative; use parentheses",
            );
        }
        result
    }

    /// Prefix range form: `..`, `..hi`, `..<hi`, `..=hi`. The full-slice
    /// `..` form is the only one that may omit `hi`; the bounded `..<`
    /// and closed `..=` forms require an upper endpoint. The pretty-printer
    /// always emits the bare `..` spelling for open-ended ranges per
    /// phase-2-locks Gap 7, so the `..<hi` source spelling round-trips
    /// through the canonical `..hi` form.
    pub(super) fn parse_prefix_range(&mut self) -> Expr {
        let start = self.pos;
        let op_tok = self.peek_kind();
        let kind = match op_tok {
            Token::DotDotEq => RangeKind::Closed,
            _ => RangeKind::HalfOpen,
        };
        let allow_missing_rhs = op_tok == Token::DotDot;
        self.bump(); // `..` / `..<` / `..=`
        let hi_opt = if allow_missing_rhs && !can_start_expr(self.peek_kind()) {
            None
        } else {
            Some(Box::new(self.parse_expr_bp(RANGE_BP + 1, true, None)))
        };
        let span = self.span_from(start);
        Expr {
            span,
            kind: ExprKind::Range {
                lo: None,
                hi: hi_opt,
                kind,
            },
        }
    }

    fn parse_prefix(&mut self, allow_struct: bool) -> Expr {
        let op = match self.peek_kind() {
            Token::Bang => Some(UnOp::Not),
            Token::Minus => Some(UnOp::Neg),
            Token::Tilde => Some(UnOp::BitNot),
            _ => None,
        };
        if let Some(op) = op {
            let start = self.pos;
            self.bump();
            // Unary operators bind tighter than `as` (AS_BP=13). We
            // recurse with bp = AS_BP so a trailing `as` still attaches
            // to the whole unary expression as `(- x) as i32`.
            let expr = self.parse_expr_bp(AS_BP, allow_struct, None);
            return Expr {
                span: self.span_from(start),
                kind: ExprKind::Unary {
                    op,
                    expr: Box::new(expr),
                },
            };
        }
        self.parse_primary(allow_struct)
    }
}
