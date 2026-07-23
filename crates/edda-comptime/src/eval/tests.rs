//! Tests for the comptime built-in dispatch and the HIR expression
//! evaluator.

use super::*;
use edda_diag::{DiagnosticClass, Diagnostics};
use edda_intern::Interner;
use edda_span::Span;
use edda_target::{AbiVariant, Arch, FeatureSet, Os, TargetTriple};
use edda_types::{Primitive, TyInterner};

use crate::value::Value;

fn x86_64_with_avx2() -> TargetCfg {
    let mut features = FeatureSet::new(Arch::X86_64);
    features.insert("avx2").unwrap();
    TargetCfg::with_features(
        TargetTriple::new(Arch::X86_64, Os::Linux, AbiVariant::Gnu),
        features,
    )
    .unwrap()
}

fn extract_usize(v: Value) -> u128 {
    match v {
        Value::Int(i) => {
            assert_eq!(i.width(), Primitive::Usize);
            i.as_u128().unwrap()
        }
        other => panic!("expected Value::Int(Usize), got {:?}", other),
    }
}

#[test]
fn size_of_primitive_returns_usize_value() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let arg = Value::Type(ty.prim(Primitive::I32));
    let result = eval_builtin(
        Builtin::SizeOf,
        &[arg],
        Span::DUMMY,
        &ty,
        &target,
        &interner,
        &mut diags,
    )
    .unwrap();
    assert_eq!(extract_usize(result), 4);
    assert!(diags.is_empty());
}

#[test]
fn align_of_primitive_returns_usize_value() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let arg = Value::Type(ty.prim(Primitive::U128));
    let result = eval_builtin(
        Builtin::AlignOf,
        &[arg],
        Span::DUMMY,
        &ty,
        &target,
        &interner,
        &mut diags,
    )
    .unwrap();
    assert_eq!(extract_usize(result), 16);
}

#[test]
fn size_of_unit_is_zero() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let arg = Value::Type(ty.prim(Primitive::Unit));
    let result = eval_builtin(
        Builtin::SizeOf,
        &[arg],
        Span::DUMMY,
        &ty,
        &target,
        &interner,
        &mut diags,
    )
    .unwrap();
    assert_eq!(extract_usize(result), 0);
}

#[test]
fn size_of_tuple_lays_out_in_declaration_order() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    // (i64, i64) → size 16, align 8.
    let pair = ty.tuple([ty.prim(Primitive::I64), ty.prim(Primitive::I64)]);
    let mut diags = Diagnostics::new();
    let size = eval_builtin(
        Builtin::SizeOf,
        &[Value::Type(pair)],
        Span::DUMMY,
        &ty,
        &target,
        &interner,
        &mut diags,
    )
    .unwrap();
    assert_eq!(extract_usize(size), 16);
    assert!(diags.is_empty());
    let mut diags2 = Diagnostics::new();
    let align = eval_builtin(
        Builtin::AlignOf,
        &[Value::Type(pair)],
        Span::DUMMY,
        &ty,
        &target,
        &interner,
        &mut diags2,
    )
    .unwrap();
    assert_eq!(extract_usize(align), 8);
}

#[test]
fn target_has_returns_true_for_enabled_feature() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let sym = interner.intern("avx2");
    let mut diags = Diagnostics::new();
    let result = eval_builtin(
        Builtin::TargetHas,
        &[Value::Str(sym)],
        Span::DUMMY,
        &ty,
        &target,
        &interner,
        &mut diags,
    )
    .unwrap();
    assert!(matches!(result, Value::Bool(true)));
    assert!(diags.is_empty());
}

#[test]
fn target_has_returns_false_for_disabled_feature() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let sym = interner.intern("sse4.2");
    let mut diags = Diagnostics::new();
    let result = eval_builtin(
        Builtin::TargetHas,
        &[Value::Str(sym)],
        Span::DUMMY,
        &ty,
        &target,
        &interner,
        &mut diags,
    )
    .unwrap();
    assert!(matches!(result, Value::Bool(false)));
}

#[test]
fn target_has_emits_unknown_target_feature_diagnostic() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let sym = interner.intern("neon"); // aarch64-only feature on x86_64
    let mut diags = Diagnostics::new();
    let result = eval_builtin(
        Builtin::TargetHas,
        &[Value::Str(sym)],
        Span::DUMMY,
        &ty,
        &target,
        &interner,
        &mut diags,
    );
    assert!(result.is_none());
    assert_eq!(diags.error_count(), 1);
    let d = diags.iter().next().unwrap();
    assert_eq!(d.class, DiagnosticClass::UnknownTargetFeature);
}

#[test]
fn arity_mismatch_is_a_typecheck_error() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let mut diags = Diagnostics::new();
    let result = eval_builtin(
        Builtin::SizeOf,
        &[],
        Span::DUMMY,
        &ty,
        &target,
        &interner,
        &mut diags,
    );
    assert!(result.is_none());
    assert_eq!(diags.error_count(), 1);
    let d = diags.iter().next().unwrap();
    assert_eq!(d.class, DiagnosticClass::TypecheckError);
    assert!(d.message.contains("size_of"));
}

#[test]
fn argument_kind_mismatch_is_a_typecheck_error() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let sym = interner.intern("i32");
    let mut diags = Diagnostics::new();
    let result = eval_builtin(
        Builtin::SizeOf,
        &[Value::Str(sym)],
        Span::DUMMY,
        &ty,
        &target,
        &interner,
        &mut diags,
    );
    assert!(result.is_none());
    assert_eq!(diags.error_count(), 1);
    let d = diags.iter().next().unwrap();
    assert_eq!(d.class, DiagnosticClass::TypecheckError);
}

#[test]
fn offset_of_is_not_yet_supported() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let sym = interner.intern("version");
    let mut diags = Diagnostics::new();
    let result = eval_builtin(
        Builtin::OffsetOf,
        &[
            Value::Type(ty.prim(Primitive::I32)),
            Value::Str(sym),
        ],
        Span::DUMMY,
        &ty,
        &target,
        &interner,
        &mut diags,
    );
    assert!(result.is_none());
    assert_eq!(diags.error_count(), 1);
    let d = diags.iter().next().unwrap();
    assert_eq!(d.class, DiagnosticClass::TypecheckError);
    assert!(d.message.contains("offset_of"));
}

#[test]
fn size_of_slice_reports_deferred_layout() {
    let target = x86_64_with_avx2();
    let interner = Interner::new();
    let ty = TyInterner::new();
    let slice = ty.slice(ty.prim(Primitive::U8));
    let mut diags = Diagnostics::new();
    let result = eval_builtin(
        Builtin::SizeOf,
        &[Value::Type(slice)],
        Span::DUMMY,
        &ty,
        &target,
        &interner,
        &mut diags,
    );
    assert!(result.is_none());
    assert_eq!(diags.error_count(), 1);
    let d = diags.iter().next().unwrap();
    assert_eq!(d.class, DiagnosticClass::TypecheckError);
    assert!(d.message.contains("slice"));
}

