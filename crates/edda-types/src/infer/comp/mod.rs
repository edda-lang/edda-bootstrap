//! Composite-expression inference.
//!
//! - **Tuple** (`(e1, e2, ..., en)`) — synthesise each element; the
//!   tuple's type is `TyKind::Tuple([t1, ..., tn])`. Tuples require
//!   `n >= 2`; a one-tuple from the parser (which shouldn't happen)
//!   yields `Error`.
//! - **Cast** (`e as T`) — numeric → numeric casts are admitted (integer
//!   to integer, integer ↔ float, float ↔ float). The target type
//!   was lowered by AST → HIR lowering (`ast::TypeKind` → `TyId`) and lives on
//!   `HirExprKind::Cast.target_ty`. Other cast forms (bool → integer,
//!   pointer / user-type casts) are deferred.
//! - **Index** (`e[i]`) — `e` must be a slice `[T]`, `i` must check
//!   against `usize`; result is `T`. The refinement side-condition
//!   `i < e.len()` from `T-IndexAccess` is the refinement layer's
//!   business and isn't enforced here.
//!
//! Module layout:
//!
//! - [`tuple`] — tuple construction + positional field access.
//! - [`cast`] — numeric `as` casts.
//! - [`index`] — slice indexing + range-element inference.
//! - [`comptime`] — `comptime` body context toggle.

mod array;
mod cast;
mod comptime;
mod index;
mod tuple;

pub(in crate::infer) use array::synth_array;
pub(in crate::infer) use cast::synth_cast;
pub(in crate::infer) use comptime::{synth_comptime, synth_comptime_block};
pub(in crate::infer) use index::{synth_index, synth_range};
pub(in crate::infer) use tuple::{synth_tuple, synth_tuple_index};

#[cfg(test)]
mod tests {
    use crate::cx::TyCx;
    use crate::infer::{InferCx, TyEnv, synth_expr};
    use crate::lower::LowerCx;
    use crate::lower::lower_expr;
    use crate::prim::Primitive;
    use crate::test_support::{Harness, path_for};
    use crate::ty::TyId;
    use edda_span::Span;
    use edda_syntax::ast::{Expr, ExprKind, Literal, Type, TypeKind};

    fn lit_int(value: u128) -> Expr {
        Expr {
            span: Span::DUMMY,
            kind: ExprKind::Literal(Literal::Int {
                value,
                base: edda_syntax::IntBase::Dec,
            }),
        }
    }

