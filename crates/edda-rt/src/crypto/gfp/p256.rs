//! GF(p) arithmetic for the NIST P-256 base field (p = 2^256 - 2^224 +
//! 2^192 + 2^96 - 1). Elements are canonical 32-byte big-endian values.

use super::super::{in_bytes, out_bytes};
use crate::EdSlice;
use crypto_bigint::modular::runtime_mod::{DynResidue, DynResidueParams};
use crypto_bigint::{Encoding, U256};
use subtle::{ConditionallySelectable, ConstantTimeEq};

const LIMBS: usize = U256::LIMBS;

const MODULUS: U256 =
    U256::from_be_hex("ffffffff00000001000000000000000000000000ffffffffffffffffffffffff");

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Element {
    bytes: [u8; 32],
}

// `Option<Element>` wire shape — `tag` 0 = some, 1 = none (Option's
// some-first/none-second variant order); `value` defined only when tag == 0.
#[repr(C)]
pub struct OptionElement {
    tag: u8,
    value: [u8; 32],
}

#[inline]
fn some(bytes: [u8; 32]) -> OptionElement {
    OptionElement { tag: 0, value: bytes }
}

#[inline]
fn none() -> OptionElement {
    OptionElement { tag: 1, value: [0; 32] }
}

#[inline]
fn to_res(bytes: &[u8; 32]) -> DynResidue<LIMBS> {
    DynResidue::new(&U256::from_be_slice(bytes), DynResidueParams::new(&MODULUS))
}

#[inline]
fn from_res(r: &DynResidue<LIMBS>) -> [u8; 32] {
    r.retrieve().to_be_bytes()
}

/// P-256 field addition.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_gfp_p256_add(a: Element, b: Element) -> Element {
    Element { bytes: from_res(&to_res(&a.bytes).add(&to_res(&b.bytes))) }
}

/// P-256 field subtraction.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_gfp_p256_sub(a: Element, b: Element) -> Element {
    Element { bytes: from_res(&to_res(&a.bytes).sub(&to_res(&b.bytes))) }
}

/// P-256 field multiplication.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_gfp_p256_mul(a: Element, b: Element) -> Element {
    Element { bytes: from_res(&to_res(&a.bytes).mul(&to_res(&b.bytes))) }
}

/// P-256 field squaring.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_gfp_p256_square(a: Element) -> Element {
    Element { bytes: from_res(&to_res(&a.bytes).square()) }
}

/// P-256 field negation.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_gfp_p256_neg(a: Element) -> Element {
    Element { bytes: from_res(&to_res(&a.bytes).neg()) }
}

/// P-256 field inversion.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_gfp_p256_inv(a: Element) -> OptionElement {
    let (inv, is_invertible) = to_res(&a.bytes).invert();
    if bool::from(is_invertible) { some(from_res(&inv)) } else { none() }
}

/// P-256 field exponentiation by an arbitrary big-endian exponent.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_gfp_p256_pow(a: Element, exponent: EdSlice) -> Element {
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

/// P-256 field zero test.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_gfp_p256_is_zero(a: Element) -> u8 {
    a.bytes.ct_eq(&[0u8; 32]).unwrap_u8()
}

/// P-256 field equality.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_gfp_p256_eq(a: Element, b: Element) -> u8 {
    a.bytes.ct_eq(&b.bytes).unwrap_u8()
}

/// P-256 constant-time field select.
#[unsafe(no_mangle)]
pub extern "C" fn __edda_gfp_p256_select(cond: u8, a: Element, b: Element) -> Element {
    let choice = subtle::Choice::from((cond != 0) as u8);
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::conditional_select(&b.bytes[i], &a.bytes[i], choice);
    }
    Element { bytes: out }
}

/// P-256 canonical field-element decoding.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_gfp_p256_from_bytes_be(bytes: EdSlice) -> OptionElement {
    // SAFETY: `bytes` satisfies the crypto-slice-borrow contract.
    let b = unsafe { in_bytes(bytes) };
    if b.len() != 32 {
        return none();
    }
    if U256::from_be_slice(b) < MODULUS {
        let mut arr = [0u8; 32];
        arr.copy_from_slice(b);
        some(arr)
    } else {
        none()
    }
}

/// P-256 field-element encoding.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_gfp_p256_to_bytes_be(a: Element, out: *mut EdSlice) {
    // SAFETY: `out` satisfies the crypto-slice-borrow-mut contract.
    let o = unsafe { out_bytes(out) };
    if o.len() == 32 {
        o.copy_from_slice(&a.bytes);
    }
}
