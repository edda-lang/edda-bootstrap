//! `derive debug` structural-formatter synthesis.
//!
//! `derive debug for T` desugars (in [`crate::codegen::collect_derive`]) to a
//! `spec std.core.fmt.debug(T)` invocation whose materialised module exposes a
//! `function format(v: T) -> String`. The stdlib spec body is a single-`?`-byte
//! placeholder because a *generic* structural formatter cannot be written in
//! V1.0 (comptime field-introspection + code-emission are post-V1.0). This
//! module synthesises a *concrete* formatter per target type from the
//! typechecker's [`TypeDeclInfo`], emitted as Edda source, parsed back into a
//! [`Spec`], and handed to `instantiate_spec` in place of the placeholder —
//! exactly the route [`crate::derive_eq`] takes for the structural comparator.
//!
//! # Why the synthesised formatter is pure (no `allocator` / `err` row)
//!
//! Each field / payload is rendered with a single-slot f-string `f"{v.<field>}"`
//! and the structural text (`"Point { x: "`, `", y: "`, `" }"`) is folded in
//! with the pure `std.core.fmt.string_concat` extern. The typechecker attributes
//! **no** effect to an f-string interpolation (see `edda_types::infer::synth_fstring`)
//! — the per-slot format call is purely a MIR-lowering concern. Primitive slots
//! lower to the leaking `__edda_format_<T>` externs and aggregate slots dispatch
//! to the nested `debug_<FieldT>.format(v.<field>)` formatter through the
//! driver-built `debug_formatters` map (the f-string fold; mirror of the
//! `eq_comparators` dispatch). None of that path needs the allocator, so the
//! synthesised `format(v: T) -> String` carries no effect row. The allocations
//! leak, matching every other bootstrap formatter (`edda-rt`'s `alloc_edstr`
//! family has no reclaim path yet).
//!
//! Why source-text → parse rather than hand-built AST: same reason as
//! `derive_eq` — the `match` (sum) bodies are far less error-prone to
//! synthesise as text, and parsing guarantees a round-trippable AST.

use std::path::PathBuf;

use edda_diag::{Diagnostics, LintConfig};
use edda_intern::Interner;
use edda_span::SourceMap;
use edda_syntax::ast::{ItemKind, Spec};
use edda_types::{FieldInfo, TypeDeclInfo, TypeDeclShape, VariantInfo, VariantPayloadInfo};

/// Synthesise the `std.core.fmt.debug(T)` formatter [`Spec`] for the
/// target type described by `target_info`. `type_name` is the target's
/// leaf name, used as the struct-header text (`Point { ... }`). Returns
/// `None` when the synthesised source fails to parse (the caller falls
/// back to the placeholder spec).
pub(crate) fn synthesize_debug_spec(
    target_info: &TypeDeclInfo,
    type_name: &str,
    interner: &Interner,
) -> Option<Spec> {
    let body = match &target_info.kind {
        TypeDeclShape::Product { fields } => record_debug_body(type_name, fields, interner),
        TypeDeclShape::Sum { variants } => sum_debug_body(variants, interner),
    };
    let src = format!(
        "module std.core.fmt\n\
         public spec debug(comptime T: Type) {{\n\
         \x20   public function format(v: T) -> String {{\n\
         \x20       {body}\n\
         \x20   }}\n\
         }}\n"
    );
    parse_spec(&src, interner)
}

/// Build the `return <expr>` body line for a record formatter, rendering
/// `Name { f0: <f0>, f1: <f1> }`.
fn record_debug_body(type_name: &str, fields: &[FieldInfo], interner: &Interner) -> String {
    if fields.is_empty() {
        return format!("return {}", string_literal(type_name));
    }
    let mut pieces: Vec<String> = Vec::with_capacity(fields.len() * 2 + 1);
    for (i, field) in fields.iter().enumerate() {
        let name = interner.resolve(field.name);
        if i == 0 {
            pieces.push(struct_header_literal(type_name, name));
        } else {
            pieces.push(field_separator_literal(name));
        }
        pieces.push(slot_fstring(&format!("v.{name}")));
    }
    pieces.push(string_literal(" }"));
    format!("return {}", concat_fold(&pieces))
}

