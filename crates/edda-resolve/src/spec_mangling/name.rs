//! CA1-pass-1 short-name mangling for `spec Path(args)` invocations.

use edda_intern::{Interner, Symbol};
use edda_syntax::ast::{Expr, ExprKind, Literal, SpecInvocation};

/// Syntactic CA1-pass-1 mangler for a `spec Path(args)` invocation.
///
/// Mirrors the `<short-mangled-name>` rule from `docs/codegen/storage.md`
/// §5 over the raw AST: the spec path's last segment, then one `_<leaf>`
/// suffix per argument whose `ExprKind::Path` last segment is the leaf
/// name, or one `_<decimal>`/`_true`/`_false`/`_<safe-id>`/`_string`
/// suffix per integer / bool / string literal argument. Nested-call
/// argument shapes are not yet admitted.
pub fn mangle_spec_invocation_name(si: &SpecInvocation, interner: &Interner) -> Option<Symbol> {
    let spec_leaf = si.path.segments.last()?.name;
    if spec_leaf == Symbol::DUMMY {
        return None;
    }
    let mut out = String::with_capacity(32);
    out.push_str(interner.resolve(spec_leaf));
    for arg in &si.args {
        out.push('_');
        let leaf = arg_leaf_name(arg, interner)?;
        out.push_str(&leaf);
    }
    Some(interner.intern(&out))
}

pub(super) fn arg_leaf_name(expr: &Expr, interner: &Interner) -> Option<String> {
    match &expr.kind {
        ExprKind::Path(p) => {
            let mut parts: Vec<&str> = Vec::with_capacity(p.segments.len());
            for seg in &p.segments {
                if seg.name == Symbol::DUMMY {
                    return None;
                }
                parts.push(interner.resolve(seg.name));
            }
            if parts.is_empty() {
                return None;
            }
            while parts.len() > 1 {
                let head = parts[0];
                match head.chars().next() {
                    Some(c) if c.is_ascii_lowercase() => {
                        parts.remove(0);
                    }
                    _ => break,
                }
            }
            Some(parts.join("_"))
        }
        ExprKind::Literal(lit) => mangle_literal(lit, interner),
        _ => None,
    }
}

pub(super) fn mangle_literal(lit: &Literal, interner: &Interner) -> Option<String> {
    match lit {
        Literal::Int { value, .. } => Some(value.to_string()),
        Literal::Bool(b) => Some(if *b { "true".to_string() } else { "false".to_string() }),
        Literal::Str(sym) => {
            if *sym == Symbol::DUMMY {
                return None;
            }
            let text = interner.resolve(*sym);
            if !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                Some(text.to_string())
            } else {
                Some("string".to_string())
            }
        }
        // Float / FString and other literal forms are not yet admitted as
        // spec-invocation arguments; the codegen `expr_to_argument` lowering
        // rejects them with a typed diagnostic at the same site.
        _ => None,
    }
}
