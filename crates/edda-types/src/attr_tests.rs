//! Tests for the slice-2 attribute registry and the
//! [`crate::validate_attributes`] entry point.

use edda_intern::Interner;
use edda_span::Span;
use edda_syntax::IntBase;
use edda_syntax::ast::{AttrArg, AttrLit, Attribute, Ident};

use edda_diag::DiagnosticClass;

use crate::attr::{AttrAbi, AttrLayout, AttrRepr, AttrTarget, validate_attributes};
use crate::test_support::Harness;

fn ident(interner: &Interner, text: &str) -> Ident {
    Ident {
        name: interner.intern(text),
        span: Span::DUMMY,
    }
}

fn attr_with_args(interner: &Interner, name: &str, args: Vec<AttrArg>) -> Attribute {
    Attribute {
        span: Span::DUMMY,
        name: ident(interner, name),
        args,
    }
}

fn lit_str(interner: &Interner, s: &str) -> AttrArg {
    AttrArg::Lit {
        span: Span::DUMMY,
        lit: AttrLit::Str(interner.intern(s)),
    }
}

fn lit_int(value: u128) -> AttrArg {
    AttrArg::Lit {
        span: Span::DUMMY,
        lit: AttrLit::Int {
            value,
            base: IntBase::Dec,
        },
    }
}

fn ident_arg(interner: &Interner, text: &str) -> AttrArg {
    AttrArg::Ident(ident(interner, text))
}

fn named_str_arg(interner: &Interner, key: &str, value: &str) -> AttrArg {
    AttrArg::Named {
        span: Span::DUMMY,
        key: ident(interner, key),
        value: Box::new(lit_str(interner, value)),
    }
}

/// `true` when at least one emitted diagnostic carries the given class.
fn has_class(h: &Harness, class: DiagnosticClass) -> bool {
    h.diags.iter().any(|d| d.class == class)
}

// === Closed-nine membership (D-18) ====================================