/// Build the `return match v { ... }` body for a sum formatter.
fn sum_debug_body(variants: &[VariantInfo], interner: &Interner) -> String {
    if variants.is_empty() {
        return format!("return {}", string_literal(""));
    }
    let mut arms: Vec<String> = Vec::with_capacity(variants.len());
    for variant in variants {
        let vname = interner.resolve(variant.name);
        let arity = payload_arity(&variant.payload);
        let pat = variant_pattern(vname, arity);
        let value = if arity == 0 {
            string_literal(&format!(".{vname}"))
        } else {
            let mut pieces: Vec<String> = Vec::with_capacity(arity * 2 + 1);
            pieces.push(string_literal(&format!(".{vname}(")));
            for i in 0..arity {
                if i > 0 {
                    pieces.push(string_literal(", "));
                }
                pieces.push(slot_fstring(&format!("a{i}")));
            }
            pieces.push(string_literal(")"));
            concat_fold(&pieces)
        };
        arms.push(format!("            case {pat} => {value}"));
    }
    format!("return match v {{\n{}\n        }}", arms.join("\n"))
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
/// `.<name>(let a0, let a1, ...)` binding each payload slot positionally
/// (named payloads bind positionally too, per the locked variant-pattern
/// grammar).
fn variant_pattern(name: &str, arity: usize) -> String {
    if arity == 0 {
        return format!(".{name}");
    }
    let binders: Vec<String> = (0..arity).map(|i| format!("let a{i}")).collect();
    format!(".{name}({})", binders.join(", "))
}

/// Fold a list of `String`-typed expression-source pieces into a single
/// expression via nested `string_concat` calls.
fn concat_fold(pieces: &[String]) -> String {
    match pieces {
        [] => string_literal(""),
        [only] => only.clone(),
        [first, rest @ ..] => {
            let mut acc = first.clone();
            for piece in rest {
                acc = format!("string_concat({acc}, {piece})");
            }
            acc
        }
    }
}

/// Render a plain Edda string-literal expression-source: `"<text>"`.
/// `text` is composed only of identifiers and structural punctuation
/// (`{` / `}` / `:` / `,` / `.` / `(` / `)` / spaces), none of which
/// require escaping inside a double-quoted literal.
fn string_literal(text: &str) -> String {
    let mut s = String::with_capacity(text.len() + 2);
    s.push('"');
    s.push_str(text);
    s.push('"');
    s
}

/// Render the struct-header literal `"<type> { <field>: "`.
fn struct_header_literal(type_name: &str, field: &str) -> String {
    let mut text = String::new();
    text.push_str(type_name);
    text.push_str(" { ");
    text.push_str(field);
    text.push_str(": ");
    string_literal(&text)
}

/// Render the inter-field separator literal `", <field>: "`.
fn field_separator_literal(field: &str) -> String {
    let mut text = String::new();
    text.push_str(", ");
    text.push_str(field);
    text.push_str(": ");
    string_literal(&text)
}

/// Render a single-slot f-string `f"{<expr>}"` — the MIR f-string fold
/// selects the per-type format call for the slot's value.
fn slot_fstring(expr: &str) -> String {
    let mut s = String::with_capacity(expr.len() + 4);
    s.push_str("f\"{");
    s.push_str(expr);
    s.push_str("}\"");
    s
}

/// Lex + parse `src` and extract the first `spec` declaration. `src` is a
/// complete synthesised module containing exactly one `spec debug(...)`.
fn parse_spec(src: &str, interner: &Interner) -> Option<Spec> {
    let source_map = SourceMap::new();
    let file_id = source_map.add_file(PathBuf::from("<derive-debug-synth>"), src.to_string());
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
