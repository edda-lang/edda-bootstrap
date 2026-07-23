//! RSA externs: PSS / PKCS#1 v1.5 signatures (`std.crypto.sig.rsa`) and OAEP
//! encryption (`std.crypto.pke.rsa.oaep`), backed by the `rsa` RustCrypto
//! crate. Keys cross the ABI as DER (SPKI / PKCS#8) byte slices; variable
//! outputs (signatures, ciphertexts, key components) write into caller buffers
//! paired with a `mutable usize` length slot. All returns use the 2-byte
//! single-err `Result<(),_>` / `Result<bool,_>` wire shapes.

use crate::EdSlice;

pub mod oaep;
pub mod sig;

// `Result<(), RsaSigError | RsaOaepError>` — tag 0 = Ok, 1 = Err(`err`).
#[repr(C)]
pub(crate) struct ResultUnit {
    pub tag: u8,
    pub err: u8,
}

// `Result<bool, RsaSigError>` — tag 0 = Ok(`val`), 1 = Err(`val`).
#[repr(C)]
pub(crate) struct ResultBool {
    pub tag: u8,
    pub val: u8,
}

#[inline]
pub(crate) fn ok_unit() -> ResultUnit {
    ResultUnit { tag: 0, err: 0 }
}

#[inline]
pub(crate) fn err_unit(discriminant: u8) -> ResultUnit {
    ResultUnit { tag: 1, err: discriminant }
}

#[inline]
pub(crate) fn ok_bool(v: bool) -> ResultBool {
    ResultBool { tag: 0, val: v as u8 }
}

#[inline]
pub(crate) fn err_bool(discriminant: u8) -> ResultBool {
    ResultBool { tag: 1, val: discriminant }
}

/// Write a variable-length byte output into a caller buffer + length slot.
pub(crate) unsafe fn write_out(out: *mut EdSlice, len_out: *mut u64, bytes: &[u8]) -> bool {
    // SAFETY: `out` satisfies the crypto-slice-borrow-mut contract.
    let o = unsafe { super::out_bytes(out) };
    if bytes.len() > o.len() {
        return false;
    }
    o[..bytes.len()].copy_from_slice(bytes);
    if !len_out.is_null() {
        // SAFETY: `len_out` is the caller's `mutable usize` slot.
        unsafe { *len_out = bytes.len() as u64 };
    }
    true
}
