//! Module-path composition and human-readable invocation formatting.
//!
//! Split out from `instantiate/mod.rs` for file-size reasons.
//! [`compose_module_path`] builds the canonical module path the
//! generated artifact declares (carrying the module-path disambig suffix);
//! [`format_invocation`] renders the display string for the artifact
//! header. The header string is inspectable only — never hashed.

use std::fmt::Write;

use crate::argument::{Argument, ArgumentTuple, PrimitiveValue};
use crate::mangle::{mangle_short_name, module_disambig_hex};

/// Compose the canonical module path the generated artifact declares.
///
/// `spec_qualified` is the fully qualified spec name
/// (`"std.alloc.Box"`); `args` is the comptime argument tuple. The
/// returned path is `<parent>.<mangled_short>_<8hex>` — e.g.
/// `"std.alloc.Box_Expr_a1e605e9"` for `Box(Expr)` — or just
/// `<mangled_short>_<8hex>` when the spec is top-level (no dot in
/// `spec_qualified`). The 8-hex suffix is dropped when
/// [`module_disambig_hex`] cannot canonicalise the args.
pub(super) fn compose_module_path(spec_qualified: &str, args: &ArgumentTuple) -> String {
    let short = mangle_short_name(spec_qualified, args);
    let leaf = match module_disambig_hex(spec_qualified, args) {
        Some(hex) => format!("{short}_{hex}"),
        None => short.to_string(),
    };
    match spec_qualified.rfind('.') {
        Some(idx) => {
            let parent = &spec_qualified[..idx];
            let mut out = String::with_capacity(parent.len() + 1 + leaf.len());
            out.push_str(parent);
            out.push('.');
            out.push_str(&leaf);
            out
        }
        None => leaf,
    }
}

/// Format a human-readable spec invocation string for the artifact header.
///
/// Produces the form `spec_qualified(arg0, arg1, …)`, e.g.
/// `"std.option.Option(i32)"`.
pub(super) fn format_invocation(spec_qualified: &str, args: &ArgumentTuple) -> String {
    let mut out = String::with_capacity(spec_qualified.len() + 16);
    out.push_str(spec_qualified);
    out.push('(');
    for (i, arg) in args.args().iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        format_arg_into(arg, &mut out);
    }
    out.push(')');
    out
}

/// Append one argument's display form to `out`.
fn format_arg_into(arg: &Argument, out: &mut String) {
    match arg {
        Argument::Type(name) => out.push_str(name.as_str()),
        Argument::Function(name) => out.push_str(name.as_str()),
        Argument::Primitive(pv) => format_primitive_into(pv, out),
        Argument::EffectRow(_) => out.push_str("<effect-row>"),
        Argument::UserDefined(_) => out.push_str("<user-defined>"),
    }
}

/// Append a primitive value's display form to `out`.
///
/// `String::write_fmt` is infallible; the `expect` calls are unreachable.
fn format_primitive_into(pv: &PrimitiveValue, out: &mut String) {
    match pv {
        PrimitiveValue::U8(v)    => write!(out, "{v}").expect("infallible"),
        PrimitiveValue::U16(v)   => write!(out, "{v}").expect("infallible"),
        PrimitiveValue::U32(v)   => write!(out, "{v}").expect("infallible"),
        PrimitiveValue::U64(v)   => write!(out, "{v}").expect("infallible"),
        PrimitiveValue::USize(v) => write!(out, "{v}").expect("infallible"),
        PrimitiveValue::I8(v)    => write!(out, "{v}").expect("infallible"),
        PrimitiveValue::I16(v)   => write!(out, "{v}").expect("infallible"),
        PrimitiveValue::I32(v)   => write!(out, "{v}").expect("infallible"),
        PrimitiveValue::I64(v)   => write!(out, "{v}").expect("infallible"),
        PrimitiveValue::ISize(v) => write!(out, "{v}").expect("infallible"),
        PrimitiveValue::Bool(v)  => out.push_str(if *v { "true" } else { "false" }),
        PrimitiveValue::String(v) => write!(out, "{:?}", v.as_str()).expect("infallible"),
    }
}