// `@export` was retired with the closed nine — it now rejects as
// `unknown_attribute` rather than populating a (deleted) `AttrSet.export`.
#[test]
fn export_is_now_unknown_attribute() {
    let mut h = Harness::new();
    let attr = attr_with_args(
        &h.interner,
        "export",
        vec![lit_str(&h.interner, "edda_callback")],
    );
    let set = validate_attributes(
        &[attr],
        AttrTarget::Function,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(has_class(&h, DiagnosticClass::UnknownAttribute));
    assert!(set.is_empty());
}

// `@stable` / `@unstable` are keywords (D-19), not attributes — rejected.
#[test]
fn stable_unstable_are_unknown_attributes() {
    for name in ["stable", "unstable"] {
        let mut h = Harness::new();
        let attr = attr_with_args(&h.interner, name, vec![]);
        validate_attributes(
            &[attr],
            AttrTarget::Function,
            &h.interner,
            &h.lint_cfg,
            &mut h.diags,
        );
        assert!(
            has_class(&h, DiagnosticClass::UnknownAttribute),
            "@{name} must reject as unknown_attribute"
        );
    }
}

#[test]
fn trust_valid_with_reason() {
    let mut h = Harness::new();
    let attr = attr_with_args(
        &h.interner,
        "trust",
        vec![named_str_arg(&h.interner, "reason", "single NLA step")],
    );
    let set = validate_attributes(
        &[attr],
        AttrTarget::Function,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(!h.diags.has_errors());
    // Name-accepted only — no AttrSet payload yet.
    assert!(set.is_empty());
}

#[test]
fn trust_rejects_empty_reason() {
    let mut h = Harness::new();
    let attr = attr_with_args(
        &h.interner,
        "trust",
        vec![named_str_arg(&h.interner, "reason", "")],
    );
    validate_attributes(
        &[attr],
        AttrTarget::Function,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(h.diags.has_errors());
}

#[test]
fn deprecated_valid_with_reason() {
    let mut h = Harness::new();
    let attr = attr_with_args(
        &h.interner,
        "deprecated",
        vec![named_str_arg(&h.interner, "reason", "use NetClient.send")],
    );
    let set = validate_attributes(
        &[attr],
        AttrTarget::TypeDecl,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(!h.diags.has_errors());
    assert!(set.is_empty());
}

#[test]
fn property_is_name_accepted() {
    let mut h = Harness::new();
    let attr = attr_with_args(&h.interner, "property", vec![]);
    let set = validate_attributes(
        &[attr],
        AttrTarget::Function,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(!h.diags.has_errors());
    assert!(set.is_empty());
}

// === @target_requires =================

#[test]
fn target_requires_valid_locked_capability_populates_payload() {
    let mut h = Harness::new();
    let attr = attr_with_args(
        &h.interner,
        "target_requires",
        vec![ident_arg(&h.interner, "Subprocess")],
    );
    let set = validate_attributes(
        &[attr],
        AttrTarget::Function,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(!h.diags.has_errors());
    let tr = set.target_requires.expect("target_requires populated");
    assert_eq!(h.interner.resolve(tr.capability), "Subprocess");
}

#[test]
fn target_requires_valid_experimental_capability_populates_payload() {
    let mut h = Harness::new();
    let attr = attr_with_args(
        &h.interner,
        "target_requires",
        vec![ident_arg(&h.interner, "Window")],
    );
    let set = validate_attributes(
        &[attr],
        AttrTarget::Function,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(!h.diags.has_errors());
    let tr = set.target_requires.expect("target_requires populated");
    assert_eq!(h.interner.resolve(tr.capability), "Window");
}

#[test]
fn target_requires_rejects_unknown_capability_name() {
    let mut h = Harness::new();
    let attr = attr_with_args(
        &h.interner,
        "target_requires",
        vec![ident_arg(&h.interner, "Simd")],
    );
    let set = validate_attributes(
        &[attr],
        AttrTarget::Function,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(h.diags.has_errors());
    assert!(set.target_requires.is_none());
}

#[test]
fn target_requires_rejects_non_function_target() {
    let mut h = Harness::new();
    let attr = attr_with_args(
        &h.interner,
        "target_requires",
        vec![ident_arg(&h.interner, "Subprocess")],
    );
    let set = validate_attributes(
        &[attr],
        AttrTarget::TypeDecl,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(h.diags.has_errors());
    assert!(set.target_requires.is_none());
}

#[test]
fn target_requires_rejects_non_ident_arg() {
    let mut h = Harness::new();
    let attr = attr_with_args(
        &h.interner,
        "target_requires",
        vec![lit_str(&h.interner, "Subprocess")],
    );
    let set = validate_attributes(
        &[attr],
        AttrTarget::Function,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(h.diags.has_errors());
    assert!(set.target_requires.is_none());
}

// === @abi =============================================================

#[test]
fn abi_valid_values_map_to_enum() {
    for (text, want) in [
        ("c", AttrAbi::C),
        ("system", AttrAbi::System),
        ("sysv64", AttrAbi::SysV64),
        ("win64", AttrAbi::Win64),
    ] {
        let mut h = Harness::new();
        let attr = attr_with_args(&h.interner, "abi", vec![lit_str(&h.interner, text)]);
        let set = validate_attributes(
            &[attr],
            AttrTarget::Function,
            &h.interner,
            &h.lint_cfg,
            &mut h.diags,
        );
        assert!(!h.diags.has_errors(), "ABI {} rejected", text);
        assert_eq!(set.abi, Some(want), "ABI {} mapped wrong", text);
    }
}

#[test]
fn abi_noncatalogue_value_is_symbol_override() {
    // A non-catalogue string on a
    // body-bearing function is a verbatim linker-symbol override.
    let mut h = Harness::new();
    let sym = h.interner.intern("__edda_alloc_raw");
    let attr = attr_with_args(&h.interner, "abi", vec![lit_str(&h.interner, "__edda_alloc_raw")]);
    let set = validate_attributes(
        &[attr],
        AttrTarget::Function,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(!h.diags.has_errors());
    assert!(set.abi.is_none());
    assert_eq!(set.abi_symbol, Some(sym));
}

#[test]
fn abi_rejects_empty_symbol_override() {
    let mut h = Harness::new();
    let attr = attr_with_args(&h.interner, "abi", vec![lit_str(&h.interner, "")]);
    let set = validate_attributes(
        &[attr],
        AttrTarget::Function,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(h.diags.has_errors());
    assert!(set.abi.is_none());
    assert!(set.abi_symbol.is_none());
}

#[test]
fn abi_rejected_on_type_decl() {
    let mut h = Harness::new();
    let attr = attr_with_args(&h.interner, "abi", vec![lit_str(&h.interner, "c")]);
    let set = validate_attributes(
        &[attr],
        AttrTarget::TypeDecl,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(h.diags.has_errors());
    assert!(set.abi.is_none());
}

// === @align ===========================================================

#[test]
fn align_valid_power_of_two() {
    for v in [1u128, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024] {
        let mut h = Harness::new();
        let attr = attr_with_args(&h.interner, "align", vec![lit_int(v)]);
        let set = validate_attributes(
            &[attr],
            AttrTarget::TypeDecl,
            &h.interner,
            &h.lint_cfg,
            &mut h.diags,
        );
        assert!(!h.diags.has_errors(), "@align({}) rejected", v);
        assert_eq!(set.align, Some(v as u32));
    }
}

#[test]
fn align_rejects_zero() {
    let mut h = Harness::new();
    let attr = attr_with_args(&h.interner, "align", vec![lit_int(0)]);
    let set = validate_attributes(
        &[attr],
        AttrTarget::TypeDecl,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(h.diags.has_errors());
    assert!(set.align.is_none());
}

#[test]
fn align_rejects_non_power_of_two() {
    for v in [3u128, 5, 6, 7, 9, 10, 15, 100] {
        let mut h = Harness::new();
        let attr = attr_with_args(&h.interner, "align", vec![lit_int(v)]);
        let set = validate_attributes(
            &[attr],
            AttrTarget::TypeDecl,
            &h.interner,
            &h.lint_cfg,
            &mut h.diags,
        );
        assert!(h.diags.has_errors(), "@align({}) should reject", v);
        assert!(set.align.is_none());
    }
}

#[test]
fn align_rejects_overflow_u32() {
    let mut h = Harness::new();
    // 2^33 is a power of two but exceeds u32::MAX.
    let attr = attr_with_args(&h.interner, "align", vec![lit_int(1u128 << 33)]);
    let set = validate_attributes(
        &[attr],
        AttrTarget::TypeDecl,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(h.diags.has_errors());
    assert!(set.align.is_none());
}

#[test]
fn align_rejected_on_function() {
    let mut h = Harness::new();
    let attr = attr_with_args(&h.interner, "align", vec![lit_int(8)]);
    let set = validate_attributes(
        &[attr],
        AttrTarget::Function,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(h.diags.has_errors());
    assert!(set.align.is_none());
}

#[test]
fn align_rejects_string_arg() {
    let mut h = Harness::new();
    let attr = attr_with_args(&h.interner, "align", vec![lit_str(&h.interner, "8")]);
    let set = validate_attributes(
        &[attr],
        AttrTarget::TypeDecl,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(h.diags.has_errors());
    assert!(set.align.is_none());
}

// === @repr ============================================================

#[test]
fn repr_valid_values_map_to_enum() {
    for (text, want) in [
        ("Edda", AttrRepr::Edda),
        ("C", AttrRepr::C),
        ("Transparent", AttrRepr::Transparent),
        ("Simd", AttrRepr::Simd),
        ("Opaque", AttrRepr::Opaque),
    ] {
        let mut h = Harness::new();
        let attr = attr_with_args(&h.interner, "repr", vec![ident_arg(&h.interner, text)]);
        let set = validate_attributes(
            &[attr],
            AttrTarget::TypeDecl,
            &h.interner,
            &h.lint_cfg,
            &mut h.diags,
        );
        assert!(!h.diags.has_errors(), "@repr({}) rejected", text);
        assert_eq!(set.repr, Some(want));
    }
}

#[test]
fn repr_rejects_unknown_value() {
    let mut h = Harness::new();
    let attr = attr_with_args(&h.interner, "repr", vec![ident_arg(&h.interner, "Packed")]);
    let set = validate_attributes(
        &[attr],
        AttrTarget::TypeDecl,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(h.diags.has_errors());
    assert!(set.repr.is_none());
}

#[test]
fn repr_rejects_string_arg() {
    let mut h = Harness::new();
    let attr = attr_with_args(&h.interner, "repr", vec![lit_str(&h.interner, "C")]);
    let set = validate_attributes(
        &[attr],
        AttrTarget::TypeDecl,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(h.diags.has_errors());
    assert!(set.repr.is_none());
}

#[test]
fn repr_rejected_on_function() {
    let mut h = Harness::new();
    let attr = attr_with_args(&h.interner, "repr", vec![ident_arg(&h.interner, "C")]);
    let set = validate_attributes(
        &[attr],
        AttrTarget::Function,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(h.diags.has_errors());
    assert!(set.repr.is_none());
}

// === @layout ==========================================================

#[test]
fn layout_valid_values_map_to_enum() {
    for (text, want) in [
        ("natural", AttrLayout::Natural),
        ("declared", AttrLayout::Declared),
        ("sorted", AttrLayout::Sorted),
        ("packed", AttrLayout::Packed),
    ] {
        let mut h = Harness::new();
        let attr = attr_with_args(&h.interner, "layout", vec![ident_arg(&h.interner, text)]);
        let set = validate_attributes(
            &[attr],
            AttrTarget::TypeDecl,
            &h.interner,
            &h.lint_cfg,
            &mut h.diags,
        );
        assert!(!h.diags.has_errors(), "@layout({}) rejected", text);
        assert_eq!(set.layout, Some(want));
    }
}

#[test]
fn layout_rejects_unknown_value() {
    let mut h = Harness::new();
    let attr = attr_with_args(
        &h.interner,
        "layout",
        vec![ident_arg(&h.interner, "padded")],
    );
    let set = validate_attributes(
        &[attr],
        AttrTarget::TypeDecl,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(h.diags.has_errors());
    assert!(set.layout.is_none());
}

#[test]
fn layout_case_sensitive() {
    // `Natural` (capitalised) is rejected; the locked spelling is
    // lowercase `natural`.
    let mut h = Harness::new();
    let attr = attr_with_args(
        &h.interner,
        "layout",
        vec![ident_arg(&h.interner, "Natural")],
    );
    let set = validate_attributes(
        &[attr],
        AttrTarget::TypeDecl,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(h.diags.has_errors());
    assert!(set.layout.is_none());
}

// === Unknown name + Other target ======================================

#[test]
fn unknown_attribute_name_rejected() {
    let mut h = Harness::new();
    let attr = attr_with_args(&h.interner, "no_mangle", vec![]);
    let set = validate_attributes(
        &[attr],
        AttrTarget::Function,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(h.diags.has_errors());
    assert!(
        has_class(&h, DiagnosticClass::UnknownAttribute),
        "an attribute outside the closed nine must emit error[unknown_attribute]"
    );
    assert!(set.is_empty());
}

#[test]
fn attribute_on_other_target_rejected() {
    // `@align` is admitted only on type declarations; on any other target it
    // is a target-mismatch (TypecheckError), distinct from unknown_attribute.
    let mut h = Harness::new();
    let attr = attr_with_args(&h.interner, "align", vec![lit_int(8)]);
    let set = validate_attributes(
        &[attr],
        AttrTarget::Other,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(h.diags.has_errors());
    assert!(set.is_empty());
}

// === Combinatorial behaviour ==========================================

#[test]
fn empty_attribute_list_yields_empty_set() {
    let mut h = Harness::new();
    let set = validate_attributes(
        &[],
        AttrTarget::Function,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(!h.diags.has_errors());
    assert!(set.is_empty());
}

#[test]
fn multiple_attributes_compose_into_one_set() {
    let mut h = Harness::new();
    let attrs = vec![
        attr_with_args(&h.interner, "align", vec![lit_int(8)]),
        attr_with_args(&h.interner, "repr", vec![ident_arg(&h.interner, "C")]),
    ];
    let set = validate_attributes(
        &attrs,
        AttrTarget::TypeDecl,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(!h.diags.has_errors());
    assert_eq!(set.align, Some(8));
    assert_eq!(set.repr, Some(AttrRepr::C));
}

#[test]
fn duplicate_attribute_last_one_wins() {
    let mut h = Harness::new();
    let attrs = vec![
        attr_with_args(&h.interner, "abi", vec![lit_str(&h.interner, "c")]),
        attr_with_args(&h.interner, "abi", vec![lit_str(&h.interner, "win64")]),
    ];
    let set = validate_attributes(
        &attrs,
        AttrTarget::Function,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(!h.diags.has_errors());
    assert_eq!(set.abi, Some(AttrAbi::Win64));
}

#[test]
fn one_invalid_attribute_does_not_drop_a_sibling_valid_one() {
    let mut h = Harness::new();
    let attrs = vec![
        attr_with_args(&h.interner, "bogus", vec![]),
        attr_with_args(&h.interner, "abi", vec![lit_str(&h.interner, "sysv64")]),
    ];
    let set = validate_attributes(
        &attrs,
        AttrTarget::Function,
        &h.interner,
        &h.lint_cfg,
        &mut h.diags,
    );
    assert!(h.diags.has_errors(), "expected error on bogus attribute");
    assert!(
        has_class(&h, DiagnosticClass::UnknownAttribute),
        "the bogus attribute must emit unknown_attribute"
    );
    assert_eq!(
        set.abi,
        Some(AttrAbi::SysV64),
        "valid sibling attribute must survive"
    );
}
