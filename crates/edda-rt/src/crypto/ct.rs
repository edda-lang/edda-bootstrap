//! Constant-time byte-comparison and zeroization externs (`std.crypto.subtle`).

use super::{in_bytes, out_bytes};
use crate::EdSlice;
use subtle::ConstantTimeEq;
use zeroize::Zeroize;

/// Constant-time byte equality. Returns `1` (true) iff `a == b`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_subtle_eq_bytes(a: EdSlice, b: EdSlice) -> u8 {
    // SAFETY: each slice arg satisfies the crypto-slice-borrow contract.
    let (a, b) = unsafe { (in_bytes(a), in_bytes(b)) };
    if a.len() != b.len() {
        return 0;
    }
    a.ct_eq(b).unwrap_u8()
}

/// Securely zero the bytes of `buf`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_subtle_zeroize(buf: *mut EdSlice) {
    // SAFETY: `buf` satisfies the crypto-slice-borrow-mut contract.
    unsafe { out_bytes(buf) }.zeroize();
}
