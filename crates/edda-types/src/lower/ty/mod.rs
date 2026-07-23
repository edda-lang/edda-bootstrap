//! AST type-expression lowering — `ast::Type` → [`TyId`].
//!
//! Walks an `edda_syntax::ast::Type` and
//! produces a [`TyId`] via the surrounding [`TyInterner`]. Pure pass
//! (no I/O); diagnostics are pushed through the caller-supplied take.
//!
//! # Coverage
//!
//! Forms lowered directly to a structural [`TyKind`]:
//!
//! - Single-segment `Path` whose ident matches a [`Primitive`] name
//!   (`i32`, `bool`, `String`, …) — lowered to `Primitive(p)`.
//! - `Tuple` — recursive, validated `len >= 2` (the parser's invariant
//!   ensures this, but the lowering still rejects degenerate cases).
//! - `Slice` — recursive on the element type.
//! - `Unit` — lowered to `Primitive::Unit`.
//! - `Meta` — lowered to `Primitive::Type`.
//! - `Error` — already-diagnosed parser recovery; returns
//!   [`TyInterner::error`] silently.
//!
//! Forms that require follow-up work emit a
//! [`DiagnosticClass::TypecheckError`] and return [`TyInterner::error`]:
//!
//! - Multi-segment paths (`std.option.Option_i32`) — user-type
//!   resolution lands when `edda-resolve` exposes item-level
//!   `BindingId`s.
//! - Single-segment paths whose ident is not a primitive — same reason.
//! - `Function` — lowers to [`crate::TyKind::FnPtr`] via
//!   [`lower_function_type`]. Each parameter slot retains its mode and
//!   type; source-level parameter names are intentionally dropped
//!   (`function(i32)` and `function(x: i32)` produce the same `TyId`).
//! - `Comptime` — comptime-only types depend on the comptime evaluator
//!   in `edda-comptime`, deferred until it lands.
//!
//! Refinement clauses (`T where pred`) are lowered to their base type
//! `T` *silently*. The predicate is the refinement layer's concern
//! (`edda-refine`); the structural type representation does not carry
//! predicates.
//!
//! # Module layout
//!
//! - This file ([`ty`](self)) — the [`lower_type`] dispatcher over every
//!   `ast::TypeKind` variant.
//! - [`path`] — [`path::lower_path`]: the `Path`-shaped resolution order
//!   (primitive / capability shortcut, then nominal lookup through the
//!   resolved package), with the parser-recovery sentinel guards.
//! - [`compound`] — [`compound::lower_function_type`] and
//!   [`compound::lower_tuple`]: the structural composites.

use edda_diag::{Diagnostics, LintConfig};
use edda_syntax::ast::{self, TypeKind};

use crate::prim::Primitive;
use crate::ty::TyId;

use super::LowerCx;
use super::emit_typecheck_error;

mod compound;
mod path;

use compound::{lower_function_type, lower_tuple};
use path::lower_path;

