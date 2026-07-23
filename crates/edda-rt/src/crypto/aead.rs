//! AEAD externs: AES-128/256-GCM and ChaCha20-Poly1305.
//!
//! All three use a 96-bit nonce and a detached 128-bit tag with additional
//! authenticated data, matching the `std.crypto.aead.*` `@abi` surface.
//! Encryption is infallible; decryption returns the Edda `Result<(), AeadError>`
//! wire shape and raises `auth_failed` on tag-verification failure.

use super::{in_bytes, out_bytes};
use crate::EdSlice;
use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes128Gcm, Aes256Gcm};
use chacha20poly1305::ChaCha20Poly1305;

const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

// `Result<(), AeadError>` wire shape. `AeadError` is the single-variant sum
// `{ auth_failed }`, so the payload slot is one discriminant byte (mirrors
// `EdAllocError`). Byte 0 is the Result tag (0 = Ok, 1 = Err); byte 1 is the
// `AeadError` discriminant, meaningful only when tag == 1 (0 = auth_failed).
#[repr(C)]
struct ResultUnitAeadError {
    tag: u8,
    err: u8,
}

#[inline]
fn aead_ok() -> ResultUnitAeadError {
    ResultUnitAeadError { tag: 0, err: 0 }
}

#[inline]
fn aead_auth_failed() -> ResultUnitAeadError {
    ResultUnitAeadError { tag: 1, err: 0 }
}

/// Seal `pt` into `ct` and write the detached authentication tag to `tag_out`.
fn seal<C: KeyInit + AeadInPlace>(
    key: &[u8],
    nonce: &[u8],
    pt: &[u8],
    aad: &[u8],
    ct: &mut [u8],
    tag_out: &mut [u8],
) {
    if nonce.len() != NONCE_LEN || tag_out.len() != TAG_LEN || ct.len() != pt.len() {
        return;
    }
    let Ok(cipher) = C::new_from_slice(key) else {
        return;
    };
    ct.copy_from_slice(pt);
    let n = GenericArray::from_slice(nonce);
    if let Ok(tag) = cipher.encrypt_in_place_detached(n, aad, ct) {
        tag_out.copy_from_slice(tag.as_slice());
    }
}

/// Open `ct` into `pt_out`, verifying `tag`. Returns `true` on success.
fn open<C: KeyInit + AeadInPlace>(
    key: &[u8],
    nonce: &[u8],
    ct: &[u8],
    aad: &[u8],
    tag: &[u8],
    pt_out: &mut [u8],
) -> bool {
    if nonce.len() != NONCE_LEN || tag.len() != TAG_LEN || pt_out.len() != ct.len() {
        return false;
    }
    let Ok(cipher) = C::new_from_slice(key) else {
        return false;
    };
    pt_out.copy_from_slice(ct);
    let n = GenericArray::from_slice(nonce);
    let t = GenericArray::from_slice(tag);
    match cipher.decrypt_in_place_detached(n, aad, pt_out, t) {
        Ok(()) => true,
        Err(_) => {
            pt_out.fill(0);
            false
        }
    }
}

/// AES-128-GCM seal (detached tag, with AAD).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_aes128_gcm_encrypt(
    key: EdSlice,
    nonce: EdSlice,
    plaintext: EdSlice,
    aad: EdSlice,
    ciphertext: *mut EdSlice,
    tag: *mut EdSlice,
) {
    // SAFETY: each slice arg satisfies the crypto-slice-borrow contract.
    unsafe {
        seal::<Aes128Gcm>(
            in_bytes(key),
            in_bytes(nonce),
            in_bytes(plaintext),
            in_bytes(aad),
            out_bytes(ciphertext),
            out_bytes(tag),
        );
    }
}

/// AES-128-GCM open (detached tag, with AAD).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_aes128_gcm_decrypt(
    key: EdSlice,
    nonce: EdSlice,
    ciphertext: EdSlice,
    aad: EdSlice,
    tag: EdSlice,
    plaintext: *mut EdSlice,
) -> ResultUnitAeadError {
    // SAFETY: each slice arg satisfies the crypto-slice-borrow contract.
    let ok = unsafe {
        open::<Aes128Gcm>(
            in_bytes(key),
            in_bytes(nonce),
            in_bytes(ciphertext),
            in_bytes(aad),
            in_bytes(tag),
            out_bytes(plaintext),
        )
    };
    if ok { aead_ok() } else { aead_auth_failed() }
}

/// AES-256-GCM seal (detached tag, with AAD).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_aes256_gcm_encrypt(
    key: EdSlice,
    nonce: EdSlice,
    plaintext: EdSlice,
    aad: EdSlice,
    ciphertext: *mut EdSlice,
    tag: *mut EdSlice,
) {
    // SAFETY: each slice arg satisfies the crypto-slice-borrow contract.
    unsafe {
        seal::<Aes256Gcm>(
            in_bytes(key),
            in_bytes(nonce),
            in_bytes(plaintext),
            in_bytes(aad),
            out_bytes(ciphertext),
            out_bytes(tag),
        );
    }
}

/// AES-256-GCM open (detached tag, with AAD).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_aes256_gcm_decrypt(
    key: EdSlice,
    nonce: EdSlice,
    ciphertext: EdSlice,
    aad: EdSlice,
    tag: EdSlice,
    plaintext: *mut EdSlice,
) -> ResultUnitAeadError {
    // SAFETY: each slice arg satisfies the crypto-slice-borrow contract.
    let ok = unsafe {
        open::<Aes256Gcm>(
            in_bytes(key),
            in_bytes(nonce),
            in_bytes(ciphertext),
            in_bytes(aad),
            in_bytes(tag),
            out_bytes(plaintext),
        )
    };
    if ok { aead_ok() } else { aead_auth_failed() }
}

/// ChaCha20-Poly1305 seal (detached tag, with AAD).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_chacha20_poly1305_encrypt(
    key: EdSlice,
    nonce: EdSlice,
    plaintext: EdSlice,
    aad: EdSlice,
    ciphertext: *mut EdSlice,
    tag: *mut EdSlice,
) {
    // SAFETY: each slice arg satisfies the crypto-slice-borrow contract.
    unsafe {
        seal::<ChaCha20Poly1305>(
            in_bytes(key),
            in_bytes(nonce),
            in_bytes(plaintext),
            in_bytes(aad),
            out_bytes(ciphertext),
            out_bytes(tag),
        );
    }
}

/// ChaCha20-Poly1305 open (detached tag, with AAD).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_chacha20_poly1305_decrypt(
    key: EdSlice,
    nonce: EdSlice,
    ciphertext: EdSlice,
    aad: EdSlice,
    tag: EdSlice,
    plaintext: *mut EdSlice,
) -> ResultUnitAeadError {
    // SAFETY: each slice arg satisfies the crypto-slice-borrow contract.
    let ok = unsafe {
        open::<ChaCha20Poly1305>(
            in_bytes(key),
            in_bytes(nonce),
            in_bytes(ciphertext),
            in_bytes(aad),
            in_bytes(tag),
            out_bytes(plaintext),
        )
    };
    if ok { aead_ok() } else { aead_auth_failed() }
}
