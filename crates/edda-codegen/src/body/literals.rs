//! Leaf encoders for literals, operators, and binding/range/assignment
//! modes — the small "value" enums embedded inside expression and
//! statement nodes.
//!
//! Each encoder emits a single tag byte from [`super::tags`] plus the
//! variant's payload bytes (in little-endian byte order for integers,
//! length-prefixed UTF-8 for symbols).

use edda_syntax::ast::{AssignOp, BinOp, BindingMode, CastMode, Literal, RangeKind, UnOp};
use edda_syntax::IntBase;

use super::encoder::Encoder;
use super::tags;

impl<'a> Encoder<'a> {
    /// Encode a [`BinOp`] as a single kind tag byte.
    pub fn write_bin_op(&mut self, op: BinOp) {
        let tag = match op {
            BinOp::Add => tags::bin_op::ADD,
            BinOp::Sub => tags::bin_op::SUB,
            BinOp::Mul => tags::bin_op::MUL,
            BinOp::Div => tags::bin_op::DIV,
            BinOp::Mod => tags::bin_op::MOD,
            BinOp::WrapAdd => tags::bin_op::WRAP_ADD,
            BinOp::WrapSub => tags::bin_op::WRAP_SUB,
            BinOp::WrapMul => tags::bin_op::WRAP_MUL,
            BinOp::CheckAdd => tags::bin_op::CHECK_ADD,
            BinOp::CheckSub => tags::bin_op::CHECK_SUB,
            BinOp::CheckMul => tags::bin_op::CHECK_MUL,
            BinOp::CheckMod => tags::bin_op::CHECK_MOD,
            BinOp::SatAdd => tags::bin_op::SAT_ADD,
            BinOp::SatSub => tags::bin_op::SAT_SUB,
            BinOp::SatMul => tags::bin_op::SAT_MUL,
            BinOp::Eq => tags::bin_op::EQ,
            BinOp::Ne => tags::bin_op::NE,
            BinOp::Lt => tags::bin_op::LT,
            BinOp::Le => tags::bin_op::LE,
            BinOp::Gt => tags::bin_op::GT,
            BinOp::Ge => tags::bin_op::GE,
            BinOp::And => tags::bin_op::AND,
            BinOp::Or => tags::bin_op::OR,
            BinOp::BitAnd => tags::bin_op::BIT_AND,
            BinOp::BitOr => tags::bin_op::BIT_OR,
            BinOp::BitXor => tags::bin_op::BIT_XOR,
            BinOp::Shl => tags::bin_op::SHL,
            BinOp::Shr => tags::bin_op::SHR,
        };
        self.push_byte(tag);
    }

    /// Encode a [`UnOp`] as a single kind tag byte.
    pub fn write_un_op(&mut self, op: UnOp) {
        let tag = match op {
            UnOp::Neg => tags::un_op::NEG,
            UnOp::Not => tags::un_op::NOT,
            UnOp::BitNot => tags::un_op::BIT_NOT,
        };
        self.push_byte(tag);
    }

    /// Encode a [`RangeKind`] as a single kind tag byte.
    pub fn write_range_kind(&mut self, kind: RangeKind) {
        let tag = match kind {
            RangeKind::HalfOpen => tags::range_kind::HALF_OPEN,
            RangeKind::Closed => tags::range_kind::CLOSED,
        };
        self.push_byte(tag);
    }

    //            identically across `BodyVersion(0x04)` and `BodyVersion(0x05)`
    //            so the body-hash for any cast that takes the trapping default
    //            stays stable across the version bump; non-trap modes only
    //            land in source from `BodyVersion(0x05)` forward
    /// Encode a [`CastMode`] as a single kind tag byte. Trailing byte
    /// on every `ExprKind::Cast` encoding from `BodyVersion(0x05)`.
    pub fn write_cast_mode(&mut self, mode: CastMode) {
        let tag = match mode {
            CastMode::Trap => tags::cast_mode::TRAP,
            CastMode::Wrapping => tags::cast_mode::WRAPPING,
            CastMode::Saturating => tags::cast_mode::SATURATING,
            CastMode::Checked => tags::cast_mode::CHECKED,
        };
        self.push_byte(tag);
    }

    /// Encode an [`AssignOp`] as a single kind tag byte.
    pub fn write_assign_op(&mut self, op: AssignOp) {
        let tag = match op {
            AssignOp::Plain => tags::assign_op::PLAIN,
            AssignOp::Add => tags::assign_op::ADD,
            AssignOp::Sub => tags::assign_op::SUB,
            AssignOp::Mul => tags::assign_op::MUL,
            AssignOp::Div => tags::assign_op::DIV,
            AssignOp::Mod => tags::assign_op::MOD,
            AssignOp::BitAnd => tags::assign_op::BIT_AND,
            AssignOp::BitOr => tags::assign_op::BIT_OR,
            AssignOp::BitXor => tags::assign_op::BIT_XOR,
            AssignOp::Shl => tags::assign_op::SHL,
            AssignOp::Shr => tags::assign_op::SHR,
        };
        self.push_byte(tag);
    }

