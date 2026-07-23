//! ECDSA over NIST P-256 (SHA-256, RFC 6979 deterministic).

use super::super::{in_bytes, out_bytes};
use super::{
    ERR_DER_BUF, ERR_INTERNAL, ERR_NOT_ON_CURVE, ERR_PRIV, ERR_PUB, ERR_SIG, ResultBool,
    ResultUnit, err_bool, err_unit, ok_bool, ok_unit,
};
use crate::EdSlice;
use p256::PublicKey;
use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{Signature, SigningKey, VerifyingKey};
use p256::elliptic_curve::sec1::ToEncodedPoint;

const RAW_SIG_LEN: usize = 64;

/// P-256 ECDSA sign, DER output.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ecdsa_p256_sign_der(
    private_scalar: EdSlice,
    message: EdSlice,
    sig_der_out: *mut EdSlice,
    sig_der_len: *mut u64,
) -> ResultUnit {
    // SAFETY: slices satisfy the crypto-slice-borrow contracts.
    let (scalar, msg, out) =
        unsafe { (in_bytes(private_scalar), in_bytes(message), out_bytes(sig_der_out)) };
    let Ok(sk) = SigningKey::from_slice(scalar) else {
        return err_unit(ERR_PRIV);
    };
    let sig: Signature = sk.sign(msg);
    let der = sig.to_der();
    let der_bytes = der.as_bytes();
    if der_bytes.len() > out.len() {
        return err_unit(ERR_DER_BUF);
    }
    out[..der_bytes.len()].copy_from_slice(der_bytes);
    if !sig_der_len.is_null() {
        // SAFETY: `sig_der_len` is the caller's `mutable usize` slot.
        unsafe { *sig_der_len = der_bytes.len() as u64 };
    }
    ok_unit()
}

/// P-256 ECDSA sign, raw r||s output.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ecdsa_p256_sign_raw(
    private_scalar: EdSlice,
    message: EdSlice,
    sig_raw_out: *mut EdSlice,
) -> ResultUnit {
    // SAFETY: slices satisfy the crypto-slice-borrow contracts.
    let (scalar, msg, out) =
        unsafe { (in_bytes(private_scalar), in_bytes(message), out_bytes(sig_raw_out)) };
    let Ok(sk) = SigningKey::from_slice(scalar) else {
        return err_unit(ERR_PRIV);
    };
    if out.len() != RAW_SIG_LEN {
        return err_unit(ERR_INTERNAL);
    }
    let sig: Signature = sk.sign(msg);
    out.copy_from_slice(sig.to_bytes().as_slice());
    ok_unit()
}

/// P-256 ECDSA verify, DER signature.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ecdsa_p256_verify_der(
    public_key: EdSlice,
    message: EdSlice,
    signature: EdSlice,
) -> ResultBool {
    // SAFETY: slices satisfy the crypto-slice-borrow contracts.
    let (pk, msg, sig_b) = unsafe { (in_bytes(public_key), in_bytes(message), in_bytes(signature)) };
    let Ok(vk) = VerifyingKey::from_sec1_bytes(pk) else {
        return err_bool(ERR_PUB);
    };
    let Ok(sig) = Signature::from_der(sig_b) else {
        return err_bool(ERR_SIG);
    };
    ok_bool(vk.verify(msg, &sig).is_ok())
}

/// P-256 ECDSA verify, raw r||s signature.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ecdsa_p256_verify_raw(
    public_key: EdSlice,
    message: EdSlice,
    signature: EdSlice,
) -> ResultBool {
    // SAFETY: slices satisfy the crypto-slice-borrow contracts.
    let (pk, msg, sig_b) = unsafe { (in_bytes(public_key), in_bytes(message), in_bytes(signature)) };
    let Ok(vk) = VerifyingKey::from_sec1_bytes(pk) else {
        return err_bool(ERR_PUB);
    };
    let Ok(sig) = Signature::from_slice(sig_b) else {
        return err_bool(ERR_SIG);
    };
    ok_bool(vk.verify(msg, &sig).is_ok())
}

/// P-256 point compression (65 → 33 bytes).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ecdsa_p256_point_compress(
    uncompressed: EdSlice,
    compressed_out: *mut EdSlice,
) -> ResultUnit {
    // SAFETY: slices satisfy the crypto-slice-borrow contracts.
    let (unc, out) = unsafe { (in_bytes(uncompressed), out_bytes(compressed_out)) };
    let Ok(pk) = PublicKey::from_sec1_bytes(unc) else {
        return err_unit(ERR_NOT_ON_CURVE);
    };
    let pt = pk.to_encoded_point(true);
    if out.len() != pt.as_bytes().len() {
        return err_unit(ERR_INTERNAL);
    }
    out.copy_from_slice(pt.as_bytes());
    ok_unit()
}

/// P-256 point decompression (33 → 65 bytes).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn __edda_ecdsa_p256_point_decompress(
    compressed: EdSlice,
    uncompressed_out: *mut EdSlice,
) -> ResultUnit {
    // SAFETY: slices satisfy the crypto-slice-borrow contracts.
    let (comp, out) = unsafe { (in_bytes(compressed), out_bytes(uncompressed_out)) };
    let Ok(pk) = PublicKey::from_sec1_bytes(comp) else {
        return err_unit(ERR_NOT_ON_CURVE);
    };
    let pt = pk.to_encoded_point(false);
    if out.len() != pt.as_bytes().len() {
        return err_unit(ERR_INTERNAL);
    }
    out.copy_from_slice(pt.as_bytes());
    ok_unit()
}
