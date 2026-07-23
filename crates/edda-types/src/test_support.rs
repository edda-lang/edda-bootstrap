//! Shared fixtures for every typing-pass test in edda-types.
//!
//! Each fixture builds a minimal `ast::*` node with `Span::DUMMY`
//! — the lowering / inference passes never dereference spans against
//! a `SourceMap`, so the dummy span is sufficient. The [`Harness`]
//! struct bundles the four pieces of mutable test state.

use edda_diag::{Diagnostics, LintConfig};
use edda_intern::Interner;
use edda_span::Span;
use edda_syntax::ast::{self, Expr, ExprKind, Ident, Literal, Path, Type, TypeKind};

use crate::ty::TyInterner;

/// Bundles the four pieces of mutable lowering state used across tests.
pub(crate) struct Harness {
    pub interner: Interner,
    pub ty_interner: TyInterner,
    pub diags: Diagnostics,
    pub lint_cfg: LintConfig,
}

impl Harness {
    pub fn new() -> Self {
        Self {
            interner: Interner::new(),
            ty_interner: TyInterner::new(),
            diags: Diagnostics::new(),
            lint_cfg: LintConfig::new(),
        }
    }
}

pub(crate) fn ident_for(interner: &Interner, text: &str) -> Ident {
    Ident {
        name: interner.intern(text),
        span: Span::DUMMY,
    }
}

pub(crate) fn path_for(interner: &Interner, segments: &[&str]) -> Path {
    Path {
        segments: segments.iter().map(|s| ident_for(interner, s)).collect(),
        span: Span::DUMMY,
    }
}

pub(crate) fn ast_ty(kind: TypeKind) -> Type {
    Type {
        span: Span::DUMMY,
        kind,
    }
}

pub(crate) fn ty_path(interner: &Interner, name: &str) -> Type {
    Type {
        span: Span::DUMMY,
        kind: TypeKind::Path(path_for(interner, &[name])),
    }
}

pub(crate) fn ast_row(members: Vec<ast::EffectMember>) -> ast::EffectRow {
    ast::EffectRow {
        span: Span::DUMMY,
        members,
    }
}

pub(crate) fn ast_param(
    interner: &Interner,
    name: &str,
    mode: ast::ParamMode,
    ty: Type,
) -> ast::Param {
    ast::Param {
        span: Span::DUMMY,
        name: ident_for(interner, name),
        mode,
        ty,
    }
}

pub(crate) fn empty_block() -> ast::Block {
    ast::Block {
        span: Span::DUMMY,
        stmts: vec![],
        trailing: None,
    }
}

pub(crate) fn ast_fn_decl(
    interner: &Interner,
    name: &str,
    params: Vec<ast::Param>,
    return_ty: Option<Type>,
    effects: Option<ast::EffectRow>,
) -> ast::FnDecl {
    ast::FnDecl {
        span: Span::DUMMY,
        stability: None,
        visibility: ast::Visibility::Module,
        refinement_stable: false,
        name: ident_for(interner, name),
        outbound_generics: vec![],
        generics: vec![],
        params,
        return_ty,
        return_mode: ast::ReturnMode::ByValue,
        effects,
        refinements: vec![],
        body: ast::FnBody::Block(empty_block()),
    }
}

/// Build an arbitrary expression node usable as a refinement
/// predicate placeholder. The structural type-system lowering
/// passes never inspect the predicate body.
pub(crate) fn synthesize_unit_expr() -> Expr {
    Expr {
        span: Span::DUMMY,
        kind: ExprKind::Literal(Literal::Unit),
    }
}
