//! GF(p) arithmetic for the NIST P-384 base field (p = 2^384 - 2^128 -
//! 2^96 + 2^32 - 1). Elements are canonical 48-byte big-endian values.

use super::super::{in_bytes, out_bytes};
use crate::EdSlice;
use crypto_bigint::modular::runtime_mod::{DynResidue, DynResidueParams};
use crypto_bigint::{Encoding, U384};
use subtle::{ConditionallySelectable, ConstantTimeEq};

const LIMBS: usize = U384::LIMBS;

const MODULUS: U384 = U384::from_be_hex(
    "fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffeffffffff0000000000000000ffffffff",
);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Element {
    bytes: [u8; 48],
}

// `Option<Element>` wire shape — `tag` 0 = some, 1 = none (Option's
// some-first/none-second variant order); `value` defined only when tag == 0.
#[repr(C)]
pub struct OptionElement {
    tag: u8,
    value: [u8; 48],
}

#[inline]
fn some(bytes: [u8; 48]) -> OptionElement {
    OptionElement { tag: 0, value: bytes }
}

#[inline]
fn none() -> OptionElement {
    OptionElement { tag: 1, value: [0; 48] }
}

#[inline]
fn to_res(bytes: &[u8; 48]) -> DynResidue<LIMBS> {
    DynResidue::new(&U384::from_be_slice(bytes), DynResidueParams::new(&MODULUS))
}

#[inline]
fn from_res(r: &DynResidue<LIMBS>) -> [u8; 48] {
    r.retrieve().to_be_bytes()
}

/// P-384 field addition.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_gfp_p384_add(a: Element, b: Element) -> Element {
    Element { bytes: from_res(&to_res(&a.bytes).add(&to_res(&b.bytes))) }
}

/// P-384 field subtraction.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_gfp_p384_sub(a: Element, b: Element) -> Element {
    Element { bytes: from_res(&to_res(&a.bytes).sub(&to_res(&b.bytes))) }
}

/// P-384 field multiplication.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_gfp_p384_mul(a: Element, b: Element) -> Element {
    Element { bytes: from_res(&to_res(&a.bytes).mul(&to_res(&b.bytes))) }
}

/// P-384 field squaring.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_gfp_p384_square(a: Element) -> Element {
    Element { bytes: from_res(&to_res(&a.bytes).square()) }
}

/// P-384 field negation.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_gfp_p384_neg(a: Element) -> Element {
    Element { bytes: from_res(&to_res(&a.bytes).neg()) }
}

/// P-384 field inversion.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_gfp_p384_inv(a: Element) -> OptionElement {
    let (inv, is_invertible) = to_res(&a.bytes).invert();
    if bool::from(is_invertible) { some(from_res(&inv)) } else { none() }
}

/// P-384 field exponentiation by an arbitrary big-endian exponent.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_gfp_p384_pow(a: Element, exponent: EdSlice) -> Element {
    // SAFETY: `exponent` satisfies the crypto-slice-borrow contract.
    let exp = unsafe { in_bytes(exponent) };
    let base = to_res(&a.bytes);
    let mut acc = DynResidue::one(DynResidueParams::new(&MODULUS));
    for &byte in exp {
        for i in (0..8).rev() {
            acc = acc.square();
            if (byte >> i) & 1 == 1 {
                acc = acc.mul(&base);
            }
        }
    }
    Element { bytes: from_res(&acc) }
}

/// P-384 field zero test.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_gfp_p384_is_zero(a: Element) -> u8 {
    a.bytes.ct_eq(&[0u8; 48]).unwrap_u8()
}

/// P-384 field equality.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_gfp_p384_eq(a: Element, b: Element) -> u8 {
    a.bytes.ct_eq(&b.bytes).unwrap_u8()
}

/// P-384 constant-time field select.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_gfp_p384_select(cond: u8, a: Element, b: Element) -> Element {
    let choice = subtle::Choice::from((cond != 0) as u8);
    let mut out = [0u8; 48];
    for i in 0..48 {
        out[i] = u8::conditional_select(&b.bytes[i], &a.bytes[i], choice);
    }
    Element { bytes: out }
}

/// P-384 canonical field-element decoding.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_gfp_p384_from_bytes_be(bytes: EdSlice) -> OptionElement {
    // SAFETY: `bytes` satisfies the crypto-slice-borrow contract.
    let b = unsafe { in_bytes(bytes) };
    if b.len() != 48 {
        return none();
    }
    if U384::from_be_slice(b) < MODULUS {
        let mut arr = [0u8; 48];
        arr.copy_from_slice(b);
        some(arr)
    } else {
        none()
    }
}

/// P-384 field-element encoding.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_gfp_p384_to_bytes_be(a: Element, out: *mut EdSlice) {
    // SAFETY: `out` satisfies the crypto-slice-borrow-mut contract.
    let o = unsafe { out_bytes(out) };
    if o.len() == 48 {
        o.copy_from_slice(&a.bytes);
    }
}
