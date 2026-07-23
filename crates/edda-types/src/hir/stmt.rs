//! Typed statement nodes (`HirStmt`, `HirStmtKind`).
//!
//! HIR statements mirror `ast::StmtKind` one-for-one: `Let`, `Assign`,
//! and bare-`Expr`. The variant set is locked surface.

use edda_span::Span;
use edda_syntax::ast::{AssignOp, BindingMode};

use crate::ty::TyId;

use super::{HirExpr, HirPat};

/// A single statement inside a [`super::HirBlock`].
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct HirStmt {
    /// Source range covering the statement.
    pub span: Span,
    /// Variant and payload.
    pub kind: HirStmtKind,
}

/// Every statement form admitted in a [`super::HirBlock`].
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum HirStmtKind {
    /// `let pat [: T] [= init]` or `var pat [: T] [= init]`.
    Let {
        /// `let` (immutable) vs `var` (mutable).
        mutability: BindingMode,
        /// Binding pattern. Pattern's `ty` field is the bound value type
        /// (either inferred or annotated).
        pat: HirPat,
        /// Annotated type from source (`let x: T = …`), or [`None`]
        /// when the user omitted the annotation. The pattern's `ty`
        /// field is the authoritative bound type after inference;
        /// `ty` here is preserved so diagnostics can point at the
        /// annotation site when the annotation disagrees with the
        /// initialiser.
        ty: Option<TyId>,
        /// Initialiser expression. Absent only when an explicit type
        /// annotation is present (per the `ast::StmtKind` invariant).
        init: Option<HirExpr>,
    },
    /// `target op rhs` assignment statement.
    Assign {
        /// Place expression being assigned to (lvalue).
        target: HirExpr,
        /// Assignment operator (`=`, `+=`, ...).
        op: AssignOp,
        /// Right-hand-side value.
        rhs: HirExpr,
    },
    /// A bare expression used for its effect (or as the block's trailer
    /// when the AST trailing expression is desugared into a statement
    /// inside the block — AST → HIR lowering decides the canonical form).
    Expr(HirExpr),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prim::Primitive;
    use crate::ty::TyInterner;
    use crate::hir::{HirExpr, HirExprKind, HirPat, HirPatKind};
    use edda_intern::Interner;
    use edda_syntax::ast::{AssignOp, BindingMode, Ident, Literal};

    fn lit(ty: &TyInterner) -> HirExpr {
        HirExpr {
            span: Span::DUMMY,
            ty: ty.prim(Primitive::Bool),
            kind: HirExprKind::Literal(Literal::Bool(true)),
        }
    }

    fn binding_pat(interner: &Interner, ty: &TyInterner, name: &str) -> HirPat {
        HirPat {
            span: Span::DUMMY,
            ty: ty.prim(Primitive::Bool),
            kind: HirPatKind::Binding(Ident {
                name: interner.intern(name),
                span: Span::DUMMY,
            }),
        }
    }

    #[test]
    fn let_stmt_round_trips() {
        let interner = Interner::new();
        let ty = TyInterner::new();
        let s = HirStmt {
            span: Span::DUMMY,
            kind: HirStmtKind::Let {
                mutability: BindingMode::Immutable,
                pat: binding_pat(&interner, &ty, "x"),
                ty: None,
                init: Some(lit(&ty)),
            },
        };
        let cloned = s.clone();
        assert_eq!(s, cloned);
    }

    #[test]
    fn var_with_annotation_no_init() {
        // `var x: bool` — declaration with no initialiser, init filled later.
        let interner = Interner::new();
        let ty = TyInterner::new();
        let s = HirStmt {
            span: Span::DUMMY,
            kind: HirStmtKind::Let {
                mutability: BindingMode::Mutable,
                pat: binding_pat(&interner, &ty, "buf"),
                ty: Some(ty.prim(Primitive::Bool)),
                init: None,
            },
        };
        match &s.kind {
            HirStmtKind::Let {
                mutability,
                ty: ann,
                init,
                ..
            } => {
                assert_eq!(*mutability, BindingMode::Mutable);
                assert_eq!(*ann, Some(ty.prim(Primitive::Bool)));
                assert!(init.is_none());
            }
            _ => panic!("expected Let"),
        }
    }

    #[test]
    fn assign_compound_op_round_trips() {
        let ty = TyInterner::new();
        let s = HirStmt {
            span: Span::DUMMY,
            kind: HirStmtKind::Assign {
                target: lit(&ty),
                op: AssignOp::Add,
                rhs: lit(&ty),
            },
        };
        match &s.kind {
            HirStmtKind::Assign { op, .. } => assert_eq!(*op, AssignOp::Add),
            _ => panic!("expected Assign"),
        }
    }

    #[test]
    fn expr_stmt_wraps_inner_expr() {
        let ty = TyInterner::new();
        let inner = lit(&ty);
        let s = HirStmt {
            span: Span::DUMMY,
            kind: HirStmtKind::Expr(inner.clone()),
        };
        match &s.kind {
            HirStmtKind::Expr(e) => assert_eq!(*e, inner),
            _ => panic!("expected Expr"),
        }
    }
}
