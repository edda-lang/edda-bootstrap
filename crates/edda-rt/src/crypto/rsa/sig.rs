//! RSA signature externs: PSS and PKCS#1 v1.5 over SHA-256/384/512, plus
//! SPKI / PKCS#8 public-component extraction and SPKI encoding.

use super::super::in_bytes;
use super::{ResultBool, ResultUnit, err_bool, err_unit, ok_bool, ok_unit, write_out};
use crate::EdSlice;
use const_oid::AssociatedOid;
use digest::{Digest, FixedOutputReset};
use rand_core::OsRng;
use rsa::pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePublicKey};
use rsa::signature::{RandomizedSigner, SignatureEncoding, Signer, Verifier};
use rsa::traits::PublicKeyParts;
use rsa::{BigUint, RsaPrivateKey, RsaPublicKey, pkcs1v15, pss};
use sha2::{Sha256, Sha384, Sha512};

// RsaSigError discriminants.
const INVALID_SPKI: u8 = 0;
const INVALID_PKCS8: u8 = 1;
const UNSUPPORTED_MODULUS: u8 = 2;
const SIG_LEN_MISMATCH: u8 = 3;
const INTERNAL: u8 = 4;

/// PSS verify over digest `D`.
fn pss_verify<D>(spki: &[u8], msg: &[u8], sig: &[u8]) -> ResultBool
where
    D: Digest + AssociatedOid + FixedOutputReset,
{
    let Ok(pk) = RsaPublicKey::from_public_key_der(spki) else {
        return err_bool(INVALID_SPKI);
    };
    let Ok(s) = pss::Signature::try_from(sig) else {
        return err_bool(SIG_LEN_MISMATCH);
    };
    let vk = pss::VerifyingKey::<D>::new(pk);
    ok_bool(vk.verify(msg, &s).is_ok())
}

/// PSS sign over digest `D` (randomized salt).
fn pss_sign<D>(pkcs8: &[u8], msg: &[u8], out: &mut [u8]) -> ResultUnit
where
    D: Digest + AssociatedOid + FixedOutputReset,
{
    let Ok(sk) = RsaPrivateKey::from_pkcs8_der(pkcs8) else {
        return err_unit(INVALID_PKCS8);
    };
    let signer = pss::SigningKey::<D>::new(sk);
    let sig = signer.sign_with_rng(&mut OsRng, msg);
    let bytes = sig.to_bytes();
    if bytes.len() != out.len() {
        return err_unit(SIG_LEN_MISMATCH);
    }
    out.copy_from_slice(&bytes);
    ok_unit()
}

/// PKCS#1 v1.5 verify over digest `D`.
fn pkcs1_verify<D>(spki: &[u8], msg: &[u8], sig: &[u8]) -> ResultBool
where
    D: Digest + AssociatedOid,
{
    let Ok(pk) = RsaPublicKey::from_public_key_der(spki) else {
        return err_bool(INVALID_SPKI);
    };
    let Ok(s) = pkcs1v15::Signature::try_from(sig) else {
        return err_bool(SIG_LEN_MISMATCH);
    };
    let vk = pkcs1v15::VerifyingKey::<D>::new(pk);
    ok_bool(vk.verify(msg, &s).is_ok())
}

/// PKCS#1 v1.5 sign over digest `D` (deterministic).
fn pkcs1_sign<D>(pkcs8: &[u8], msg: &[u8], out: &mut [u8]) -> ResultUnit
where
    D: Digest + AssociatedOid,
{
    let Ok(sk) = RsaPrivateKey::from_pkcs8_der(pkcs8) else {
        return err_unit(INVALID_PKCS8);
    };
    let signer = pkcs1v15::SigningKey::<D>::new(sk);
    let sig = signer.sign(msg);
    let bytes = sig.to_bytes();
    if bytes.len() != out.len() {
        return err_unit(SIG_LEN_MISMATCH);
    }
    out.copy_from_slice(&bytes);
    ok_unit()
}

