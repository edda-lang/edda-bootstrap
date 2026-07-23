//! Pretty-printing of MIR types and ADT definitions.

use crate::adt::{AdtDef, AdtKind, FieldDef, VariantDef};
use crate::effect::CapabilityKind;
use crate::ids::AdtId;
use crate::ty::{FnSig, MirType, MirTypeKind, ParamMode};

use super::PrettyPrinter;

impl PrettyPrinter<'_> {
    /// Format a [`MirTypeKind`] into a fresh `String` — recursive into nested
    /// types (tuples, slices, function pointers).
    pub(crate) fn format_type(&self, kind: &MirTypeKind) -> String {
        match kind {
            MirTypeKind::Prim(p) => p.as_str().to_string(),
            MirTypeKind::Adt(id) => format_adt_ref(*id),
            MirTypeKind::Tuple(elems) => {
                let mut s = String::from("(");
                for (i, t) in elems.iter().enumerate() {
                    if i > 0 {
                        s.push_str(", ");
                    }
                    s.push_str(&self.format_type(&t.kind));
                }
                s.push(')');
                s
            }
            MirTypeKind::Slice(inner) => {
                let mut s = String::from("[");
                s.push_str(&self.format_type(&inner.kind));
                s.push(']');
                s
            }
            MirTypeKind::Unit => "()".to_string(),
            MirTypeKind::Never => "!".to_string(),
            MirTypeKind::Capability(kind) => self.format_capability(kind),
            MirTypeKind::FnPtr(sig) => self.format_fn_sig(sig),
        }
    }

    /// Lowercase rendering of a capability kind (resolving `Named` via the
    /// interner).
    pub(crate) fn format_capability(&self, kind: &CapabilityKind) -> String {
        if let Some(name) = kind.well_known_str() {
            return format!("cap({})", name);
        }
        match kind {
            CapabilityKind::Named(sym) => format!("cap({})", self.resolve(*sym)),
            _ => "cap(?)".to_string(),
        }
    }

    /// Render a function-pointer signature as `fn(let i32, mutable u64) -> bool`.
    pub(crate) fn format_fn_sig(&self, sig: &FnSig) -> String {
        let mut s = String::from("fn(");
        for (i, (mode, ty)) in sig.params.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(mode.as_str());
            s.push(' ');
            s.push_str(&self.format_type(&ty.kind));
        }
        s.push_str(") -> ");
        s.push_str(&self.format_type(&sig.ret.kind));
        if !sig.capabilities.is_empty() || !sig.may_raise.is_empty() || sig.may_panic {
            s.push_str(" with {");
            self.append_effect_summary(
                &mut s,
                &sig.capabilities,
                &sig.may_raise,
                sig.may_panic,
            );
            s.push('}');
        }
        s
    }

    /// Append `cap; err; panic` summary fragments into `out`.
    fn append_effect_summary(
        &self,
        out: &mut String,
        caps: &[CapabilityKind],
        errs: &[AdtId],
        panic: bool,
    ) {
        let mut first = true;
        for c in caps {
            if !first {
                out.push_str(", ");
            }
            first = false;
            out.push_str(&self.format_capability(c));
        }
        for a in errs {
            if !first {
                out.push_str(", ");
            }
            first = false;
            out.push_str(&format!("err: {}", format_adt_ref(*a)));
        }
        if panic {
            if !first {
                out.push_str(", ");
            }
            out.push_str("panic");
        }
    }

    /// Render one [`AdtDef`] block. Non-default `layout` fields (from
    /// `@align` / `@repr` / `@layout` overrides) appear as annotations
    /// on the header line; the natural default suppresses them.
    pub(crate) fn print_adt(&mut self, id: AdtId, adt: &AdtDef) {
        let header_kind = match adt.kind {
            AdtKind::Product => "product",
            AdtKind::Sum => "sum",
        };
        let mut line = format!(
            "{} {} ({})",
            header_kind,
            self.resolve(adt.name),
            format_adt_ref(id),
        );
        let layout_suffix = format_layout_annotations(&adt.layout);
        if !layout_suffix.is_empty() {
            line.push(' ');
            line.push_str(&layout_suffix);
        }
        line.push_str(" {");
        self.write_line(&line);
        self.with_indent(|p| {
            for (idx, variant) in adt.variants.iter().enumerate() {
                p.print_variant(idx, variant, adt.kind);
            }
        });
        self.write_line("}");
    }

    /// Render one variant within an ADT block.
    fn print_variant(&mut self, idx: usize, variant: &VariantDef, kind: AdtKind) {
        let header = match kind {
            AdtKind::Product => format!("fields {} {{", self.resolve(variant.name)),
            AdtKind::Sum => {
                let disc = variant
                    .discriminant
                    .map(|d| format!(" = {}", d))
                    .unwrap_or_default();
                format!("variant {} (v{}) {} {{", self.resolve(variant.name), idx, disc)
            }
        };
        self.write_line(&header);
        self.with_indent(|p| {
            for (fi, field) in variant.fields.iter().enumerate() {
                p.print_field(fi, field);
            }
        });
        self.write_line("}");
    }

    /// Render one field declaration.
    fn print_field(&mut self, idx: usize, field: &FieldDef) {
        let line = format!(
            "f{}: {} ; // {}",
            idx,
            self.format_type(&field.ty.kind),
            self.resolve(field.name),
        );
        self.write_line(&line);
    }

    /// Render a parameter mode + type as it appears in a function signature.
    pub(crate) fn format_param(&self, mode: ParamMode, ty: &MirType) -> String {
        format!("{} {}", mode.as_str(), self.format_type(&ty.kind))
    }
}

/// Render an `AdtId` as `adt7` — used in inline references inside types.
fn format_adt_ref(id: AdtId) -> String {
    format!("adt{}", id.as_u32())
}

/// Render non-default fields of a [`crate::layout::LayoutInfo`] as the
/// space-separated `repr=C align=16 layout=packed` form attached to an
/// ADT header. Returns an empty string when every field is at its
/// natural default — the printer suppresses the prefix entirely in that
/// case.
fn format_layout_annotations(layout: &crate::layout::LayoutInfo) -> String {
    use crate::layout::{LayoutPolicy, ReprKind};
    let mut parts: Vec<String> = Vec::new();
    if layout.repr != ReprKind::Edda {
        let s = match layout.repr {
            ReprKind::Edda => unreachable!(),
            ReprKind::C => "C",
            ReprKind::Transparent => "Transparent",
            ReprKind::Simd => "Simd",
            ReprKind::Opaque => "Opaque",
        };
        parts.push(format!("repr={}", s));
    }
    if layout.policy != LayoutPolicy::Natural {
        let s = match layout.policy {
            LayoutPolicy::Natural => unreachable!(),
            LayoutPolicy::Declared => "declared",
            LayoutPolicy::Sorted => "sorted",
            LayoutPolicy::Packed => "packed",
        };
        parts.push(format!("layout={}", s));
    }
    if let Some(align) = layout.align {
        parts.push(format!("align={}", align.get()));
    }
    parts.join(" ")
}
