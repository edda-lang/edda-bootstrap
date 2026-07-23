//! Pretty-printing of operands, places, rvalues, and interned constants.

use crate::constant::{Const, ConstValue};
use crate::ids::{BlockId, ConstId, LocalId};
use crate::operand::Operand;
use crate::place::{Place, Projection};
use crate::program::MirProgram;
use crate::rvalue::{Rvalue, RvalueKind};

use super::PrettyPrinter;

impl PrettyPrinter<'_> {
    /// Format a [`LocalId`] as `_N` (matches rustc-MIR convention).
    pub(crate) fn format_local(local: LocalId) -> String {
        format!("_{}", local.as_u32())
    }

    /// Format a [`BlockId`] as `bbN`.
    pub(crate) fn format_block(block: BlockId) -> String {
        format!("bb{}", block.as_u32())
    }

    /// Format a place: `_N` plus a projection suffix (`.fK`, `[_M]`,
    /// `as vK`, `.*<ty>`). Takes `&self` so the `Deref` arm can render
    /// the pointed-to leaf type via [`Self::format_type`].
    pub(crate) fn format_place(&self, place: &Place) -> String {
        let mut s = Self::format_local(place.local);
        for p in &place.projection {
            match p {
                Projection::Field(idx) => {
                    s.push('.');
                    s.push('f');
                    s.push_str(&idx.as_u32().to_string());
                }
                Projection::Index(local) => {
                    s.push('[');
                    s.push_str(&Self::format_local(*local));
                    s.push(']');
                }
                Projection::VariantDowncast(variant) => {
                    s.push_str(" as v");
                    s.push_str(&variant.as_u32().to_string());
                }
                Projection::Deref(ty) => {
                    s.push_str(".*");
                    s.push_str(&self.format_type(&ty.kind));
                }
            }
        }
        s
    }

    /// Format an operand using the program's constant arena to dereference
    /// `Operand::Const`.
    pub(crate) fn format_operand(&self, operand: &Operand, program: &MirProgram) -> String {
        match operand {
            Operand::Copy(p) => format!("copy {}", self.format_place(p)),
            Operand::Move(p) => format!("move {}", self.format_place(p)),
            Operand::Const(id) => self.format_const_ref(*id, program),
            Operand::Unit => "unit".to_string(),
        }
    }

    /// Format a constant reference: `const cK = <value>`.
    fn format_const_ref(&self, id: ConstId, program: &MirProgram) -> String {
        match program.consts.get(id) {
            Some(c) => format!("const c{} = {}", id.as_u32(), self.format_const_value(c)),
            None => format!("const c{} = <missing>", id.as_u32()),
        }
    }

    /// Format the value payload of a [`Const`].
    pub(crate) fn format_const_value(&self, c: &Const) -> String {
        match &c.value {
            ConstValue::Int(v) => format!("{}i", v),
            ConstValue::Uint(v) => format!("{}u", v),
            ConstValue::Float(bits) => {
                let f = f64::from_bits(*bits);
                format!("{}f", f)
            }
            ConstValue::Bool(b) => b.to_string(),
            ConstValue::Str(sym) => format!("{:?}", self.resolve(*sym)),
            ConstValue::Unit => "unit".to_string(),
            ConstValue::Zero => "zero".to_string(),
        }
    }

    /// Format a full rvalue expression.
    pub(crate) fn format_rvalue(&self, rvalue: &Rvalue, program: &MirProgram) -> String {
        match &rvalue.kind {
            RvalueKind::Use(op) => self.format_operand(op, program),
            RvalueKind::BinOp { op, lhs, rhs, prim } => format!(
                "{}.{}({}, {})",
                op.mnemonic(),
                prim.as_str(),
                self.format_operand(lhs, program),
                self.format_operand(rhs, program),
            ),
            RvalueKind::UnOp { op, arg, prim } => format!(
                "{}.{}({})",
                op.mnemonic(),
                prim.as_str(),
                self.format_operand(arg, program),
            ),
            RvalueKind::Cast {
                src,
                src_prim,
                dst_prim,
            } => format!(
                "cast({}: {} -> {})",
                self.format_operand(src, program),
                src_prim.as_str(),
                dst_prim.as_str(),
            ),
            RvalueKind::MakeArray { elems } => {
                format!("make_array{}", self.format_operand_list(elems, program))
            }
            RvalueKind::MakeTuple { elems } => {
                format!("make_tuple{}", self.format_operand_list(elems, program))
            }
            RvalueKind::MakeRecord { adt, fields } => format!(
                "make_record(adt{}){}",
                adt.as_u32(),
                self.format_operand_list(fields, program),
            ),
            RvalueKind::MakeVariant {
                adt,
                variant,
                fields,
            } => format!(
                "make_variant(adt{}, v{}){}",
                adt.as_u32(),
                variant.as_u32(),
                self.format_operand_list(fields, program),
            ),
            RvalueKind::ArrayIndex { array, idx } => format!(
                "array_index({}, {})",
                self.format_operand(array, program),
                self.format_operand(idx, program),
            ),
            RvalueKind::SliceSubrange { source, lo, hi } => format!(
                "slice_subrange({}, {}, {})",
                self.format_operand(source, program),
                self.format_operand(lo, program),
                self.format_operand(hi, program),
            ),
            RvalueKind::ArrayLen { array } => {
                format!("array_len({})", self.format_operand(array, program))
            }
            RvalueKind::ExtractField {
                subject,
                variant,
                field,
            } => {
                let v = match variant {
                    Some(idx) => format!(", v{}", idx.as_u32()),
                    None => String::new(),
                };
                format!(
                    "extract_field({}, f{}{})",
                    self.format_operand(subject, program),
                    field.as_u32(),
                    v,
                )
            }
            RvalueKind::ExtractTag { subject } => {
                format!("extract_tag({})", self.format_operand(subject, program))
            }
            RvalueKind::StringBytes(op) => {
                format!("string_bytes({})", self.format_operand(op, program))
            }
            RvalueKind::FunctionRef(body_id) => {
                format!("fn_ref(body{})", body_id.as_u32())
            }
            RvalueKind::MakeClosure { code, env } => format!(
                "make_closure(body{}, {})",
                code.as_u32(),
                self.format_operand(env, program),
            ),
            RvalueKind::Ref { place } => format!("ref {}", self.format_place(place)),
        }
    }

    /// Render `[op0, op1, ...]` — the parenthesised list form used by the
    /// `make_*` rvalues.
    fn format_operand_list(&self, ops: &[Operand], program: &MirProgram) -> String {
        let mut s = String::from("[");
        for (i, op) in ops.iter().enumerate() {
            if i > 0 {
                s.push_str(", ");
            }
            s.push_str(&self.format_operand(op, program));
        }
        s.push(']');
        s
    }
}
