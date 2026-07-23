//! Diagnostic helpers and HIR-variant naming for the comptime evaluator.

use edda_diag::Diagnostics;
use edda_span::Span;
use edda_types::{HirExprKind, TyId, TyInterner, TyKind};

use crate::error::ComptimeError;
use crate::eval::op::OpError;

pub(super) fn push_panic(diags: &mut Diagnostics, span: Span, message: String) {
    let err = ComptimeError::Panic { span, message };
    diags.push(err.to_diagnostic());
}

pub(super) fn push_op_error(diags: &mut Diagnostics, span: Span, err: OpError) {
    let comp = ComptimeError::Panic {
        span,
        message: err.message(),
    };
    diags.push(comp.to_diagnostic());
}

pub(super) fn push_not_supported(diags: &mut Diagnostics, span: Span, what: &str) {
    let err = ComptimeError::Panic {
        span,
        message: format!("comptime evaluation does not yet support {what}"),
    };
    diags.push(err.to_diagnostic());
}

pub(super) fn variant_name(kind: &HirExprKind) -> &'static str {
    match kind {
        HirExprKind::Literal(_) => "literal",
        HirExprKind::FString(_) => "f-string",
        HirExprKind::Path(_) => "path",
        HirExprKind::Binary { .. } => "binary",
        HirExprKind::Unary { .. } => "unary",
        HirExprKind::Call { .. } => "call",
        HirExprKind::MethodCall { .. } => "method call",
        HirExprKind::Field { .. } => "field access",
        HirExprKind::TupleIndex { .. } => "tuple-index access",
        HirExprKind::Index { .. } => "index",
        HirExprKind::If { .. } => "if",
        HirExprKind::Match { .. } => "match",
        HirExprKind::Block(_) => "block",
        HirExprKind::Cast { .. } => "cast",
        HirExprKind::Range { .. } => "range",
        HirExprKind::Tuple(_) => "tuple construction",
        HirExprKind::Array(_) => "array construction",
        HirExprKind::StructLit { .. } => "struct literal",
        HirExprKind::Loop { .. } => "loop",
        HirExprKind::For { .. } => "for",
        HirExprKind::Try(_) => "try `?`",
        HirExprKind::Await(_) => "await",
        HirExprKind::Raise(_) => "raise",
        HirExprKind::Panic(_) => "panic",
        HirExprKind::Comptime(_) => "comptime prefix",
        HirExprKind::ComptimeBlock(_) => "comptime block",
        HirExprKind::Scope { .. } => "scope",
        HirExprKind::Return(_) => "return",
        HirExprKind::Break { .. } => "break",
        HirExprKind::Continue { .. } => "continue",
        HirExprKind::EffectRow(_) => "effect row literal",
        HirExprKind::Handle { .. } => "handle",
        HirExprKind::Forall { .. } => "forall",
        HirExprKind::Exists { .. } => "exists",
        HirExprKind::Closure(_) => "closure",
        HirExprKind::Spawn(_) => "spawn",
        HirExprKind::Error => "<error>",
    }
}

pub(super) fn ty_display(_kind: &TyKind, ty_interner: &TyInterner, id: TyId) -> String {
    ty_interner.display(id).to_string()
}
