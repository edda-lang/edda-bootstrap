//! Binary-operator inference.
//!
//! Implements the binary operator rules from
//! `docs/types/inference-rules.md §1a.4`:
//!
//! - **T-BinaryArith-Int** — arithmetic operators (`+ - * / %`) unify
//!   the operand types and require both to be numeric. Floats follow
//!   the same shape (not separately listed in §1a.4; treated as a
//!   uniform `T-BinaryArith` here).
//! - **Comparison** (`== != < <= > >=`) — operands unify; result is
//!   `bool`. `<`/`<=`/`>`/`>=` further require the unified type to be
//!   numeric (ordering relations aren't admitted on `bool` / `String`
//!   / unit / never).
//! - **Logical** (`&& ||`) — both operands check against `bool`;
//!   result is `bool`.
//! - **Bitwise** (`& | ^`) and **Shift** (`<< >>`) — operands unify;
//!   result equals the unified type; require integer-typed operands.

use edda_span::Span;
use edda_syntax::ast::BinOp;

use crate::hir::HirExpr;
use crate::prim::Primitive;
use crate::ty::{TyId, TyKind};

use super::super::{InferCx, TyEnv, check_expr, synth_expr};
use super::overflow::attach_overflow_row;

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum BinClass {
    Arithmetic,    // + - * /   (numeric: integer or float)
    IntegerMod,    // %         (integer-only; floats use `std.math.scalar.fmod`)
    WrappingArith, // +% -% *% +| -| *|  (integer-only; modulo two's-complement / saturating-clamp)
    CheckedArith,  // +? -? *? %? (integer-only; originates `err: Overflow`)
    Comparison,    // == != < <= > >=
    Logical,       // && ||
    Bitwise,       // & | ^
    Shift,         // << >>
}

fn classify(op: BinOp) -> BinClass {
    match op {
        BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => BinClass::Arithmetic,
        BinOp::Mod => BinClass::IntegerMod,
        BinOp::WrapAdd | BinOp::WrapSub | BinOp::WrapMul
        | BinOp::SatAdd | BinOp::SatSub | BinOp::SatMul => BinClass::WrappingArith,
        BinOp::CheckAdd | BinOp::CheckSub | BinOp::CheckMul | BinOp::CheckMod => {
            BinClass::CheckedArith
        }
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            BinClass::Comparison
        }
        BinOp::And | BinOp::Or => BinClass::Logical,
        BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => BinClass::Bitwise,
        BinOp::Shl | BinOp::Shr => BinClass::Shift,
    }
}

// integer-only modulo operators whose float-operand diagnostic must
// point users at `std.math.scalar.fmod`
/// Whether `op` is one of the modulo operators (`%` or `%?`).
fn is_modulo(op: BinOp) -> bool {
    matches!(op, BinOp::Mod | BinOp::CheckMod)
}

fn requires_ordering(op: BinOp) -> bool {
    matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
}

/// Synthesise a binary expression's type.
pub(in crate::infer) fn synth_binary(
    env: &mut TyEnv,
    op: BinOp,
    lhs: &mut HirExpr,
    rhs: &mut HirExpr,
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    let class = classify(op);
    match class {
        BinClass::Logical => synth_logical(env, lhs, rhs, ic),
        BinClass::Arithmetic => synth_unified_numeric(env, lhs, rhs, ic, op, span),
        BinClass::IntegerMod | BinClass::WrappingArith | BinClass::Bitwise | BinClass::Shift => {
            synth_unified_integer(env, lhs, rhs, ic, op, span)
        }
        BinClass::CheckedArith => synth_checked_arith(env, lhs, rhs, ic, op, span),
        BinClass::Comparison => synth_comparison(env, lhs, rhs, ic, op, span),
    }
}

