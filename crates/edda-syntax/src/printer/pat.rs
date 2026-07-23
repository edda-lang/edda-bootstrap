//! Pattern pretty-printer.

use super::Printer;
use crate::ast::{Pat, PatKind, Path, RangeKind, VariantPatPayload};

impl<'a> Printer<'a> {
    /// Print a pattern in **permissive** mode — `Binding` patterns emit as
    /// bare ident because the surrounding `let`/`var`/`uninit`/`for`
    /// keyword already supplies the binding marker. Mirrors
    /// [`crate::parser::Parser::parse_pat`].
    pub(crate) fn print_pat(&mut self, p: &Pat) {
        self.print_pat_impl(p, false);
    }

    /// Print a pattern in **strict** mode — `Binding` patterns emit as
    /// `let <name>` because match-arm pattern position rejects bare ident.
    /// Mirrors [`crate::parser::Parser::parse_match_pat`].
    pub(crate) fn print_match_pat(&mut self, p: &Pat) {
        self.print_pat_impl(p, true);
    }

    fn print_pat_impl(&mut self, p: &Pat, strict: bool) {
        match &p.kind {
            PatKind::Wildcard => self.write("_"),
            PatKind::Binding(name) => {
                if strict {
                    self.write("let ");
                }
                self.write_resolved(name.name);
            }
            PatKind::Literal(l) => self.print_literal(l),
            PatKind::Tuple(elems) => {
                self.write("(");
                self.comma_separated(elems, |p, e| p.print_pat_impl(e, strict));
                self.write(")");
            }
            PatKind::Variant { path, payload } => {
                self.print_variant_pat_head(path);
                match payload {
                    VariantPatPayload::None => {}
                    VariantPatPayload::Tuple(pats) => {
                        self.write("(");
                        self.comma_separated(pats, |p, e| p.print_pat_impl(e, strict));
                        self.write(")");
                    }
                    VariantPatPayload::Struct(fields) => {
                        self.write(" {");
                        self.with_indent(|p| {
                            for (i, f) in fields.iter().enumerate() {
                                if i > 0 {
                                    p.write(",");
                                }
                                p.write_newline();
                                p.write_resolved(f.name.name);
                                p.write(": ");
                                p.print_pat_impl(&f.pat, strict);
                            }
                        });
                        self.write_newline();
                        self.write("}");
                    }
                }
            }
            PatKind::Struct {
                path,
                fields,
                rest,
            } => {
                self.print_path(path);
                self.write(" {");
                self.with_indent(|p| {
                    for (i, f) in fields.iter().enumerate() {
                        if i > 0 {
                            p.write(",");
                        }
                        p.write_newline();
                        p.write_resolved(f.name.name);
                        p.write(": ");
                        p.print_pat_impl(&f.pat, strict);
                    }
                    if *rest {
                        if !fields.is_empty() {
                            p.write(",");
                        }
                        p.write_newline();
                        p.write("..");
                    }
                });
                self.write_newline();
                self.write("}");
            }
            PatKind::Guard { pat, cond } => {
                self.print_pat_impl(pat, strict);
                self.write(" where ");
                self.print_expr(cond);
            }
            PatKind::Range { lo, hi, kind } => {
                self.print_literal(lo);
                self.write(match kind {
                    RangeKind::HalfOpen => "..<",
                    RangeKind::Closed => "..=",
                });
                self.print_literal(hi);
            }
            PatKind::AtBinding { name, inner } => {
                self.write_resolved(name.name);
                self.write(" @ ");
                self.print_pat_impl(inner, strict);
            }
            PatKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                self.write("[");
                let mut first = true;
                for p in prefix {
                    if !first {
                        self.write(", ");
                    }
                    first = false;
                    self.print_pat_impl(p, strict);
                }
                if let Some(binding) = rest {
                    if !first {
                        self.write(", ");
                    }
                    first = false;
                    self.write("..");
                    if let Some(name) = binding {
                        self.write_resolved(name.name);
                    }
                }
                for p in suffix {
                    if !first {
                        self.write(", ");
                    }
                    first = false;
                    self.print_pat_impl(p, strict);
                }
                self.write("]");
            }
            PatKind::Error => self.write("_ /* parse error */"),
        }
    }

    /// Emit the head of a variant pattern: `.<name>` for single-segment
    /// (leading-dot shorthand) or `Type.<name>` for multi-segment.
    fn print_variant_pat_head(&mut self, path: &Path) {
        if path.segments.len() == 1 {
            self.write(".");
            self.write_resolved(path.segments[0].name);
        } else {
            self.print_path(path);
        }
    }
}