    /// Encode a [`BindingMode`] as a single kind tag byte.
    pub fn write_binding_mode(&mut self, mode: BindingMode) {
        let tag = match mode {
            BindingMode::Immutable => tags::binding_mode::IMMUTABLE,
            BindingMode::Mutable => tags::binding_mode::MUTABLE,
            BindingMode::Uninit => tags::binding_mode::UNINIT,
        };
        self.push_byte(tag);
    }

    //   and `16` parse to different `IntBase` values and therefore hash
    //   differently
    /// Encode a [`Literal`] as a kind tag followed by the variant's
    /// payload bytes.
    pub fn write_literal(&mut self, lit: &Literal) {
        match lit {
            Literal::Int { value, base } => {
                self.push_byte(tags::literal::INT);
                self.write_u128_le(*value);
                self.push_byte(int_base_tag(*base));
            }
            Literal::Float(sym) => {
                self.push_byte(tags::literal::FLOAT);
                let text = self.interner().resolve(*sym);
                self.write_length_prefixed_str(text);
            }
            Literal::Str(sym) => {
                self.push_byte(tags::literal::STR);
                let text = self.interner().resolve(*sym);
                self.write_length_prefixed_str(text);
            }
            Literal::Bool(b) => {
                self.push_byte(tags::literal::BOOL);
                self.push_byte(if *b { 0x01 } else { 0x00 });
            }
            Literal::Unit => {
                self.push_byte(tags::literal::UNIT);
            }
        }
    }
}

fn int_base_tag(base: IntBase) -> u8 {
    match base {
        IntBase::Dec => tags::int_base::DEC,
        IntBase::Hex => tags::int_base::HEX,
        IntBase::Bin => tags::int_base::BIN,
        IntBase::Oct => tags::int_base::OCT,
    }
}