    fn lower_and_synth(h: &mut Harness, ast: &Expr) -> TyId {
        let cx = LowerCx {
            interner: &h.interner,
            ty_interner: &h.ty_interner,
            package: None,
        };
        let ty_cx = TyCx::new();
        let mut hir = lower_expr(ast, &cx, &mut h.diags, &h.lint_cfg);
        let mut env = TyEnv::new();
        let mut ic = InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags);
        synth_expr(&mut env, &mut hir, &mut ic)
    }

    fn ty_path(h: &Harness, name: &str) -> Type {
        Type {
            span: Span::DUMMY,
            kind: TypeKind::Path(path_for(&h.interner, &[name])),
        }
    }

    #[test]
    fn tuple_synthesises_structural_tuple_type() {
        let mut h = Harness::new();
        let bool_lit = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Literal(Literal::Bool(true)),
        };
        let e = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Tuple(vec![lit_int(1), bool_lit]),
        };
        let ty = lower_and_synth(&mut h, &e);
        let i64_id = h.ty_interner.prim(Primitive::I64);
        let bool_id = h.ty_interner.prim(Primitive::Bool);
        assert_eq!(ty, h.ty_interner.tuple([i64_id, bool_id]));
        assert!(h.diags.is_empty());
    }

    #[test]
    fn tuple_one_element_emits_diagnostic() {
        let mut h = Harness::new();
        let e = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Tuple(vec![lit_int(1)]),
        };
        let ty = lower_and_synth(&mut h, &e);
        assert_eq!(ty, h.ty_interner.error());
        assert!(
            h.diags
                .iter()
                .any(|d| d.message.contains("at least 2 elements"))
        );
    }

    #[test]
    fn cast_int_to_int_admitted() {
        let mut h = Harness::new();
        let e = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Cast {
                expr: Box::new(lit_int(42)),
                ty: Box::new(ty_path(&h, "u8")),
                mode: edda_syntax::ast::CastMode::Trap,
            },
        };
        let ty = lower_and_synth(&mut h, &e);
        assert_eq!(ty, h.ty_interner.prim(Primitive::U8));
        assert!(h.diags.is_empty());
    }

    #[test]
    fn cast_int_to_float_admitted() {
        let mut h = Harness::new();
        let e = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Cast {
                expr: Box::new(lit_int(5)),
                ty: Box::new(ty_path(&h, "f64")),
                mode: edda_syntax::ast::CastMode::Trap,
            },
        };
        let ty = lower_and_synth(&mut h, &e);
        assert_eq!(ty, h.ty_interner.prim(Primitive::F64));
        assert!(h.diags.is_empty());
    }

    #[test]
    fn cast_bool_to_int_rejected() {
        let mut h = Harness::new();
        let bool_lit = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Literal(Literal::Bool(true)),
        };
        let e = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Cast {
                expr: Box::new(bool_lit),
                ty: Box::new(ty_path(&h, "u8")),
                mode: edda_syntax::ast::CastMode::Trap,
            },
        };
        let ty = lower_and_synth(&mut h, &e);
        assert_eq!(ty, h.ty_interner.error());
        assert!(h.diags.iter().any(|d| d.message.contains("cannot cast")));
    }

    #[test]
    fn cast_with_error_target_propagates_silently() {
        // `42 as UnknownType` — lowering emits the unknown-type error,
        // and the cast itself should not add a second diagnostic.
        let mut h = Harness::new();
        let e = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Cast {
                expr: Box::new(lit_int(42)),
                ty: Box::new(ty_path(&h, "MyType")),
                mode: edda_syntax::ast::CastMode::Trap,
            },
        };
        let ty = lower_and_synth(&mut h, &e);
        assert_eq!(ty, h.ty_interner.error());
        // Only the lower-pass diagnostic — no extra from synth_cast.
        assert_eq!(h.diags.error_count(), 1);
    }

    #[test]
    fn index_on_slice_returns_element_type() {
        // We can't construct a slice value directly (no constructors),
        // and a `var xs: [u8]` with no initialiser is `Uninit` under
        // the §4 mode tracker — reading it would diagnose.
        // So we pre-bind `xs: [u8]` at `Valid` in the env directly and
        // synth just the index expression.
        let mut h = Harness::new();
        let xs_ident = crate::test_support::ident_for(&h.interner, "xs");
        let inner = ty_path(&h, "u8");
        let slice_ty_ast = Type {
            span: Span::DUMMY,
            kind: TypeKind::Slice(Box::new(inner)),
        };
        let cx = LowerCx {
            interner: &h.interner,
            ty_interner: &h.ty_interner,
            package: None,
        };
        let slice_ty =
            crate::lower::lower_type(&slice_ty_ast, &cx, &mut h.diags, &h.lint_cfg);
        let mut env = TyEnv::new();
        env.bind(xs_ident.name, slice_ty);

        let index_expr = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Index {
                receiver: Box::new(Expr {
                    span: Span::DUMMY,
                    kind: ExprKind::Path(edda_syntax::ast::Path {
                        segments: vec![xs_ident],
                        span: Span::DUMMY,
                    }),
                }),
                index: Box::new(lit_int(0)),
            },
        };
        let mut hir = lower_expr(&index_expr, &cx, &mut h.diags, &h.lint_cfg);
        let ty_cx = TyCx::new();
        let ty = synth_expr(
            &mut env,
            &mut hir,
            &mut InferCx::new(&cx, &ty_cx, &h.lint_cfg, &mut h.diags),
        );
        assert_eq!(ty, h.ty_interner.prim(Primitive::U8));
        assert!(h.diags.is_empty());
    }

    #[test]
    fn index_on_non_slice_emits_diagnostic() {
        // `1[0]` — receiver isn't a slice.
        let mut h = Harness::new();
        let e = Expr {
            span: Span::DUMMY,
            kind: ExprKind::Index {
                receiver: Box::new(lit_int(1)),
                index: Box::new(lit_int(0)),
            },
        };
        let ty = lower_and_synth(&mut h, &e);
        assert_eq!(ty, h.ty_interner.error());
        assert!(
            h.diags
                .iter()
                .any(|d| d.message.contains("only slice types"))
        );
    }
}
