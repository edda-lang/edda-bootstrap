//! Crypto/PKI runtime externs backing `std.crypto.*`.
//!
//! Sub-unit 1 — AEAD, KDF, CSPRNG, and constant-time byte ops. Every
//! primitive is backed by an audited pure-Rust RustCrypto crate so the
//! staticlib stays cross-compilable to every target triple with no C or
//! assembly toolchain.
//!
//! ABI conventions (mirrors the `@abi(...)` signatures in each
//! `std.crypto.*` package):
//!   - immutable `[u8]` param  → `EdSlice` by value (`{ ptr, len }`)
//!   - `mutable [u8]` out-param → `*mut EdSlice` (pointer to the caller's slot)
//!   - capability param         → leading opaque `*const ()` (ignored)
//!   - `-> bool`                → `u8` (0/1); `-> ()`            → void
//!   - `-> () with {err: E}`    → the Edda sum-ADT Result wire shape

use crate::EdSlice;

pub mod aead;
pub mod bigint;
pub mod ct;
pub mod ecdh;
pub mod ecdsa;
pub mod ed25519;
pub mod ed448;
pub mod gfp;
pub mod kdf;
pub mod random;
pub mod rsa;
pub mod secp256k1;
pub mod x509;

/// Borrow an immutable Edda `[u8]` slice argument as a Rust byte slice.
unsafe fn in_bytes<'a>(s: EdSlice) -> &'a [u8] {
    if s.ptr.is_null() || s.len == 0 {
        return &[];
    }
    // SAFETY: caller asserts `ptr` heads `len` initialised, readable bytes.
    unsafe { std::slice::from_raw_parts(s.ptr as *const u8, s.len as usize) }
}

/// Borrow a `mutable [u8]` out-param slot as a writable Rust byte slice.
pub(crate) unsafe fn out_bytes<'a>(s: *mut EdSlice) -> &'a mut [u8] {
    if s.is_null() {
        return &mut [];
    }
    // SAFETY: caller asserts `s` heads the live `mutable [u8]` slot.
    let slot = unsafe { &*s };
    if slot.ptr.is_null() || slot.len == 0 {
        return &mut [];
    }
    // SAFETY: `slot.ptr` heads `slot.len` writable bytes.
    unsafe { std::slice::from_raw_parts_mut(slot.ptr, slot.len as usize) }
}
