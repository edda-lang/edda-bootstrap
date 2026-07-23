//! HKDF externs: RFC 5869 extract/expand plus the TLS 1.3 (RFC 8446 §7.1)
//! HKDF-Expand-Label / Derive-Secret derivations, over SHA-256 and SHA-384.

use super::{in_bytes, out_bytes};
use crate::EdSlice;
use hkdf::Hkdf;
use sha2::{Sha256, Sha384};

/// Build the RFC 8446 `HkdfLabel` structure for HKDF-Expand-Label.
fn build_hkdf_label(label: &[u8], context: &[u8], length: usize) -> Option<Vec<u8>> {
    let full_label_len = 6 + label.len(); // "tls13 " is 6 bytes
    if length > u16::MAX as usize || full_label_len > 255 || context.len() > 255 {
        return None;
    }
    let mut out = Vec::with_capacity(2 + 1 + full_label_len + 1 + context.len());
    out.extend_from_slice(&(length as u16).to_be_bytes());
    out.push(full_label_len as u8);
    out.extend_from_slice(b"tls13 ");
    out.extend_from_slice(label);
    out.push(context.len() as u8);
    out.extend_from_slice(context);
    Some(out)
}

/// HKDF-Expand-Label over SHA-256 into `out` (no-op on bad lengths).
fn expand_label_256(secret: &[u8], label: &[u8], context: &[u8], out: &mut [u8]) {
    if let Some(info) = build_hkdf_label(label, context, out.len())
        && let Ok(hk) = Hkdf::<Sha256>::from_prk(secret)
    {
        let _ = hk.expand(&info, out);
    }
}

/// HKDF-Expand-Label over SHA-384 into `out` (no-op on bad lengths).
fn expand_label_384(secret: &[u8], label: &[u8], context: &[u8], out: &mut [u8]) {
    if let Some(info) = build_hkdf_label(label, context, out.len())
        && let Ok(hk) = Hkdf::<Sha384>::from_prk(secret)
    {
        let _ = hk.expand(&info, out);
    }
}

/// HKDF-Extract over SHA-256 → 32-byte PRK.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_hkdf_sha256_extract(salt: EdSlice, ikm: EdSlice, prk: *mut EdSlice) {
    // SAFETY: each slice arg satisfies the crypto-slice-borrow contract.
    let (salt, ikm, out) = unsafe { (in_bytes(salt), in_bytes(ikm), out_bytes(prk)) };
    let (prk_bytes, _) = Hkdf::<Sha256>::extract(Some(salt), ikm);
    if out.len() == prk_bytes.len() {
        out.copy_from_slice(&prk_bytes);
    }
}

/// HKDF-Expand over SHA-256 into `okm`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_hkdf_sha256_expand(prk: EdSlice, info: EdSlice, okm: *mut EdSlice) {
    // SAFETY: each slice arg satisfies the crypto-slice-borrow contract.
    let (prk, info, out) = unsafe { (in_bytes(prk), in_bytes(info), out_bytes(okm)) };
    if let Ok(hk) = Hkdf::<Sha256>::from_prk(prk) {
        let _ = hk.expand(info, out);
    }
}

/// HKDF-Extract over SHA-384 → 48-byte PRK.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_hkdf_sha384_extract(salt: EdSlice, ikm: EdSlice, prk: *mut EdSlice) {
    // SAFETY: each slice arg satisfies the crypto-slice-borrow contract.
    let (salt, ikm, out) = unsafe { (in_bytes(salt), in_bytes(ikm), out_bytes(prk)) };
    let (prk_bytes, _) = Hkdf::<Sha384>::extract(Some(salt), ikm);
    if out.len() == prk_bytes.len() {
        out.copy_from_slice(&prk_bytes);
    }
}

/// HKDF-Expand over SHA-384 into `okm`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_hkdf_sha384_expand(prk: EdSlice, info: EdSlice, okm: *mut EdSlice) {
    // SAFETY: each slice arg satisfies the crypto-slice-borrow contract.
    let (prk, info, out) = unsafe { (in_bytes(prk), in_bytes(info), out_bytes(okm)) };
    if let Ok(hk) = Hkdf::<Sha384>::from_prk(prk) {
        let _ = hk.expand(info, out);
    }
}

/// TLS 1.3 HKDF-Expand-Label over SHA-256.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_hkdf_sha256_expand_label(
    secret: EdSlice,
    label: EdSlice,
    context: EdSlice,
    out: *mut EdSlice,
) {
    // SAFETY: each slice arg satisfies the crypto-slice-borrow contract.
    let (secret, label, context, out) =
        unsafe { (in_bytes(secret), in_bytes(label), in_bytes(context), out_bytes(out)) };
    expand_label_256(secret, label, context, out);
}

/// TLS 1.3 Derive-Secret over SHA-256.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_hkdf_sha256_derive_secret(
    secret: EdSlice,
    label: EdSlice,
    transcript_hash: EdSlice,
    out: *mut EdSlice,
) {
    // SAFETY: each slice arg satisfies the crypto-slice-borrow contract.
    let (secret, label, th, out) =
        unsafe { (in_bytes(secret), in_bytes(label), in_bytes(transcript_hash), out_bytes(out)) };
    expand_label_256(secret, label, th, out);
}

/// TLS 1.3 HKDF-Expand-Label over SHA-384.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_hkdf_sha384_expand_label(
    secret: EdSlice,
    label: EdSlice,
    context: EdSlice,
    out: *mut EdSlice,
) {
    // SAFETY: each slice arg satisfies the crypto-slice-borrow contract.
    let (secret, label, context, out) =
        unsafe { (in_bytes(secret), in_bytes(label), in_bytes(context), out_bytes(out)) };
    expand_label_384(secret, label, context, out);
}

/// TLS 1.3 Derive-Secret over SHA-384.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_hkdf_sha384_derive_secret(
    secret: EdSlice,
    label: EdSlice,
    transcript_hash: EdSlice,
    out: *mut EdSlice,
) {
    // SAFETY: each slice arg satisfies the crypto-slice-borrow contract.
    let (secret, label, th, out) =
        unsafe { (in_bytes(secret), in_bytes(label), in_bytes(transcript_hash), out_bytes(out)) };
    expand_label_384(secret, label, th, out);
}
