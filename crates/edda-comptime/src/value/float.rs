//! Comptime floating-point value.
//!
//! IEEE-754 single and double precision per `types.md` *Floating
//! point*. The width tag is the corresponding [`edda_types::Primitive`]
//! (`F32` or `F64`).
//!
//! [`FloatValue`] deliberately does not implement `Eq` or `Hash`
//! because IEEE-754 equality is not reflexive on NaN; callers that
//! need bitwise comparison should match on the variants and compare
//! payload bits.

use edda_types::Primitive;

/// A comptime floating-point value.
#[derive(Copy, Clone, Debug)]
pub enum FloatValue {
    /// `f32` payload.
    F32(f32),
    /// `f64` payload.
    F64(f64),
}

impl FloatValue {
    /// Width of this float as the corresponding [`Primitive`].
    pub const fn width(self) -> Primitive {
        match self {
            Self::F32(_) => Primitive::F32,
            Self::F64(_) => Primitive::F64,
        }
    }
}
