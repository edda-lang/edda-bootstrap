use super::*;
use super::args::type_leaf_mangle;
use crate::argument::{
    Argument, ArgumentTuple, EffectRowArgument, EffectRowEntry, PrimitiveValue, UserField,
    UserValue, UserVariantPayload,
};

fn args(items: Vec<Argument>) -> ArgumentTuple {
    ArgumentTuple::new(items)
}

#[test]
fn leaf_of_takes_last_path_segment() {
    assert_eq!(leaf_of("std.option.Option"), "Option");
    assert_eq!(leaf_of("Option"), "Option");
    assert_eq!(leaf_of("a.b.c.d"), "d");
    assert_eq!(leaf_of(""), "");
}

#[test]
fn spec_with_no_args_mangles_to_its_leaf() {
    let n = mangle_short_name("std.option.Option", &ArgumentTuple::empty());
    assert_eq!(n.as_str(), "Option");
}

#[test]
fn type_argument_uses_leaf_form() {
    let n = mangle_short_name(
        "std.option.Option",
        &args(vec![Argument::Type(SmolStr::new("i32"))]),
    );
    assert_eq!(n.as_str(), "Option_i32");
}

#[test]
fn type_leaf_mangle_strips_module_path_and_joins_type_suffix() {
    // Nested-spec args need their
    // spec-mangled intermediate preserved so two distinct
    // `Vec(Vec_X.Vec)` invocations don't collide on the bare leaf.
    assert_eq!(type_leaf_mangle("i32"), "i32");
    assert_eq!(type_leaf_mangle("Option_i32"), "Option_i32");
    assert_eq!(type_leaf_mangle("std.option.Option"), "Option");
    assert_eq!(type_leaf_mangle("foo.Point"), "Point");
    assert_eq!(
        type_leaf_mangle("std.collections.vec.Vec_String.Vec"),
        "Vec_String_Vec",
    );
    assert_eq!(
        type_leaf_mangle("std.collections.vec.Vec_usize.Vec"),
        "Vec_usize_Vec",
    );
}

#[test]
fn nested_vec_args_do_not_collide_on_mangled_name() {
    // `Vec(Vec_String.Vec)` and
    // `Vec(Vec_usize.Vec)` must produce distinct short names so the
    // generated artifacts ship to different module paths.
    let a = mangle_short_name(
        "std.collections.vec.Vec",
        &args(vec![Argument::Type(SmolStr::new(
            "std.collections.vec.Vec_String.Vec",
        ))]),
    );
    let b = mangle_short_name(
        "std.collections.vec.Vec",
        &args(vec![Argument::Type(SmolStr::new(
            "std.collections.vec.Vec_usize.Vec",
        ))]),
    );
    assert_eq!(a.as_str(), "Vec_Vec_String_Vec");
    assert_eq!(b.as_str(), "Vec_Vec_usize_Vec");
    assert_ne!(a, b);
}

#[test]
fn single_segment_primitive_arg_keeps_leaf_only_form() {
    // A single-segment primitive
    // arg (`Vec(String)`) must produce the leaf-only mangle
    // `Vec_String`. `type_leaf_mangle` is structurally a no-op for
    // single-segment qnames — the loop only strips when `segments.len()
    // > 1` — so `String` flows through verbatim.
    let n = mangle_short_name(
        "std.collections.vec.Vec",
        &args(vec![Argument::Type(SmolStr::new("String"))]),
    );
    assert_eq!(n.as_str(), "Vec_String");
}