/// RSA-PSS verify (SHA-256).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_verify_pss_sha256(
    public_key_spki_der: EdSlice,
    message: EdSlice,
    signature: EdSlice,
) -> ResultBool {
    unsafe { pss_verify::<Sha256>(in_bytes(public_key_spki_der), in_bytes(message), in_bytes(signature)) }
}

/// RSA-PSS verify (SHA-384).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_verify_pss_sha384(
    public_key_spki_der: EdSlice,
    message: EdSlice,
    signature: EdSlice,
) -> ResultBool {
    unsafe { pss_verify::<Sha384>(in_bytes(public_key_spki_der), in_bytes(message), in_bytes(signature)) }
}

/// RSA-PSS verify (SHA-512).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_verify_pss_sha512(
    public_key_spki_der: EdSlice,
    message: EdSlice,
    signature: EdSlice,
) -> ResultBool {
    unsafe { pss_verify::<Sha512>(in_bytes(public_key_spki_der), in_bytes(message), in_bytes(signature)) }
}

/// RSA-PSS sign (SHA-256).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_sign_pss_sha256(
    _random: *const (),
    private_key_pkcs8_der: EdSlice,
    message: EdSlice,
    sig_out: *mut EdSlice,
) -> ResultUnit {
    unsafe {
        pss_sign::<Sha256>(in_bytes(private_key_pkcs8_der), in_bytes(message), super::super::out_bytes(sig_out))
    }
}

/// RSA-PSS sign (SHA-384).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_sign_pss_sha384(
    _random: *const (),
    private_key_pkcs8_der: EdSlice,
    message: EdSlice,
    sig_out: *mut EdSlice,
) -> ResultUnit {
    unsafe {
        pss_sign::<Sha384>(in_bytes(private_key_pkcs8_der), in_bytes(message), super::super::out_bytes(sig_out))
    }
}

/// RSA-PSS sign (SHA-512).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_sign_pss_sha512(
    _random: *const (),
    private_key_pkcs8_der: EdSlice,
    message: EdSlice,
    sig_out: *mut EdSlice,
) -> ResultUnit {
    unsafe {
        pss_sign::<Sha512>(in_bytes(private_key_pkcs8_der), in_bytes(message), super::super::out_bytes(sig_out))
    }
}

/// RSA-PKCS#1-v1.5 verify (SHA-256).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_verify_pkcs1_v15_sha256(
    public_key_spki_der: EdSlice,
    message: EdSlice,
    signature: EdSlice,
) -> ResultBool {
    unsafe { pkcs1_verify::<Sha256>(in_bytes(public_key_spki_der), in_bytes(message), in_bytes(signature)) }
}

/// RSA-PKCS#1-v1.5 verify (SHA-384).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_verify_pkcs1_v15_sha384(
    public_key_spki_der: EdSlice,
    message: EdSlice,
    signature: EdSlice,
) -> ResultBool {
    unsafe { pkcs1_verify::<Sha384>(in_bytes(public_key_spki_der), in_bytes(message), in_bytes(signature)) }
}

/// RSA-PKCS#1-v1.5 verify (SHA-512).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_verify_pkcs1_v15_sha512(
    public_key_spki_der: EdSlice,
    message: EdSlice,
    signature: EdSlice,
) -> ResultBool {
    unsafe { pkcs1_verify::<Sha512>(in_bytes(public_key_spki_der), in_bytes(message), in_bytes(signature)) }
}

/// RSA-PKCS#1-v1.5 sign (SHA-256).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_sign_pkcs1_v15_sha256(
    private_key_pkcs8_der: EdSlice,
    message: EdSlice,
    sig_out: *mut EdSlice,
) -> ResultUnit {
    unsafe {
        pkcs1_sign::<Sha256>(in_bytes(private_key_pkcs8_der), in_bytes(message), super::super::out_bytes(sig_out))
    }
}

