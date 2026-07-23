//! RSA-OAEP encryption externs over SHA-256/384/512, with optional label.

use super::super::in_bytes;
use super::{ResultUnit, err_unit, ok_unit, write_out};
use crate::EdSlice;
use digest::{Digest, DynDigest};
use rand_core::OsRng;
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey};
use rsa::{Oaep, RsaPrivateKey, RsaPublicKey};
use sha2::{Sha256, Sha384, Sha512};

// RsaOaepError discriminants.
const INVALID_SPKI: u8 = 0;
const INVALID_PKCS8: u8 = 1;
const PT_TOO_LONG: u8 = 4;
const CT_BUF_SMALL: u8 = 5;
const PT_BUF_SMALL: u8 = 6;
const DECRYPT_ERR: u8 = 7;
const INTERNAL: u8 = 8;

fn make_padding<D>(label: Option<&[u8]>) -> Option<Oaep>
where
    D: Digest + DynDigest + Send + Sync + 'static,
{
    match label {
        None => Some(Oaep::new::<D>()),
        Some(l) => core::str::from_utf8(l)
            .ok()
            .map(|s| Oaep::new_with_label::<D, _>(s)),
    }
}

fn oaep_encrypt<D>(
    spki: &[u8],
    pt: &[u8],
    label: Option<&[u8]>,
    ct_out: *mut EdSlice,
    ct_len_out: *mut u64,
) -> ResultUnit
where
    D: Digest + DynDigest + Send + Sync + 'static,
{
    let Ok(pk) = RsaPublicKey::from_public_key_der(spki) else {
        return err_unit(INVALID_SPKI);
    };
    let Some(padding) = make_padding::<D>(label) else {
        return err_unit(INTERNAL);
    };
    let Ok(ct) = pk.encrypt(&mut OsRng, padding, pt) else {
        return err_unit(PT_TOO_LONG);
    };
    // SAFETY: out/len slots satisfy the crypto-slice-borrow-mut contract.
    if unsafe { !write_out(ct_out, ct_len_out, &ct) } {
        return err_unit(CT_BUF_SMALL);
    }
    ok_unit()
}

fn oaep_decrypt<D>(
    pkcs8: &[u8],
    ct: &[u8],
    label: Option<&[u8]>,
    pt_out: *mut EdSlice,
    pt_len_out: *mut u64,
) -> ResultUnit
where
    D: Digest + DynDigest + Send + Sync + 'static,
{
    let Ok(sk) = RsaPrivateKey::from_pkcs8_der(pkcs8) else {
        return err_unit(INVALID_PKCS8);
    };
    let Some(padding) = make_padding::<D>(label) else {
        return err_unit(INTERNAL);
    };
    let Ok(pt) = sk.decrypt(padding, ct) else {
        return err_unit(DECRYPT_ERR);
    };
    // SAFETY: out/len slots satisfy the crypto-slice-borrow-mut contract.
    if unsafe { !write_out(pt_out, pt_len_out, &pt) } {
        return err_unit(PT_BUF_SMALL);
    }
    ok_unit()
}

/// RSA-OAEP encrypt (SHA-256, empty label).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_oaep_encrypt_sha256(
    _random: *const (),
    public_key_spki_der: EdSlice,
    plaintext: EdSlice,
    ciphertext_out: *mut EdSlice,
    ciphertext_len_out: *mut u64,
) -> ResultUnit {
    unsafe {
        oaep_encrypt::<Sha256>(in_bytes(public_key_spki_der), in_bytes(plaintext), None, ciphertext_out, ciphertext_len_out)
    }
}

/// RSA-OAEP encrypt (SHA-384, empty label).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_oaep_encrypt_sha384(
    _random: *const (),
    public_key_spki_der: EdSlice,
    plaintext: EdSlice,
    ciphertext_out: *mut EdSlice,
    ciphertext_len_out: *mut u64,
) -> ResultUnit {
    unsafe {
        oaep_encrypt::<Sha384>(in_bytes(public_key_spki_der), in_bytes(plaintext), None, ciphertext_out, ciphertext_len_out)
    }
}

/// RSA-OAEP encrypt (SHA-512, empty label).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_oaep_encrypt_sha512(
    _random: *const (),
    public_key_spki_der: EdSlice,
    plaintext: EdSlice,
    ciphertext_out: *mut EdSlice,
    ciphertext_len_out: *mut u64,
) -> ResultUnit {
    unsafe {
        oaep_encrypt::<Sha512>(in_bytes(public_key_spki_der), in_bytes(plaintext), None, ciphertext_out, ciphertext_len_out)
    }
}