#[test]
fn multi_segment_module_qualified_arg_keeps_leaf_only_form() {
    // A multi-segment arg whose
    // leading segments are snake_case Edda module names (the standard
    // convention) must mangle to the leaf-only form `Option_Token`.
    // The strip-lowercase loop in `type_leaf_mangle` is the structural
    // collision-detection mechanism: lowercase module segments are
    // never load-bearing for disambiguation (two args with the same
    // type-leaf but different module paths refer to the same item
    // after resolution), so stripping them yields the canonical
    // leaf-only mangle. This is the "no collision in scope" branch
    // of the issue's fix-shape — there's no other arg in the spec
    // body whose leaf-only mangle collides with `Token`.
    assert_eq!(
        mangle_short_name(
            "std.core.option.Option",
            &args(vec![Argument::Type(SmolStr::new("kind_mod.Token"))]),
        )
        .as_str(),
        "Option_Token",
    );
    assert_eq!(
        mangle_short_name(
            "std.core.option.Option",
            &args(vec![Argument::Type(SmolStr::new(
                "repro.tokens.kind.Token",
            ))]),
        )
        .as_str(),
        "Option_Token",
    );
}

#[test]
fn multi_segment_spec_mangled_arg_path_qualifies() {
    // A multi-segment arg whose
    // intermediate segments are spec-mangled (CamelCase / underscore-
    // bearing) names retains those segments so collision-prone
    // nested-Vec invocations stay distinct. `Vec_String.Vec` and
    // `Vec_usize.Vec` would BOTH collapse to `Vec_Vec` under the
    // old leaf-only rule; the strip-lowercase loop terminates as
    // soon as it sees a non-lowercase head, so `Vec_String` /
    // `Vec_usize` are preserved and the two mangle to distinct names.
    // This is the "collision in scope" branch of the issue's fix-shape.
    let a = mangle_short_name(
        "std.collections.vec.Vec",
        &args(vec![Argument::Type(SmolStr::new(
            "std.collections.vec.Vec_String.Vec",
        ))]),
    );
    let b = mangle_short_name(
        "std.collections.vec.Vec",
        &args(vec![Argument::Type(SmolStr::new(
            "std.collections.vec.Vec_usize.Vec",
        ))]),
    );
    assert_ne!(a, b, "nested-Vec invocations must produce distinct names");
    assert_eq!(a.as_str(), "Vec_Vec_String_Vec");
    assert_eq!(b.as_str(), "Vec_Vec_usize_Vec");
}

#[test]
fn type_argument_is_recursive_for_generated_types() {
    // §5 worked example: Stack(Option_i32) → Stack_Option_i32.
    // The argument's qualified name is already the recursively-
    // mangled form ("Option_i32") — we just take its leaf.
    let n = mangle_short_name(
        "std.stack.Stack",
        &args(vec![Argument::Type(SmolStr::new("Option_i32"))]),
    );
    assert_eq!(n.as_str(), "Stack_Option_i32");
}

#[test]
fn multiple_type_arguments_chain() {
    let n = mangle_short_name(
        "parser.Pair",
        &args(vec![
            Argument::Type(SmolStr::new("String")),
            Argument::Type(SmolStr::new("JsonValue")),
        ]),
    );
    assert_eq!(n.as_str(), "Pair_String_JsonValue");
}

#[test]
fn primitive_integer_mangles_to_decimal() {
    let n = mangle_short_name(
        "S",
        &args(vec![Argument::Primitive(PrimitiveValue::U32(42))]),
    );
    assert_eq!(n.as_str(), "S_42");
}

#[test]
fn primitive_signed_negative_uses_minus_prefix() {
    let n = mangle_short_name(
        "S",
        &args(vec![Argument::Primitive(PrimitiveValue::I32(-5))]),
    );
    assert_eq!(n.as_str(), "S_-5");
}

#[test]
fn primitive_bool_mangles_to_keyword() {
    let t = mangle_short_name(
        "S",
        &args(vec![Argument::Primitive(PrimitiveValue::Bool(true))]),
    );
    let f = mangle_short_name(
        "S",
        &args(vec![Argument::Primitive(PrimitiveValue::Bool(false))]),
    );
    assert_eq!(t.as_str(), "S_true");
    assert_eq!(f.as_str(), "S_false");
}