/// Check a binary expression against an expected type.
///
/// Arithmetic (`+ - * / %`), bitwise (`& | ^`), and shift (`<< >>`)
/// operators set the result type equal to the unified operand type, so
/// in checking position the expected type is pushed into both operand
/// positions. This is what drives integer-literal narrowing through a
/// nested arithmetic tree — without it, `return 1 + 2 * 3` declared
/// `-> i32` lowers with `i64`-typed temps (the synth default), which
/// trips the MIR emitter's `non-lowerable-local` guard because
/// the binary-temp type comes out as the `Error` sentinel.
///
/// Comparison and logical operators always produce `bool`; they
/// synthesise via [`synth_binary`] and then compare against `expected`
/// at the dispatch site (no element-checking needed because the
/// operand types don't depend on `expected`).
pub(in crate::infer) fn check_binary(
    env: &mut TyEnv,
    op: BinOp,
    lhs: &mut HirExpr,
    rhs: &mut HirExpr,
    expected: TyId,
    ic: &mut InferCx<'_>,
    span: Span,
) -> TyId {
    use super::super::path::check_synth_against_expected;

    let class = classify(op);
    match class {
        // Arithmetic: both operands check against `expected` if it's
        // numeric; otherwise fall back to synth+compare (which will
        // surface the standard "expected `T`, found `<numeric>`" error).
        BinClass::Arithmetic => {
            let is_numeric_expected = matches!(
                ic.lower.ty_interner.kind(expected),
                TyKind::Primitive(p) if p.is_numeric()
            );
            if is_numeric_expected {
                check_expr(env, lhs, expected, ic);
                check_expr(env, rhs, expected, ic);
                return expected;
            }
            let synth = synth_unified_numeric(env, lhs, rhs, ic, op, span);
            check_synth_against_expected(synth, expected, ic, span)
        }
        // Integer modulo / wrapping arithmetic / bitwise / shift:
        // operands must be integers; if `expected` is an integer, drive
        // narrowing on both operands.
        BinClass::IntegerMod | BinClass::WrappingArith | BinClass::Bitwise | BinClass::Shift => {
            let is_integer_expected = matches!(
                ic.lower.ty_interner.kind(expected),
                TyKind::Primitive(p) if p.is_integer()
            );
            if is_integer_expected {
                check_expr(env, lhs, expected, ic);
                check_expr(env, rhs, expected, ic);
                return expected;
            }
            let synth = synth_unified_integer(env, lhs, rhs, ic, op, span);
            check_synth_against_expected(synth, expected, ic, span)
        }
        // Checked arithmetic: same operand-type rule as wrapping, but
        // additionally contributes `err: Overflow` to the row. The
        // operand-narrowing path is shared; the row contribution is
        // attached inside `synth_checked_arith`.
        BinClass::CheckedArith => {
            let is_integer_expected = matches!(
                ic.lower.ty_interner.kind(expected),
                TyKind::Primitive(p) if p.is_integer()
            );
            if is_integer_expected {
                check_expr(env, lhs, expected, ic);
                check_expr(env, rhs, expected, ic);
                attach_overflow_row(ic, op, span);
                return expected;
            }
            let synth = synth_checked_arith(env, lhs, rhs, ic, op, span);
            check_synth_against_expected(synth, expected, ic, span)
        }
        // Comparison / logical always produce `bool`; synth and bridge.
        BinClass::Comparison | BinClass::Logical => {
            let synth = synth_binary(env, op, lhs, rhs, ic, span);
            check_synth_against_expected(synth, expected, ic, span)
        }
    }
}

/// `&&` / `||` — both operands check against `bool`; result `bool`.
fn synth_logical(
    env: &mut TyEnv,
    lhs: &mut HirExpr,
    rhs: &mut HirExpr,
    ic: &mut InferCx<'_>,
) -> TyId {
    let bool_ty = ic.lower.ty_interner.prim(Primitive::Bool);
    check_expr(env, lhs, bool_ty, ic);
    check_expr(env, rhs, bool_ty, ic);
    bool_ty
}

