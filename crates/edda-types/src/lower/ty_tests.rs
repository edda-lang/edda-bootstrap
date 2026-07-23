//! Tests for [`super::lower_type`] and the nominal-path resolution
//! it picks up through [`super::super::LowerCx::resolutions`].

use super::*;
use crate::test_support::{Harness, ast_ty, path_for, synthesize_unit_expr};
use edda_diag::DiagnosticClass;

fn lower(h: &mut Harness, ty: &ast::Type) -> TyId {
    let cx = LowerCx::new(&h.interner, &h.ty_interner);
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
fn multi_segment_path_without_resolutions_emits_diagnostic() {
    let mut h = Harness::new();
    let path = path_for(&h.interner, &["std", "option", "Option"]);
    let id = lower(&mut h, &ast_ty(TypeKind::Path(path)));
    assert_eq!(id, h.ty_interner.error());
    assert_eq!(h.diags.error_count(), 1);
    let d = h.diags.iter().next().unwrap();
    assert_eq!(d.class, DiagnosticClass::TypecheckError);
    assert!(d.message.contains("qualified type paths"));
    assert!(d.message.contains("resolution artifact"));
}

#[test]
fn unknown_single_segment_without_resolutions_emits_diagnostic() {
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
fn function_type_emits_diagnostic_and_returns_error() {
    let mut h = Harness::new();
    let ret = Box::new(ast_ty(TypeKind::Unit));
    let func = ast_ty(TypeKind::Function {
        params: vec![],
        ret,
        effects: None,
    });
    let id = lower(&mut h, &func);
    assert_eq!(id, h.ty_interner.error());
    assert_eq!(h.diags.error_count(), 1);
    assert!(
        h.diags
            .iter()
            .next()
            .unwrap()
            .message
            .contains("function types")
    );
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

// ---- Nominal user-type lowering via Resolutions --------------------

use edda_resolve::{BindingId, ModuleId, Resolutions, Resolved};
use edda_syntax::ast::Ident;

/// Build an `ast::Path` whose span is `Span::DUMMY` — every test
/// in this module uses its own `Resolutions`, so the shared
/// sentinel span is unambiguous within a single test.
fn ast_path(interner: &edda_intern::Interner, segments: &[&str]) -> ast::Path {
    ast::Path {
        segments: segments
            .iter()
            .map(|s| Ident {
                name: interner.intern(s),
                span: Span::DUMMY,
            })
            .collect(),
        span: Span::DUMMY,
    }
}

#[test]
fn single_segment_nominal_path_lowers_to_nominal_binding() {
    let mut h = Harness::new();
    let path = ast_path(&h.interner, &["Point"]);
    let binding_id = BindingId::new(ModuleId::from_raw(0), 3);
    let mut resolutions = Resolutions::new();
    resolutions.record_for_testing(path.span, Resolved::Binding(binding_id));

    let cx = LowerCx::new(&h.interner, &h.ty_interner).with_resolutions(&resolutions);
    let id = lower_type(
        &ast_ty(TypeKind::Path(path)),
        &cx,
        &mut h.diags,
        &h.lint_cfg,
    );
    assert_eq!(id, h.ty_interner.nominal(binding_id));
    assert!(h.diags.is_empty());
}

#[test]
fn multi_segment_nominal_path_lowers_to_nominal_binding() {
    let mut h = Harness::new();
    let path = ast_path(&h.interner, &["std", "option", "Option"]);
    let binding_id = BindingId::new(ModuleId::from_raw(2), 7);
    let mut resolutions = Resolutions::new();
    resolutions.record_for_testing(path.span, Resolved::Binding(binding_id));

    let cx = LowerCx::new(&h.interner, &h.ty_interner).with_resolutions(&resolutions);
    let id = lower_type(
        &ast_ty(TypeKind::Path(path)),
        &cx,
        &mut h.diags,
        &h.lint_cfg,
    );
    assert_eq!(id, h.ty_interner.nominal(binding_id));
    assert!(h.diags.is_empty());
}

#[test]
fn primitive_path_still_takes_fast_path_with_resolutions() {
    let mut h = Harness::new();
    // Even with resolutions attached, a primitive name should
    // skip the resolution lookup and lower directly.
    let resolutions = Resolutions::new();
    let cx = LowerCx::new(&h.interner, &h.ty_interner).with_resolutions(&resolutions);
    let path = path_for(&h.interner, &["i32"]);
    let id = lower_type(
        &ast_ty(TypeKind::Path(path)),
        &cx,
        &mut h.diags,
        &h.lint_cfg,
    );
    assert_eq!(id, h.ty_interner.prim(Primitive::I32));
    assert!(h.diags.is_empty());
}

#[test]
fn nominal_path_resolving_to_module_emits_diagnostic() {
    let mut h = Harness::new();
    let path = ast_path(&h.interner, &["fs"]);
    let module_id = ModuleId::from_raw(5);
    let mut resolutions = Resolutions::new();
    resolutions.record_for_testing(path.span, Resolved::Module(module_id));

    let cx = LowerCx::new(&h.interner, &h.ty_interner).with_resolutions(&resolutions);
    let id = lower_type(
        &ast_ty(TypeKind::Path(path)),
        &cx,
        &mut h.diags,
        &h.lint_cfg,
    );
    assert_eq!(id, h.ty_interner.error());
    assert_eq!(h.diags.error_count(), 1);
    let msg = &h.diags.iter().next().unwrap().message;
    assert!(msg.contains("module, not a type"));
}

#[test]
fn nominal_path_missing_from_resolutions_emits_diagnostic() {
    let mut h = Harness::new();
    let path = ast_path(&h.interner, &["Unknown"]);
    let resolutions = Resolutions::new();
    let cx = LowerCx::new(&h.interner, &h.ty_interner).with_resolutions(&resolutions);
    let id = lower_type(
        &ast_ty(TypeKind::Path(path)),
        &cx,
        &mut h.diags,
        &h.lint_cfg,
    );
    assert_eq!(id, h.ty_interner.error());
    assert_eq!(h.diags.error_count(), 1);
    let msg = &h.diags.iter().next().unwrap().message;
    assert!(msg.contains("not resolved"));
    assert!(msg.contains("Unknown"));
}

#[test]
fn already_errored_resolution_propagates_silently() {
    let mut h = Harness::new();
    let path = ast_path(&h.interner, &["Whatever"]);
    let mut resolutions = Resolutions::new();
    resolutions.record_for_testing(path.span, Resolved::Error);

    let cx = LowerCx::new(&h.interner, &h.ty_interner).with_resolutions(&resolutions);
    let id = lower_type(
        &ast_ty(TypeKind::Path(path)),
        &cx,
        &mut h.diags,
        &h.lint_cfg,
    );
    assert_eq!(id, h.ty_interner.error());
    // The resolver already emitted; we do not double-diagnose.
    assert!(h.diags.is_empty());
}

#[test]
fn nominal_dedup_via_interner() {
    let h = Harness::new();
    let binding = BindingId::new(ModuleId::from_raw(0), 0);
    let a = h.ty_interner.nominal(binding);
    let b = h.ty_interner.nominal(binding);
    assert_eq!(a, b);
}

#[test]
fn nominal_distinct_bindings_distinct_ids() {
    let h = Harness::new();
    let b1 = BindingId::new(ModuleId::from_raw(0), 0);
    let b2 = BindingId::new(ModuleId::from_raw(0), 1);
    assert_ne!(h.ty_interner.nominal(b1), h.ty_interner.nominal(b2));
}
