//! MIR-level constants: typed scalar values referenced by [`crate::Operand`].

use edda_intern::Symbol;

use crate::ty::MirType;

/// A typed compile-time constant.
#[derive(Clone, Eq, PartialEq, Debug)]
pub struct Const {
    /// Type of the constant value.
    pub ty: MirType,
    /// Value payload.
    pub value: ConstValue,
}

/// Constant payload. Codegen narrows integer constants to the destination
/// primitive width.
#[derive(Clone, Eq, PartialEq, Debug)]
pub enum ConstValue {
    /// Signed integer constant; codegen narrows to the destination primitive.
    Int(i128),
    /// Unsigned integer constant; codegen narrows to the destination primitive.
    Uint(u128),
    /// Floating-point constant stored as the `f64::to_bits` pattern.
    Float(u64),
    /// Boolean constant.
    Bool(bool),
    /// Interned UTF-8 string literal.
    Str(Symbol),
    /// `()` constant.
    Unit,
    /// Zero-initialised value of `ty` (used for `init`-mode slot init).
    Zero,
}