/// Lower an AST type expression to its interned [`TyId`].
///
/// Pure pass: walks `ty` against the rules in this module's doc and
/// returns a structural type handle. Diagnostics are pushed to `diags`
/// when a form is not yet supported; the call-site lint configuration
/// gates the severity escalation.
///
/// Always returns a valid [`TyId`] — failures lower to
/// [`TyInterner::error`] so downstream passes can continue.
pub(crate) fn lower_type(
    ty: &ast::Type,
    cx: &LowerCx<'_>,
    diags: &mut Diagnostics,
    lint_cfg: &LintConfig,
) -> TyId {
    match &ty.kind {
        TypeKind::Unit => cx.ty_interner.prim(Primitive::Unit),
        TypeKind::Meta => cx.ty_interner.prim(Primitive::Type),
        TypeKind::Error => cx.ty_interner.error(),
        TypeKind::Path(path) => lower_path(path, ty.span, cx, diags, lint_cfg),
        TypeKind::Slice(elem) => {
            let elem_id = lower_type(elem, cx, diags, lint_cfg);
            cx.ty_interner.slice(elem_id)
        }
        TypeKind::Tuple(elems) => lower_tuple(elems, ty.span, cx, diags, lint_cfg),
        TypeKind::Function {
            params,
            ret,
            effects,
        } => lower_function_type(params, ret, effects.as_ref(), cx, diags, lint_cfg),
        TypeKind::Comptime(_) => {
            emit_typecheck_error(
                diags,
                lint_cfg,
                ty.span,
                "comptime type expressions are not yet supported",
            );
            cx.ty_interner.error()
        }
        TypeKind::Refined { base, .. } => {
            // Predicate is consumed by the refinement layer (edda-refine),
            // not the structural type representation. Lower base silently.
            lower_type(base, cx, diags, lint_cfg)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Harness, ast_ty, path_for, synthesize_unit_expr};
    use edda_diag::DiagnosticClass;

    fn lower(h: &mut Harness, ty: &ast::Type) -> TyId {
        let cx = LowerCx {
            interner: &h.interner,
            ty_interner: &h.ty_interner,
            package: None,
        };
        lower_type(ty, &cx, &mut h.diags, &h.lint_cfg)
    }

    #[test]
    fn unit_lowers_to_primitive_unit() {
        let mut h = Harness::new();
        let id = lower(&mut h, &ast_ty(TypeKind::Unit));
        assert_eq!(id, h.ty_interner.prim(Primitive::Unit));
        assert!(h.diags.is_empty());
    }

    #[test]
    fn meta_lowers_to_primitive_type() {
        let mut h = Harness::new();
        let id = lower(&mut h, &ast_ty(TypeKind::Meta));
        assert_eq!(id, h.ty_interner.prim(Primitive::Type));
        assert!(h.diags.is_empty());
    }

    #[test]
    fn ast_error_lowers_silently_to_error() {
        let mut h = Harness::new();
        let id = lower(&mut h, &ast_ty(TypeKind::Error));
        assert_eq!(id, h.ty_interner.error());
        assert!(h.diags.is_empty());
    }

    #[test]
    fn primitive_paths_lower_for_every_locked_name() {
        let mut h = Harness::new();
        for p in Primitive::ALL {
            let path = path_for(&h.interner, &[p.name()]);
            let id = lower(&mut h, &ast_ty(TypeKind::Path(path)));
            assert_eq!(id, h.ty_interner.prim(p), "primitive {p:?} did not round-trip");
        }
        assert!(h.diags.is_empty());
    }

    #[test]
    fn slice_lowers_recursively() {
        let mut h = Harness::new();
        let elem = ast_ty(TypeKind::Path(path_for(&h.interner, &["u8"])));
        let slice = ast_ty(TypeKind::Slice(Box::new(elem)));
        let id = lower(&mut h, &slice);
        let u8_id = h.ty_interner.prim(Primitive::U8);
        assert_eq!(id, h.ty_interner.slice(u8_id));
        assert!(h.diags.is_empty());
    }

    #[test]
    fn tuple_lowers_recursively() {
        let mut h = Harness::new();
        let i32_ast = ast_ty(TypeKind::Path(path_for(&h.interner, &["i32"])));
        let str_ast = ast_ty(TypeKind::Path(path_for(&h.interner, &["String"])));
        let tup = ast_ty(TypeKind::Tuple(vec![i32_ast, str_ast]));
        let id = lower(&mut h, &tup);
        let i32_id = h.ty_interner.prim(Primitive::I32);
        let str_id = h.ty_interner.prim(Primitive::String);
        assert_eq!(id, h.ty_interner.tuple([i32_id, str_id]));
        assert!(h.diags.is_empty());
    }

    #[test]
    fn nested_composition() {
        let mut h = Harness::new();
        let i32_ast = ast_ty(TypeKind::Path(path_for(&h.interner, &["i32"])));
        let u8_ast = ast_ty(TypeKind::Path(path_for(&h.interner, &["u8"])));
        let inner_slice = ast_ty(TypeKind::Slice(Box::new(u8_ast)));
        let inner_tup = ast_ty(TypeKind::Tuple(vec![i32_ast, inner_slice]));
        let outer = ast_ty(TypeKind::Slice(Box::new(inner_tup)));
        let id = lower(&mut h, &outer);
        assert_eq!(h.ty_interner.display(id).to_string(), "[(i32, [u8])]");
        assert!(h.diags.is_empty());
    }

    #[test]
    fn multi_segment_path_emits_diagnostic_and_returns_error() {
        let mut h = Harness::new();
        let path = path_for(&h.interner, &["std", "option", "Option"]);
        let id = lower(&mut h, &ast_ty(TypeKind::Path(path)));
        assert_eq!(id, h.ty_interner.error());
        assert_eq!(h.diags.error_count(), 1);
        let d = h.diags.iter().next().unwrap();
        assert_eq!(d.class, DiagnosticClass::TypecheckError);
        assert!(d.message.contains("qualified type paths"));
    }

    #[test]
    fn unknown_single_segment_emits_diagnostic_and_returns_error() {
        let mut h = Harness::new();
        let path = path_for(&h.interner, &["MyType"]);
        let id = lower(&mut h, &ast_ty(TypeKind::Path(path)));
        assert_eq!(id, h.ty_interner.error());
        assert_eq!(h.diags.error_count(), 1);
        let msg = &h.diags.iter().next().unwrap().message;
        assert!(msg.contains("MyType"));
        assert!(msg.contains("not a built-in primitive"));
    }

    #[test]
    fn case_sensitive_primitive_match() {
        let mut h = Harness::new();
        let path = path_for(&h.interner, &["I32"]);
        let id = lower(&mut h, &ast_ty(TypeKind::Path(path)));
        assert_eq!(id, h.ty_interner.error());
        assert_eq!(h.diags.error_count(), 1);
    }

    #[test]
    fn nullary_function_type_lowers_to_fn_ptr() {
        let mut h = Harness::new();
        let ret = Box::new(ast_ty(TypeKind::Unit));
        let func = ast_ty(TypeKind::Function {
            params: vec![],
            ret,
            effects: None,
        });
        let id = lower(&mut h, &func);
        assert_ne!(id, h.ty_interner.error());
        assert!(h.diags.is_empty());
        let kind = h.ty_interner.kind(id);
        let crate::ty::TyKind::FnPtr(sig) = kind else {
            panic!("expected TyKind::FnPtr, got {kind:?}");
        };
        assert_eq!(sig.params.len(), 0);
        assert_eq!(sig.return_ty, h.ty_interner.prim(crate::Primitive::Unit));
        assert!(sig.effects.is_empty());
    }

    #[test]
    fn function_type_param_names_do_not_affect_identity() {
        let mut h = Harness::new();
        let bare = ast_ty(TypeKind::Function {
            params: vec![edda_syntax::ast::FnTypeParam {
                span: edda_span::Span::DUMMY,
                name: None,
                mode: edda_syntax::ast::ParamMode::Default,
                ty: ast_ty(TypeKind::Path(path_for(&h.interner, &["i32"]))),
            }],
            ret: Box::new(ast_ty(TypeKind::Path(path_for(&h.interner, &["i32"])))),
            effects: None,
        });
        let named = ast_ty(TypeKind::Function {
            params: vec![edda_syntax::ast::FnTypeParam {
                span: edda_span::Span::DUMMY,
                name: Some(edda_syntax::ast::Ident {
                    name: h.interner.intern("x"),
                    span: edda_span::Span::DUMMY,
                }),
                mode: edda_syntax::ast::ParamMode::Default,
                ty: ast_ty(TypeKind::Path(path_for(&h.interner, &["i32"]))),
            }],
            ret: Box::new(ast_ty(TypeKind::Path(path_for(&h.interner, &["i32"])))),
            effects: None,
        });
        let bare_id = lower(&mut h, &bare);
        let named_id = lower(&mut h, &named);
        assert_eq!(
            bare_id, named_id,
            "function(i32) and function(x: i32) must share the same TyId — names are not part of the type"
        );
        assert!(h.diags.is_empty());
    }

    #[test]
    fn comptime_type_emits_diagnostic_and_returns_error() {
        let mut h = Harness::new();
        let inner = Box::new(ast_ty(TypeKind::Path(path_for(&h.interner, &["i32"]))));
        let ct = ast_ty(TypeKind::Comptime(inner));
        let id = lower(&mut h, &ct);
        assert_eq!(id, h.ty_interner.error());
        assert_eq!(h.diags.error_count(), 1);
        assert!(
            h.diags
                .iter()
                .next()
                .unwrap()
                .message
                .contains("comptime")
        );
    }

    #[test]
    fn refined_lowers_base_silently() {
        let mut h = Harness::new();
        let base = Box::new(ast_ty(TypeKind::Path(path_for(&h.interner, &["i32"]))));
        let pred = synthesize_unit_expr();
        let refined = ast_ty(TypeKind::Refined { base, pred });
        let id = lower(&mut h, &refined);
        assert_eq!(id, h.ty_interner.prim(Primitive::I32));
        assert!(h.diags.is_empty());
    }

    #[test]
    fn ast_error_inside_composite_does_not_double_diagnose() {
        let mut h = Harness::new();
        let inner = ast_ty(TypeKind::Error);
        let slice = ast_ty(TypeKind::Slice(Box::new(inner)));
        let id = lower(&mut h, &slice);
        assert_eq!(id, h.ty_interner.slice(h.ty_interner.error()));
        assert!(h.diags.is_empty());
    }

    // Regression: when a stdlib module
    // failed to fully build, the resolver could record (or the parser
    // could produce) a single-segment Path whose head segment carries
    // the parser-recovery sentinel `Symbol::DUMMY`. `lower_path` then
    // walked into `cx.interner.resolve` (a stable @stable API that
    // panics on DUMMY by contract) and tore down the bootstrap with
    // `edda-intern: Symbol(4294967295) is out of range`.
    //
    // The fix routes every interner-touching code path in `lower_path`
    // through `try_resolve`, cascading to the Error sentinel without
    // re-diagnosing — the upstream parse_error / import_resolution_error
    // is already on the diagnostic take. The matrix below covers every
    // such site so a future refactor that bypasses one of them surfaces
    // here instead of in production.
    #[test]
    fn dummy_head_single_segment_does_not_panic_without_package() {
        let mut h = Harness::new();
        let path = ast::Path {
            segments: vec![edda_syntax::ast::Ident {
                name: edda_intern::Symbol::DUMMY,
                span: edda_span::Span::DUMMY,
            }],
            span: edda_span::Span::DUMMY,
        };
        let id = lower(&mut h, &ast_ty(TypeKind::Path(path)));
        // Cascade silently — the parse error owns the diagnostic; this
        // lowering must neither panic nor re-emit.
        assert_eq!(id, h.ty_interner.error());
        assert!(h.diags.is_empty());
    }

    #[test]
    fn dummy_head_inside_composite_does_not_panic() {
        let mut h = Harness::new();
        let dummy_path = ast::Path {
            segments: vec![edda_syntax::ast::Ident {
                name: edda_intern::Symbol::DUMMY,
                span: edda_span::Span::DUMMY,
            }],
            span: edda_span::Span::DUMMY,
        };
        // A `take param: [DUMMY]` parameter type — slice over a
        // parser-recovered head. The structural lowering still produces
        // a slice node; the element lowers cleanly to the Error sentinel.
        let elem = ast_ty(TypeKind::Path(dummy_path));
        let slice = ast_ty(TypeKind::Slice(Box::new(elem)));
        let id = lower(&mut h, &slice);
        assert_eq!(id, h.ty_interner.slice(h.ty_interner.error()));
        assert!(h.diags.is_empty());
    }
}