/// RSA-OAEP encrypt (SHA-256, labelled).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_oaep_encrypt_sha256_lbl(
    _random: *const (),
    public_key_spki_der: EdSlice,
    plaintext: EdSlice,
    label: EdSlice,
    ciphertext_out: *mut EdSlice,
    ciphertext_len_out: *mut u64,
) -> ResultUnit {
    unsafe {
        oaep_encrypt::<Sha256>(in_bytes(public_key_spki_der), in_bytes(plaintext), Some(in_bytes(label)), ciphertext_out, ciphertext_len_out)
    }
}

/// RSA-OAEP encrypt (SHA-384, labelled).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_oaep_encrypt_sha384_lbl(
    _random: *const (),
    public_key_spki_der: EdSlice,
    plaintext: EdSlice,
    label: EdSlice,
    ciphertext_out: *mut EdSlice,
    ciphertext_len_out: *mut u64,
) -> ResultUnit {
    unsafe {
        oaep_encrypt::<Sha384>(in_bytes(public_key_spki_der), in_bytes(plaintext), Some(in_bytes(label)), ciphertext_out, ciphertext_len_out)
    }
}

/// RSA-OAEP encrypt (SHA-512, labelled).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_oaep_encrypt_sha512_lbl(
    _random: *const (),
    public_key_spki_der: EdSlice,
    plaintext: EdSlice,
    label: EdSlice,
    ciphertext_out: *mut EdSlice,
    ciphertext_len_out: *mut u64,
) -> ResultUnit {
    unsafe {
        oaep_encrypt::<Sha512>(in_bytes(public_key_spki_der), in_bytes(plaintext), Some(in_bytes(label)), ciphertext_out, ciphertext_len_out)
    }
}

/// RSA-OAEP decrypt (SHA-256, empty label).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_oaep_decrypt_sha256(
    private_key_pkcs8_der: EdSlice,
    ciphertext: EdSlice,
    plaintext_out: *mut EdSlice,
    plaintext_len_out: *mut u64,
) -> ResultUnit {
    unsafe {
        oaep_decrypt::<Sha256>(in_bytes(private_key_pkcs8_der), in_bytes(ciphertext), None, plaintext_out, plaintext_len_out)
    }
}

/// RSA-OAEP decrypt (SHA-384, empty label).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_oaep_decrypt_sha384(
    private_key_pkcs8_der: EdSlice,
    ciphertext: EdSlice,
    plaintext_out: *mut EdSlice,
    plaintext_len_out: *mut u64,
) -> ResultUnit {
    unsafe {
        oaep_decrypt::<Sha384>(in_bytes(private_key_pkcs8_der), in_bytes(ciphertext), None, plaintext_out, plaintext_len_out)
    }
}

/// RSA-OAEP decrypt (SHA-512, empty label).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_oaep_decrypt_sha512(
    private_key_pkcs8_der: EdSlice,
    ciphertext: EdSlice,
    plaintext_out: *mut EdSlice,
    plaintext_len_out: *mut u64,
) -> ResultUnit {
    unsafe {
        oaep_decrypt::<Sha512>(in_bytes(private_key_pkcs8_der), in_bytes(ciphertext), None, plaintext_out, plaintext_len_out)
    }
}

/// RSA-OAEP decrypt (SHA-256, labelled).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_oaep_decrypt_sha256_lbl(
    private_key_pkcs8_der: EdSlice,
    ciphertext: EdSlice,
    label: EdSlice,
    plaintext_out: *mut EdSlice,
    plaintext_len_out: *mut u64,
) -> ResultUnit {
    unsafe {
        oaep_decrypt::<Sha256>(in_bytes(private_key_pkcs8_der), in_bytes(ciphertext), Some(in_bytes(label)), plaintext_out, plaintext_len_out)
    }
}

/// RSA-OAEP decrypt (SHA-384, labelled).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_oaep_decrypt_sha384_lbl(
    private_key_pkcs8_der: EdSlice,
    ciphertext: EdSlice,
    label: EdSlice,
    plaintext_out: *mut EdSlice,
    plaintext_len_out: *mut u64,
) -> ResultUnit {
    unsafe {
        oaep_decrypt::<Sha384>(in_bytes(private_key_pkcs8_der), in_bytes(ciphertext), Some(in_bytes(label)), plaintext_out, plaintext_len_out)
    }
}

/// RSA-OAEP decrypt (SHA-512, labelled).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_oaep_decrypt_sha512_lbl(
    private_key_pkcs8_der: EdSlice,
    ciphertext: EdSlice,
    label: EdSlice,
    plaintext_out: *mut EdSlice,
    plaintext_len_out: *mut u64,
) -> ResultUnit {
    unsafe {
        oaep_decrypt::<Sha512>(in_bytes(private_key_pkcs8_der), in_bytes(ciphertext), Some(in_bytes(label)), plaintext_out, plaintext_len_out)
    }
}