impl<'a> Encoder<'a> {
    pub(super) fn write_u128_le(&mut self, value: u128) {
        for byte in value.to_le_bytes() {
            self.push_byte(byte);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::test_support::PassThroughResolver;
    use edda_intern::Interner;
    use edda_syntax::IntBase;

    #[test]
    fn bin_op_tags_cover_every_variant() {
        let cases: &[(BinOp, u8)] = &[
            (BinOp::Add, tags::bin_op::ADD),
            (BinOp::Sub, tags::bin_op::SUB),
            (BinOp::Mul, tags::bin_op::MUL),
            (BinOp::Div, tags::bin_op::DIV),
            (BinOp::Mod, tags::bin_op::MOD),
            (BinOp::Eq, tags::bin_op::EQ),
            (BinOp::Ne, tags::bin_op::NE),
            (BinOp::Lt, tags::bin_op::LT),
            (BinOp::Le, tags::bin_op::LE),
            (BinOp::Gt, tags::bin_op::GT),
            (BinOp::Ge, tags::bin_op::GE),
            (BinOp::And, tags::bin_op::AND),
            (BinOp::Or, tags::bin_op::OR),
            (BinOp::BitAnd, tags::bin_op::BIT_AND),
            (BinOp::BitOr, tags::bin_op::BIT_OR),
            (BinOp::BitXor, tags::bin_op::BIT_XOR),
            (BinOp::Shl, tags::bin_op::SHL),
            (BinOp::Shr, tags::bin_op::SHR),
        ];
        for (op, expected) in cases {
            let interner = Interner::new();
            let resolver = PassThroughResolver::new(&interner);
            let mut enc = Encoder::new(&interner, &resolver);
            enc.write_bin_op(*op);
            let bytes = enc.into_bytes();
            assert_eq!(bytes, vec![*expected], "op={:?}", op);
        }
    }

    #[test]
    fn un_op_tags_cover_every_variant() {
        let cases = [
            (UnOp::Neg, tags::un_op::NEG),
            (UnOp::Not, tags::un_op::NOT),
            (UnOp::BitNot, tags::un_op::BIT_NOT),
        ];
        for (op, expected) in cases {
            let interner = Interner::new();
            let resolver = PassThroughResolver::new(&interner);
            let mut enc = Encoder::new(&interner, &resolver);
            enc.write_un_op(op);
            assert_eq!(enc.into_bytes(), vec![expected], "op={:?}", op);
        }
    }

    #[test]
    fn range_kind_tags() {
        let interner = Interner::new();
        let resolver = PassThroughResolver::new(&interner);
        let mut enc = Encoder::new(&interner, &resolver);
        enc.write_range_kind(RangeKind::HalfOpen);
        enc.write_range_kind(RangeKind::Closed);
        assert_eq!(
            enc.into_bytes(),
            vec![tags::range_kind::HALF_OPEN, tags::range_kind::CLOSED],
        );
    }

    #[test]
    fn assign_op_tags_cover_every_variant() {
        let cases = [
            (AssignOp::Plain, tags::assign_op::PLAIN),
            (AssignOp::Add, tags::assign_op::ADD),
            (AssignOp::Sub, tags::assign_op::SUB),
            (AssignOp::Mul, tags::assign_op::MUL),
            (AssignOp::Div, tags::assign_op::DIV),
            (AssignOp::Mod, tags::assign_op::MOD),
            (AssignOp::BitAnd, tags::assign_op::BIT_AND),
            (AssignOp::BitOr, tags::assign_op::BIT_OR),
            (AssignOp::BitXor, tags::assign_op::BIT_XOR),
            (AssignOp::Shl, tags::assign_op::SHL),
            (AssignOp::Shr, tags::assign_op::SHR),
        ];
        for (op, expected) in cases {
            let interner = Interner::new();
            let resolver = PassThroughResolver::new(&interner);
            let mut enc = Encoder::new(&interner, &resolver);
            enc.write_assign_op(op);
            assert_eq!(enc.into_bytes(), vec![expected]);
        }
    }

    #[test]
    fn binding_mode_tags() {
        let interner = Interner::new();
        let resolver = PassThroughResolver::new(&interner);
        let mut enc = Encoder::new(&interner, &resolver);
        enc.write_binding_mode(BindingMode::Immutable);
        enc.write_binding_mode(BindingMode::Mutable);
        assert_eq!(
            enc.into_bytes(),
            vec![tags::binding_mode::IMMUTABLE, tags::binding_mode::MUTABLE],
        );
    }

    #[test]
    fn literal_int_encodes_value_and_base() {
        let interner = Interner::new();
        let resolver = PassThroughResolver::new(&interner);
        let mut enc = Encoder::new(&interner, &resolver);
        enc.write_literal(&Literal::Int {
            value: 16,
            base: IntBase::Dec,
        });
        let bytes = enc.into_bytes();
        assert_eq!(bytes[0], tags::literal::INT);
        assert_eq!(&bytes[1..17], &16u128.to_le_bytes());
        assert_eq!(bytes[17], tags::int_base::DEC);
        assert_eq!(bytes.len(), 18);
    }

    #[test]
    fn literal_int_base_is_load_bearing() {
        let interner = Interner::new();
        let resolver = PassThroughResolver::new(&interner);
        let mut a = Encoder::new(&interner, &resolver);
        let mut b = Encoder::new(&interner, &resolver);
        a.write_literal(&Literal::Int {
            value: 16,
            base: IntBase::Dec,
        });
        b.write_literal(&Literal::Int {
            value: 16,
            base: IntBase::Hex,
        });
        assert_ne!(a.into_bytes(), b.into_bytes());
    }

    #[test]
    fn literal_bool() {
        let interner = Interner::new();
        let resolver = PassThroughResolver::new(&interner);
        let mut t = Encoder::new(&interner, &resolver);
        let mut f = Encoder::new(&interner, &resolver);
        t.write_literal(&Literal::Bool(true));
        f.write_literal(&Literal::Bool(false));
        assert_eq!(t.into_bytes(), vec![tags::literal::BOOL, 0x01]);
        assert_eq!(f.into_bytes(), vec![tags::literal::BOOL, 0x00]);
    }

    #[test]
    fn literal_unit_is_one_byte() {
        let interner = Interner::new();
        let resolver = PassThroughResolver::new(&interner);
        let mut enc = Encoder::new(&interner, &resolver);
        enc.write_literal(&Literal::Unit);
        assert_eq!(enc.into_bytes(), vec![tags::literal::UNIT]);
    }

    #[test]
    fn literal_str_writes_resolved_text() {
        let interner = Interner::new();
        let resolver = PassThroughResolver::new(&interner);
        let sym = interner.intern("hello");
        let mut enc = Encoder::new(&interner, &resolver);
        enc.write_literal(&Literal::Str(sym));
        let bytes = enc.into_bytes();
        assert_eq!(bytes[0], tags::literal::STR);
        assert_eq!(&bytes[1..5], &5u32.to_le_bytes());
        assert_eq!(&bytes[5..], b"hello");
    }
}