#[test]
fn primitive_safe_string_inlines_verbatim() {
    let n = mangle_short_name(
        "S",
        &args(vec![Argument::Primitive(PrimitiveValue::String(
            SmolStr::new("hello_world"),
        ))]),
    );
    assert_eq!(n.as_str(), "S_hello_world");
}

#[test]
fn primitive_unsafe_string_uses_placeholder() {
    let n = mangle_short_name(
        "S",
        &args(vec![Argument::Primitive(PrimitiveValue::String(
            SmolStr::new("has space"),
        ))]),
    );
    assert_eq!(n.as_str(), "S_string");
}

#[test]
fn effect_row_mangles_per_storage_md_worked_example() {
    // §6 worked example:
    //   Input row: {yield: i32, err: ParseError, err: AllocError, panic}
    //   Canonical: err: AllocError, err: ParseError, panic, yield: i32
    //   Mangle:   with_err_AllocError_err_ParseError_panic_yield_i32
    let row = EffectRowArgument::new([
        EffectRowEntry::Yield(SmolStr::new("i32")),
        EffectRowEntry::Err(SmolStr::new("ParseError")),
        EffectRowEntry::Err(SmolStr::new("AllocError")),
        EffectRowEntry::Panic,
    ]);
    let n = mangle_short_name(
        "parser.Pipeline",
        &args(vec![Argument::EffectRow(row)]),
    );
    assert_eq!(
        n.as_str(),
        "Pipeline_with_err_AllocError_err_ParseError_panic_yield_i32",
    );
}

#[test]
fn empty_effect_row_mangles_to_with() {
    let n = mangle_short_name(
        "S",
        &args(vec![Argument::EffectRow(EffectRowArgument::empty())]),
    );
    assert_eq!(n.as_str(), "S_with");
}

#[test]
fn user_defined_uses_type_leaf_only() {
    // §5: mangled-value ::= type-leaf-name. Field contents are
    // NOT part of the short name; the hash prefix disambiguates.
    let v = UserValue::Record {
        type_qualified: SmolStr::new("foo.Point"),
        fields: Box::new([UserField::new(
            "x",
            Argument::Primitive(PrimitiveValue::I32(1)),
        )]),
    };
    let n = mangle_short_name("S", &args(vec![Argument::UserDefined(v)]));
    assert_eq!(n.as_str(), "S_Point");
}

#[test]
fn user_defined_different_field_values_share_mangled_form() {
    // The mangled form ignores user-value contents; only the
    // type leaf appears. Both values collide here — the artifact
    // hash prefix is the actual disambiguator.
    let mk = |v: i32| {
        args(vec![Argument::UserDefined(UserValue::Record {
            type_qualified: SmolStr::new("foo.Point"),
            fields: Box::new([UserField::new(
                "v",
                Argument::Primitive(PrimitiveValue::I32(v)),
            )]),
        })])
    };
    let a = mangle_short_name("S", &mk(1));
    let b = mangle_short_name("S", &mk(2));
    assert_eq!(a, b);
    assert_eq!(a.as_str(), "S_Point");
}

#[test]
fn user_defined_variant_uses_type_leaf_too() {
    let v = UserValue::Variant {
        type_qualified: SmolStr::new("foo.Color"),
        variant_tag: SmolStr::new("rgb"),
        payload: UserVariantPayload::Unit,
    };
    let n = mangle_short_name("S", &args(vec![Argument::UserDefined(v)]));
    assert_eq!(n.as_str(), "S_Color");
}

#[test]
fn artifact_name_composes_short_and_hash_prefix() {
    let hash = ArtifactHash::from_bytes([0xab; 32]);
    let name = artifact_name(
        "std.option.Option",
        &args(vec![Argument::Type(SmolStr::new("i32"))]),
        &hash,
    )
    .expect("safe mangled form should compose");
    assert_eq!(name.short(), "Option_i32");
    // 0xab repeated → first 12 hex chars = "abababababab".
    assert_eq!(name.to_string(), "Option_i32__abababababab");
}
