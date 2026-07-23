//! X.509 certificate externs (`std.crypto.x509.{cert,chain}`), backed by the
//! pure-Rust `x509-parser` for DER/PEM parsing and field access, with
//! signature verification routed through the existing RustCrypto verifiers
//! (RSA / ECDSA / Ed25519). `Certificate` and `TrustStore` are opaque `u64`
//! handles (affine types): a `Certificate` is a leaked `Box<Vec<u8>>` of owned
//! DER (re-parsed per accessor); a `TrustStore` is a leaked `Box<Vec<Vec<u8>>>`
//! of trust-anchor DER. `rustls-webpki` is intentionally NOT used: its only
//! crypto providers are C-backed (ring / aws-lc-rs), which would break the
//! pure-Rust / no-C-toolchain constraint — so `validate_chain` is hand-rolled
//! over the same per-cert primitives.

use crate::EdSlice;

pub mod cert;
pub mod chain;
pub(crate) mod verify;

// `Result<Handle, Err>` (parse_der / trust_store_new) — tag at 0, the u64
// handle (or err discriminant in its low byte) at offset 8. 16 bytes, sret.
#[repr(C)]
pub(crate) struct ResultHandle {
    pub tag: u8,
    pub _pad: [u8; 7],
    pub payload: u64,
}

// `Result<(), Err>` — 2 bytes.
#[repr(C)]
pub(crate) struct ResultUnit {
    pub tag: u8,
    pub err: u8,
}

// `Result<bool|enum, Err>` — 2 bytes. `val` is the bool / result-enum
// discriminant on Ok, the err discriminant on Err.
#[repr(C)]
pub(crate) struct ResultByte {
    pub tag: u8,
    pub val: u8,
}

#[inline]
pub(crate) fn ok_handle(h: u64) -> ResultHandle {
    ResultHandle { tag: 0, _pad: [0; 7], payload: h }
}

#[inline]
pub(crate) fn err_handle(d: u8) -> ResultHandle {
    ResultHandle { tag: 1, _pad: [0; 7], payload: d as u64 }
}

#[inline]
pub(crate) fn ok_unit() -> ResultUnit {
    ResultUnit { tag: 0, err: 0 }
}

#[inline]
pub(crate) fn err_unit(d: u8) -> ResultUnit {
    ResultUnit { tag: 1, err: d }
}

#[inline]
pub(crate) fn ok_byte(v: u8) -> ResultByte {
    ResultByte { tag: 0, val: v }
}

#[inline]
pub(crate) fn err_byte(d: u8) -> ResultByte {
    ResultByte { tag: 1, val: d }
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