/// Arithmetic — unify lhs's type with rhs, require numeric, return unified.
fn synth_unified_numeric(
    env: &mut TyEnv,
    lhs: &mut HirExpr,
    rhs: &mut HirExpr,
    ic: &mut InferCx<'_>,
    op: BinOp,
    span: Span,
) -> TyId {
    let lhs_ty = synth_expr(env, lhs, ic);
    check_expr(env, rhs, lhs_ty, ic);
    let error = ic.ty_error();
    if lhs_ty == error {
        return lhs_ty;
    }
    if let TyKind::Primitive(p) = ic.lower.ty_interner.kind(lhs_ty)
        && p.is_numeric()
    {
        return lhs_ty;
    }
    let display = ic.display_ty(lhs_ty).to_string();
    ic.emit_typecheck_error(
        span,
        format!(
            "binary `{}` requires numeric operands, found `{display}`",
            op_keyword(op),
        ),
    );
    error
}

/// Checked arithmetic (`+? -? *?`) — integer-only operand check plus
/// `err: Overflow` row contribution. Overflow is resolved via the
/// function's declared row: the user must `import std.overflow` and
/// declare `with {err: Overflow, ...}` at the function boundary. If
/// no `err: T` in the declared row resolves to a nominal type named
/// `Overflow`, the operator is rejected with a precise diagnostic.
fn synth_checked_arith(
    env: &mut TyEnv,
    lhs: &mut HirExpr,
    rhs: &mut HirExpr,
    ic: &mut InferCx<'_>,
    op: BinOp,
    span: Span,
) -> TyId {
    let unified = synth_unified_integer(env, lhs, rhs, ic, op, span);
    if unified == ic.ty_error() {
        return unified;
    }
    attach_overflow_row(ic, op, span);
    unified
}

/// Bitwise / shift / integer modulo — unify lhs with rhs, require
/// integer, return unified. Modulo operators (`%` / `%?`) tag the
/// diagnostic with the canonical workaround when the operand is a
/// float: `std.math.scalar.fmod` is the float-modulo function name.
fn synth_unified_integer(
    env: &mut TyEnv,
    lhs: &mut HirExpr,
    rhs: &mut HirExpr,
    ic: &mut InferCx<'_>,
    op: BinOp,
    span: Span,
) -> TyId {
    let lhs_ty = synth_expr(env, lhs, ic);
    check_expr(env, rhs, lhs_ty, ic);
    let error = ic.ty_error();
    if lhs_ty == error {
        return lhs_ty;
    }
    if let TyKind::Primitive(p) = ic.lower.ty_interner.kind(lhs_ty)
        && p.is_integer()
    {
        return lhs_ty;
    }
    let display = ic.display_ty(lhs_ty).to_string();
    let is_float_operand = matches!(
        ic.lower.ty_interner.kind(lhs_ty),
        TyKind::Primitive(p) if p.is_float()
    );
    let message = if is_modulo(op) && is_float_operand {
        format!(
            "modulo (`{}`) is integer-only; use `std.math.scalar.fmod` for floats — found `{display}`",
            op_keyword(op),
        )
    } else {
        format!(
            "binary `{}` requires integer operands, found `{display}`",
            op_keyword(op),
        )
    };
    ic.emit_typecheck_error(span, message);
    error
}

/// `== != < <= > >=` — operands unify, result `bool`. Ordering
/// comparisons additionally require numeric operands.
fn synth_comparison(
    env: &mut TyEnv,
    lhs: &mut HirExpr,
    rhs: &mut HirExpr,
    ic: &mut InferCx<'_>,
    op: BinOp,
    span: Span,
) -> TyId {
    let lhs_ty = synth_expr(env, lhs, ic);
    check_expr(env, rhs, lhs_ty, ic);
    let bool_ty = ic.lower.ty_interner.prim(Primitive::Bool);
    if lhs_ty == ic.ty_error() {
        return bool_ty;
    }
    if requires_ordering(op) {
        let numeric = matches!(
            ic.lower.ty_interner.kind(lhs_ty),
            TyKind::Primitive(p) if p.is_numeric()
        );
        if !numeric {
            let display = ic.display_ty(lhs_ty).to_string();
            ic.emit_typecheck_error(
                span,
                format!(
                    "ordering operator `{}` requires numeric operands, found `{display}`",
                    op_keyword(op),
                ),
            );
        }
    }
    bool_ty
}

pub(in crate::infer::op) fn op_keyword(op: BinOp) -> &'static str {
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
