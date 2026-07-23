//! `derive eq` structural-comparator synthesis.
//!
//! `derive eq for T` desugars (in [`crate::codegen::collect_derive`]) to a
//! `spec std.core.compare.eq(T)` invocation whose materialised module
//! exposes a `function eq(a: T, b: T) -> bool`. The stdlib spec body is a
//! `return false` placeholder because a *generic* structural comparator
//! cannot be written in V1.0 (comptime field-introspection + code-emission
//! are post-V1.0). This module synthesises a *concrete* comparator per
//! target type from the typechecker's [`TypeDeclInfo`], emitted as Edda
//! source, parsed back into a [`Spec`], and handed to `instantiate_spec`
//! in place of the placeholder. The materialised comparator then resolves,
//! type-checks, lowers, and runs like any other generated module.
//!
//! Why source-text → parse rather than hand-built AST: the sum (match) and
//! slice (loop) bodies in later milestones are far less error-prone to
//! synthesise as text, and parsing guarantees a round-trippable AST.

use std::path::PathBuf;

use edda_diag::{Diagnostics, LintConfig};
use edda_intern::Interner;
use edda_span::SourceMap;
use edda_syntax::ast::{ItemKind, Spec};
use edda_types::{FieldInfo, TypeDeclInfo, TypeDeclShape, VariantInfo, VariantPayloadInfo};

/// Synthesise the `std.core.compare.eq(T)` comparator [`Spec`] for the
/// target type described by `target_info`. Returns `None` for shapes not
/// yet handled (the caller falls back to the placeholder spec).
pub(crate) fn synthesize_eq_spec(
    target_info: &TypeDeclInfo,
    interner: &Interner,
) -> Option<Spec> {
    let body = match &target_info.kind {
        TypeDeclShape::Product { fields } => record_compare_body(fields, interner),
        TypeDeclShape::Sum { variants } => sum_compare_body(variants, interner),
    };
    let src = format!(
        "module std.core.compare\n\
         public spec eq(comptime T: Type) {{\n\
         \x20   public function eq(a: T, b: T) -> bool {{\n\
         \x20       {body}\n\
         \x20   }}\n\
         }}\n"
    );
    parse_spec(&src, interner)
}

/// Build the `return <expr>` body line for a record comparator.
fn record_compare_body(fields: &[FieldInfo], interner: &Interner) -> String {
    if fields.is_empty() {
        return "return true".to_string();
    }
    let terms: Vec<String> = fields
        .iter()
        .map(|field| {
            let name = interner.resolve(field.name);
            format!("a.{name} == b.{name}")
        })
        .collect();
    format!("return {}", terms.join(" && "))
}

/// Build the `return match a { ... }` body for a sum comparator.
fn sum_compare_body(variants: &[VariantInfo], interner: &Interner) -> String {
    if variants.is_empty() {
        return "return true".to_string();
    }
    let mut arms: Vec<String> = Vec::with_capacity(variants.len());
    for variant in variants {
        let vname = interner.resolve(variant.name);
        let arity = payload_arity(&variant.payload);
        let a_pat = variant_pattern(vname, arity, 'a');
        let b_pat = variant_pattern(vname, arity, 'b');
        let payload_eq = if arity == 0 {
            "true".to_string()
        } else {
            (0..arity)
                .map(|i| format!("a{i} == b{i}"))
                .collect::<Vec<_>>()
                .join(" && ")
        };
        arms.push(format!(
            "            case {a_pat} => match b {{\n                case {b_pat} => {payload_eq}\n                case _ => false\n            }}"
        ));
    }
    format!("return match a {{\n{}\n        }}", arms.join("\n"))
}

/// Number of payload slots a variant carries (0 for unit; the element /
/// field count for tuple / struct payloads).
fn payload_arity(payload: &VariantPayloadInfo) -> usize {
    match payload {
        VariantPayloadInfo::Unit => 0,
        VariantPayloadInfo::Tuple { elems } => elems.len(),
        VariantPayloadInfo::Struct { fields } => fields.len(),
    }
}

/// Build a variant match pattern: `.<name>` for a unit payload, else
/// `.<name>(let <prefix>0, let <prefix>1, ...)` binding each payload slot
/// positionally (named payloads bind positionally too, per the locked
/// variant-pattern grammar).
fn variant_pattern(name: &str, arity: usize, prefix: char) -> String {
    if arity == 0 {
        return format!(".{name}");
    }
    let binders: Vec<String> = (0..arity).map(|i| format!("let {prefix}{i}")).collect();
    format!(".{name}({})", binders.join(", "))
}

/// Lex + parse `src` and extract the first `spec` declaration. `src` is a
/// complete synthesised module containing exactly one `spec eq(...)`.
fn parse_spec(src: &str, interner: &Interner) -> Option<Spec> {
    let source_map = SourceMap::new();
    let file_id = source_map.add_file(PathBuf::from("<derive-eq-synth>"), src.to_string());
    let mut diags = Diagnostics::new();
    let lint_cfg = LintConfig::new();
    let tokens = edda_syntax::lex(src, file_id, interner, &mut diags, &lint_cfg);
    let file = edda_syntax::parse_file(&tokens, interner, &mut diags, &lint_cfg);
    if diags.has_errors() {
        return None;
    }
    file.items.into_iter().find_map(|item| match item.kind {
        ItemKind::Spec(spec) => Some(*spec),
        _ => None,
    })
}