/// RSA-PKCS#1-v1.5 sign (SHA-384).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_sign_pkcs1_v15_sha384(
    private_key_pkcs8_der: EdSlice,
    message: EdSlice,
    sig_out: *mut EdSlice,
) -> ResultUnit {
    unsafe {
        pkcs1_sign::<Sha384>(in_bytes(private_key_pkcs8_der), in_bytes(message), super::super::out_bytes(sig_out))
    }
}

/// RSA-PKCS#1-v1.5 sign (SHA-512).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_sign_pkcs1_v15_sha512(
    private_key_pkcs8_der: EdSlice,
    message: EdSlice,
    sig_out: *mut EdSlice,
) -> ResultUnit {
    unsafe {
        pkcs1_sign::<Sha512>(in_bytes(private_key_pkcs8_der), in_bytes(message), super::super::out_bytes(sig_out))
    }
}

/// Extract (n, e) from an RSA SPKI DER public key.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_parse_spki(
    spki_der: EdSlice,
    n_out: *mut EdSlice,
    n_len_out: *mut u64,
    e_out: *mut EdSlice,
    e_len_out: *mut u64,
) -> ResultUnit {
    // SAFETY: `spki_der` satisfies the crypto-slice-borrow contract.
    let spki = unsafe { in_bytes(spki_der) };
    let Ok(pk) = RsaPublicKey::from_public_key_der(spki) else {
        return err_unit(INVALID_SPKI);
    };
    let n = pk.n().to_bytes_be();
    let e = pk.e().to_bytes_be();
    // SAFETY: out/len slots satisfy the crypto-slice-borrow-mut contract.
    if unsafe { !write_out(n_out, n_len_out, &n) || !write_out(e_out, e_len_out, &e) } {
        return err_unit(INTERNAL);
    }
    ok_unit()
}

/// Encode (n, e) as an RSA SPKI DER public key.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_encode_spki(
    n_be: EdSlice,
    e_be: EdSlice,
    spki_out: *mut EdSlice,
    spki_len_out: *mut u64,
) -> ResultUnit {
    // SAFETY: slices satisfy the crypto-slice-borrow contracts.
    let (n_b, e_b) = unsafe { (in_bytes(n_be), in_bytes(e_be)) };
    let Ok(pk) = RsaPublicKey::new(BigUint::from_bytes_be(n_b), BigUint::from_bytes_be(e_b)) else {
        return err_unit(UNSUPPORTED_MODULUS);
    };
    let Ok(doc) = pk.to_public_key_der() else {
        return err_unit(INTERNAL);
    };
    // SAFETY: out/len slots satisfy the crypto-slice-borrow-mut contract.
    if unsafe { !write_out(spki_out, spki_len_out, doc.as_bytes()) } {
        return err_unit(INTERNAL);
    }
    ok_unit()
}

/// Extract the public (n, e) from an RSA PKCS#8 DER private key.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_rsa_pkcs8_public_components(
    private_key_pkcs8_der: EdSlice,
    n_out: *mut EdSlice,
    n_len_out: *mut u64,
    e_out: *mut EdSlice,
    e_len_out: *mut u64,
) -> ResultUnit {
    // SAFETY: `private_key_pkcs8_der` satisfies the crypto-slice-borrow contract.
    let pkcs8 = unsafe { in_bytes(private_key_pkcs8_der) };
    let Ok(sk) = RsaPrivateKey::from_pkcs8_der(pkcs8) else {
        return err_unit(INVALID_PKCS8);
    };
    let pk = sk.to_public_key();
    let n = pk.n().to_bytes_be();
    let e = pk.e().to_bytes_be();
    // SAFETY: out/len slots satisfy the crypto-slice-borrow-mut contract.
    if unsafe { !write_out(n_out, n_len_out, &n) || !write_out(e_out, e_len_out, &e) } {
        return err_unit(INTERNAL);
    }
    ok_unit()
}
